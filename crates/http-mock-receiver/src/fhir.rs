//! A mock of Promptly's asynchronous `$process-message` API.
//!
//! Implements `spec/11-mock-receivers.md` §3.10. Upstream:
//! `http_mock_receiver/fhir_handler.py` (`#442`).
//!
//! A pure state machine: [`FhirMock::submit`] and [`FhirMock::status`] take
//! text and return a [`MockResponse`], so every scenario is tested without a
//! socket. Behaviour is chosen by `Bundle.id`:
//!
//! | `Bundle.id` | Behaviour |
//! | --- | --- |
//! | anything else | `202`, two `202` polls, then `200` success |
//! | `mock-slow` | six `202` polls before success |
//! | `mock-error-outcome` | `200` with an `error` `OperationOutcome` |
//! | `mock-status-400` | the status poll returns `400` |
//! | `mock-400` `-401` `-403` `-404` `-409` `-429` `-500` | the `POST` is rejected with that status (`429` adds `Retry-After`) |
//! | `mock-429-once` `mock-500-once` | the first `POST` for that id fails, the retry is accepted |
//!
//! State is in memory and lost on restart. A finished job is kept for five
//! minutes so a sender repeating a `GET` after a dropped connection sees the
//! same outcome.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};

/// The path a status URL lives under.
pub const STATUS_PATH: &str = "/fhir/r4/status";

/// The path of the operation.
pub const PROCESS_MESSAGE_PATH: &str = "/fhir/r4/$process-message";

/// Whether `path` names the operation, with `$` written either way.
///
/// A client may send `$` as `%24`, and the local env files must: Compose
/// reads a bare `$process` as a variable (`local/fhir-sender.env`). Upstream's
/// `FastAPI` decodes the path before routing, so the same URL works there; found
/// by the `wpas-to-proms` end-to-end run, where the sender's request fell
/// through to the SOAP endpoint.
#[must_use]
pub fn is_process_message_path(path: &str) -> bool {
    path == PROCESS_MESSAGE_PATH || path.eq_ignore_ascii_case("/fhir/r4/%24process-message")
}

/// Prefix that selects a scenario.
pub const SCENARIO_PREFIX: &str = "mock-";

/// `202` polls before success, by default.
pub const DEFAULT_POLLS: u32 = 2;

/// `202` polls before success for `mock-slow`.
pub const SLOW_POLLS: u32 = 6;

/// Sent with every `429` and every `202` poll.
pub const RETRY_AFTER_SECONDS: &str = "1";

/// How long a finished job is kept.
pub const TERMINAL_RETENTION_SECONDS: f64 = 300.0;

/// What the mock answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockResponse {
    /// The status code.
    pub status: u16,
    /// A JSON body, when there is one.
    pub body: Option<Value>,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
}

impl MockResponse {
    /// A response with no extra headers.
    const fn new(status: u16, body: Option<Value>) -> Self {
        Self {
            status,
            body,
            headers: vec![],
        }
    }

    /// This response with one more header.
    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// An `OperationOutcome` with one issue.
#[must_use]
pub fn operation_outcome(severity: &str, code: &str, text: &str) -> Value {
    json!({
        "resourceType": "OperationOutcome",
        "issue": [{"severity": severity, "code": code, "details": {"text": text}}],
    })
}

/// One accepted Bundle being "processed".
#[derive(Debug)]
struct Job {
    /// `202` polls still to answer.
    polls_remaining: u32,
    /// What the poll says when they run out.
    final_response: MockResponse,
    /// When the final answer was first given, on the clock's scale.
    completed_at: Option<f64>,
}

/// The mutable part.
#[derive(Debug, Default)]
struct State {
    /// Job id to job.
    jobs: HashMap<String, Job>,
    /// Ids whose one-off failure has been used.
    failed_once: HashSet<String>,
}

/// The mock. Shared by every connection.
#[derive(Debug)]
pub struct FhirMock {
    /// All state, behind one lock: every operation is brief.
    state: Mutex<State>,
    /// Zero of the retention clock.
    start: Instant,
}

impl Default for FhirMock {
    fn default() -> Self {
        Self::new()
    }
}

impl FhirMock {
    /// An empty mock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            start: Instant::now(),
        }
    }

    /// Accept or reject a `POST` body. `base_url` is the origin the status
    /// URL is built on, without a trailing slash.
    #[must_use]
    pub fn submit(&self, raw_body: &str, base_url: &str) -> MockResponse {
        self.submit_at(raw_body, base_url, self.start.elapsed().as_secs_f64())
    }

    /// Answer a status poll.
    #[must_use]
    pub fn status(&self, job_id: &str) -> MockResponse {
        self.status_at(job_id, self.start.elapsed().as_secs_f64())
    }

    /// [`Self::submit`] at an explicit time, for tests.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock is the whole operation; releasing it earlier would split one atomic step"
    )]
    fn submit_at(&self, raw_body: &str, base_url: &str, now: f64) -> MockResponse {
        let mut state = self.lock();
        purge_expired(&mut state, now);

        let Ok(bundle) = serde_json::from_str::<Value>(raw_body) else {
            return MockResponse::new(
                400,
                Some(operation_outcome(
                    "error",
                    "invalid",
                    "Body is not valid JSON",
                )),
            );
        };
        let is_message_bundle = bundle.get("resourceType").and_then(Value::as_str)
            == Some("Bundle")
            && bundle.get("type").and_then(Value::as_str) == Some("message");
        if !is_message_bundle {
            return MockResponse::new(
                400,
                Some(operation_outcome(
                    "error",
                    "invalid",
                    "Expected a Bundle of type 'message'",
                )),
            );
        }

        let bundle_id = bundle
            .get("id")
            .map_or_default(|id| id.as_str().map_or_else(|| id.to_string(), str::to_string));
        let scenario = bundle_id.strip_prefix(SCENARIO_PREFIX).unwrap_or("");

        if let Some(failure) = submit_failure(&mut state, &bundle_id, scenario) {
            return failure;
        }

        let job_id = uuid::Uuid::new_v4().simple().to_string();
        state.jobs.insert(
            job_id.clone(),
            Job {
                polls_remaining: if scenario == "slow" {
                    SLOW_POLLS
                } else {
                    DEFAULT_POLLS
                },
                final_response: final_response(scenario),
                completed_at: None,
            },
        );
        MockResponse::new(202, None).with_header(
            "Content-Location",
            &format!("{}{STATUS_PATH}/{job_id}", base_url.trim_end_matches('/')),
        )
    }

    /// [`Self::status`] at an explicit time, for tests.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock is the whole operation; releasing it earlier would split one atomic step"
    )]
    fn status_at(&self, job_id: &str, now: f64) -> MockResponse {
        let mut state = self.lock();
        purge_expired(&mut state, now);

        let Some(job) = state.jobs.get_mut(job_id) else {
            return MockResponse::new(
                404,
                Some(operation_outcome("error", "not-found", "Unknown status id")),
            );
        };
        if let Some(remaining) = job.polls_remaining.checked_sub(1) {
            job.polls_remaining = remaining;
            return MockResponse::new(202, None)
                .with_header("X-Progress", "Processing")
                .with_header("Retry-After", RETRY_AFTER_SECONDS);
        }
        // Kept, so a repeated GET sees the same outcome.
        job.completed_at.get_or_insert(now);
        job.final_response.clone()
    }

    /// Take the lock, ignoring poison (see the note inside).
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A panic while holding the lock cannot leave the maps inconsistent:
        // every mutation is a single insert, remove or counter step.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Drop jobs that finished more than the retention ago.
fn purge_expired(state: &mut State, now: f64) {
    state.jobs.retain(|_, job| {
        job.completed_at
            .is_none_or(|done| now - done <= TERMINAL_RETENTION_SECONDS)
    });
}

/// A `POST` the scenario rejects, if it does.
fn submit_failure(state: &mut State, bundle_id: &str, scenario: &str) -> Option<MockResponse> {
    let immediate = matches!(
        scenario,
        "400" | "401" | "403" | "404" | "409" | "429" | "500"
    );
    let once = match scenario {
        "429-once" => Some(429),
        "500-once" => Some(500),
        _ => None,
    };

    let (status, text) = if immediate {
        let status: u16 = scenario.parse().ok()?;
        (status, format!("Mock {status} response"))
    } else if let Some(status) = once
        && state.failed_once.insert(bundle_id.to_string())
    {
        (status, format!("Mock {status} (first attempt)"))
    } else {
        return None;
    };

    let mut response = MockResponse::new(
        status,
        Some(operation_outcome("error", "processing", &text)),
    );
    if status == 429 {
        response = response.with_header("Retry-After", RETRY_AFTER_SECONDS);
    }
    Some(response)
}

/// What a finished job answers.
fn final_response(scenario: &str) -> MockResponse {
    match scenario {
        "status-400" => MockResponse::new(
            400,
            Some(operation_outcome(
                "error",
                "invalid",
                "Invalid message format",
            )),
        ),
        "error-outcome" => MockResponse::new(
            200,
            Some(operation_outcome(
                "error",
                "business-rule",
                "Patient could not be matched",
            )),
        ),
        _ => MockResponse::new(
            200,
            Some(operation_outcome(
                "information",
                "informational",
                "Message processed successfully",
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "http://mock:8080/";

    fn bundle(id: &str) -> String {
        format!(r#"{{"resourceType":"Bundle","type":"message","id":"{id}"}}"#)
    }

    fn job_id(response: &MockResponse) -> String {
        let (_, location) = response
            .headers
            .iter()
            .find(|(name, _)| name == "Content-Location")
            .expect("a Content-Location");
        location
            .strip_prefix("http://mock:8080/fhir/r4/status/")
            .expect("the status URL is under the base")
            .to_string()
    }

    fn severity(response: &MockResponse) -> &str {
        response.body.as_ref().unwrap()["issue"][0]["severity"]
            .as_str()
            .unwrap()
    }

    /// Poll to the end, counting the `202`s.
    fn drain(mock: &FhirMock, id: &str) -> (usize, MockResponse) {
        let mut polls = 0;
        loop {
            let response = mock.status(id);
            if response.status != 202 {
                return (polls, response);
            }
            polls += 1;
        }
    }

    #[test]
    fn an_ordinary_bundle_takes_two_polls_then_succeeds() {
        // spec/11-mock-receivers.md §3.10.
        let mock = FhirMock::new();
        let accepted = mock.submit(&bundle("b-1"), BASE);
        assert_eq!(accepted.status, 202);
        assert!(accepted.body.is_none());

        let (polls, done) = drain(&mock, &job_id(&accepted));
        assert_eq!(polls, 2);
        assert_eq!(done.status, 200);
        assert_eq!(severity(&done), "information");
    }

    #[test]
    fn a_poll_says_processing_and_when_to_come_back() {
        let mock = FhirMock::new();
        let id = job_id(&mock.submit(&bundle("b"), BASE));
        let poll = mock.status(&id);
        assert!(
            poll.headers
                .contains(&("X-Progress".into(), "Processing".into()))
        );
        assert!(poll.headers.contains(&("Retry-After".into(), "1".into())));
    }

    #[test]
    fn mock_slow_takes_six_polls() {
        let mock = FhirMock::new();
        let id = job_id(&mock.submit(&bundle("mock-slow"), BASE));
        assert_eq!(drain(&mock, &id).0, 6);
    }

    #[test]
    fn the_final_scenarios() {
        let mock = FhirMock::new();
        let id = job_id(&mock.submit(&bundle("mock-error-outcome"), BASE));
        let (_, done) = drain(&mock, &id);
        assert_eq!((done.status, severity(&done)), (200, "error"));

        let id = job_id(&mock.submit(&bundle("mock-status-400"), BASE));
        assert_eq!(drain(&mock, &id).1.status, 400);
    }

    #[test]
    fn the_immediate_failures_reject_the_post() {
        let mock = FhirMock::new();
        for status in [400_u16, 401, 403, 404, 409, 429, 500] {
            let response = mock.submit(&bundle(&format!("mock-{status}")), BASE);
            assert_eq!(response.status, status);
            assert_eq!(severity(&response), "error");
            assert_eq!(
                response
                    .headers
                    .iter()
                    .any(|(name, _)| name == "Retry-After"),
                status == 429,
                "{status}"
            );
        }
    }

    #[test]
    fn a_once_scenario_fails_the_first_post_only_and_per_id() {
        let mock = FhirMock::new();
        for (id, status) in [("mock-429-once", 429), ("mock-500-once", 500)] {
            assert_eq!(mock.submit(&bundle(id), BASE).status, status);
            assert_eq!(mock.submit(&bundle(id), BASE).status, 202, "the retry");
        }
    }

    #[test]
    fn a_body_that_is_not_a_message_bundle_is_a_400() {
        let mock = FhirMock::new();
        for body in [
            "not json",
            r#"{"resourceType":"Patient"}"#,
            r#"{"resourceType":"Bundle","type":"batch"}"#,
        ] {
            assert_eq!(mock.submit(body, BASE).status, 400, "{body}");
        }
    }

    #[test]
    fn the_operation_path_matches_with_the_dollar_literal_or_encoded() {
        assert!(is_process_message_path("/fhir/r4/$process-message"));
        assert!(is_process_message_path("/fhir/r4/%24process-message"));
        assert!(is_process_message_path(
            "/fhir/r4/%24Process-Message".to_lowercase().as_str()
        ));
        assert!(!is_process_message_path("/fhir/r4/process-message"));
        assert!(!is_process_message_path("/soap"));
    }

    #[test]
    fn an_unknown_status_id_is_a_404() {
        assert_eq!(FhirMock::new().status("nope").status, 404);
    }

    #[test]
    fn a_finished_job_repeats_its_answer_until_it_expires() {
        let mock = FhirMock::new();
        let accepted = mock.submit_at(&bundle("b"), BASE, 0.0);
        let id = job_id(&accepted);
        for _ in 0..2 {
            assert_eq!(mock.status_at(&id, 1.0).status, 202);
        }
        assert_eq!(mock.status_at(&id, 2.0).status, 200);
        assert_eq!(
            mock.status_at(&id, 100.0).status,
            200,
            "the same outcome again"
        );
        assert_eq!(
            mock.status_at(&id, 303.0).status,
            404,
            "over five minutes after it finished"
        );
    }

    #[test]
    fn the_status_url_is_built_on_the_given_origin() {
        let accepted = FhirMock::new().submit(&bundle("b"), "http://promptly-mock-receiver:8080");
        assert!(
            accepted.headers[0]
                .1
                .starts_with("http://promptly-mock-receiver:8080/fhir/r4/status/")
        );
    }
}
