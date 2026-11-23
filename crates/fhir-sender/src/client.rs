//! The `$process-message` client: the `POST`, the poll and the retry policy.
//!
//! Implements `spec/10-senders.md` §8.2 and §8.4. Upstream:
//! `fhir_sender/fhir_sender_client.py`.
//!
//! The IG's asynchronous pattern: `POST` the message Bundle; `202 Accepted`
//! carries a `Content-Location` status URL; poll that URL until it returns
//! `200` and an `OperationOutcome`.
//!
//! Retry policy, for the `POST` and for every poll:
//!
//! | Response | Behaviour |
//! | --- | --- |
//! | `429` | wait `Retry-After` (capped), else the backoff, then retry |
//! | `5xx`, network error, timeout | exponential backoff, up to `max_attempts` |
//! | `401` | one retry with a fresh token if the credentials can refresh, else [`DeliveryError::Authorization`] |
//! | `403` | [`DeliveryError::Authorization`], never retried |
//! | any other `4xx` | [`DeliveryError::Permanent`], never retried |

use std::future::Future;
use std::time::{Duration, Instant};

use crate::auth::Auth;
use crate::error::DeliveryError;
use crate::http::{Method, Request, Response, Transport};

/// The media type of every request and the one asked for in reply.
pub const FHIR_JSON: &str = "application/fhir+json";

/// The longest detail carried into a log line or a dead letter.
pub const MAX_DETAIL_LENGTH: usize = 500;

/// A monotonic clock that can wait, so the policy is tested without waiting.
pub trait Clock: Send + Sync {
    /// Seconds on a monotonic scale.
    fn now(&self) -> f64;

    /// Wait.
    fn sleep(&mut self, seconds: f64) -> impl Future<Output = ()> + Send;
}

/// The real clock.
#[derive(Debug, Clone)]
pub struct TokioClock {
    /// The zero of this clock's scale.
    start: Instant,
}

impl TokioClock {
    /// A clock that starts now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for TokioClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TokioClock {
    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    async fn sleep(&mut self, seconds: f64) {
        tokio::time::sleep(Duration::from_secs_f64(seconds.max(0.0))).await;
    }
}

/// The tunables (`spec/10-senders.md` §8.7).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Per-request timeout, seconds.
    pub timeout_seconds: f64,
    /// Attempts per request for `429`, `5xx` and network errors.
    pub max_attempts: u32,
    /// The first backoff; it doubles each attempt.
    pub backoff_seconds: f64,
    /// The cap on a `Retry-After` wait.
    pub max_retry_after_seconds: f64,
    /// The poll delay when the response gives none.
    pub poll_interval_seconds: f64,
    /// The longest to wait for processing to finish.
    pub poll_timeout_seconds: f64,
    /// Whether plain `http` is allowed (local mocks only).
    pub allow_insecure_http: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout_seconds: 30.0,
            max_attempts: 3,
            backoff_seconds: 2.0,
            max_retry_after_seconds: 60.0,
            poll_interval_seconds: 5.0,
            poll_timeout_seconds: 120.0,
            allow_insecure_http: false,
        }
    }
}

/// A client for one `$process-message` endpoint.
#[derive(Debug)]
pub struct FhirClient<T, C> {
    /// The `$process-message` URL.
    endpoint: String,
    /// The credentials.
    auth: Auth,
    /// The tunables.
    settings: Settings,
    /// How requests are sent.
    transport: T,
    /// How time passes.
    clock: C,
}

impl<T: Transport, C: Clock> FhirClient<T, C> {
    /// A client for `endpoint`.
    ///
    /// # Errors
    ///
    /// A message when `max_attempts` is zero or the endpoint is not `https`
    /// (or `http` with `allow_insecure_http`).
    pub fn new(
        endpoint: &str,
        auth: Auth,
        settings: Settings,
        transport: T,
        clock: C,
    ) -> Result<Self, String> {
        if settings.max_attempts < 1 {
            return Err("max_attempts must be at least 1".to_string());
        }
        let lower = endpoint.to_ascii_lowercase();
        if !(lower.starts_with("https://")
            || (settings.allow_insecure_http && lower.starts_with("http://")))
        {
            return Err("FHIR endpoint must use HTTPS".to_string());
        }
        Ok(Self {
            endpoint: endpoint.to_string(),
            auth,
            settings,
            transport,
            clock,
        })
    }

    /// Submit a message Bundle and wait for Promptly to finish processing it.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::Permanent`]: rejected, do not retry.
    /// [`DeliveryError::Authorization`]: the credentials were rejected.
    /// [`DeliveryError::Transient`]: delivery could not be confirmed.
    pub async fn send_bundle(&mut self, bundle_json: &str) -> Result<(), DeliveryError> {
        let endpoint = self.endpoint.clone();
        let response = self
            .request(Method::Post, &endpoint, Some(bundle_json.as_bytes()), None)
            .await?;

        match response.status {
            202 => {
                let status_url = self.resolve_status_url(&response)?;
                self.poll(&status_url).await
            }
            200 => check_outcome(&response),
            _ => Err(error_for_status(&response)),
        }
    }

    /// The status URL from a `202`, refused unless it is the endpoint's origin.
    fn resolve_status_url(&self, response: &Response) -> Result<String, DeliveryError> {
        let location = response
            .header("content-location")
            .filter(|location| !location.is_empty())
            .ok_or_else(|| {
                DeliveryError::Transient(
                    "202 Accepted without a Content-Location status URL".to_string(),
                )
            })?;
        let status_url = join_url(&self.endpoint, location);
        // Credentials are sent to the status URL, so it must be the
        // configured endpoint's origin (`spec/10-senders.md` §8.4).
        if origin(&status_url) != origin(&self.endpoint) {
            return Err(DeliveryError::Permanent {
                status: 202,
                detail: "Status URL origin differs from the configured endpoint".to_string(),
            });
        }
        Ok(status_url)
    }

    /// Poll the status URL until `200`, an error, or the deadline.
    async fn poll(&mut self, status_url: &str) -> Result<(), DeliveryError> {
        let deadline = self.clock.now() + self.settings.poll_timeout_seconds;
        loop {
            let response = self
                .request(Method::Get, status_url, None, Some(deadline))
                .await?;
            if response.status == 200 {
                return check_outcome(&response);
            }
            if response.status != 202 {
                return Err(error_for_status(&response));
            }

            let delay = retry_after_seconds(&response)
                .unwrap_or(self.settings.poll_interval_seconds)
                .min(self.settings.max_retry_after_seconds);
            if self.clock.now() + delay > deadline {
                return Err(DeliveryError::Transient(format!(
                    "Processing not complete after {:.0}s (X-Progress: {})",
                    self.settings.poll_timeout_seconds,
                    response.header("x-progress").unwrap_or("n/a")
                )));
            }
            tracing::debug!(
                delay_seconds = delay,
                "message still processing; polling again"
            );
            self.clock.sleep(delay).await;
        }
    }

    /// Send with retries. A deadline (monotonic seconds) bounds timeouts and
    /// waits so polling cannot overrun it.
    async fn request(
        &mut self,
        method: Method,
        url: &str,
        data: Option<&[u8]>,
        deadline: Option<f64>,
    ) -> Result<Response, DeliveryError> {
        let name = method.as_str();
        let shown = url.split('?').next().unwrap_or(url);
        let max = self.settings.max_attempts;
        let mut last_error = "no attempts made".to_string();
        let mut token_refreshed = false;
        let mut attempt = 1_u32;

        while attempt <= max {
            let mut timeout = self.settings.timeout_seconds;
            if let Some(deadline) = deadline {
                let remaining = deadline - self.clock.now();
                if remaining <= 0.0 {
                    return Err(DeliveryError::Transient(format!(
                        "Polling deadline reached before {name} could complete: {last_error}"
                    )));
                }
                timeout = timeout.min(remaining);
            }

            let mut headers = vec![("Accept".to_string(), FHIR_JSON.to_string())];
            headers.extend(self.auth.headers(&mut self.transport, &self.clock).await?);
            if data.is_some() {
                headers.push(("Content-Type".to_string(), FHIR_JSON.to_string()));
            }

            let sent = self
                .transport
                .send(Request {
                    method,
                    url: url.to_string(),
                    headers,
                    body: data.map(<[u8]>::to_vec),
                    timeout: Duration::from_secs_f64(timeout.max(0.001)),
                })
                .await;

            let response = match sent {
                Ok(response) => response,
                Err(error) => {
                    last_error = format!("{error}");
                    tracing::warn!(url = shown, attempt, max, error = %last_error, "{name} failed");
                    self.backoff(attempt, deadline).await?;
                    // Running out of attempt numbers is running out of attempts.
                    let Some(next) = attempt.checked_add(1) else {
                        break;
                    };
                    attempt = next;
                    continue;
                }
            };

            let status = response.status;
            if status == 401 && !token_refreshed && self.auth.invalidate() {
                token_refreshed = true;
                tracing::warn!("received 401; retrying once with fresh credentials");
                continue;
            }
            if status == 429 {
                last_error = "HTTP 429 Too Many Requests".to_string();
                let delay =
                    retry_after_seconds(&response).unwrap_or_else(|| self.backoff_delay(attempt));
                tracing::warn!(attempt, max, delay_seconds = delay, "rate limited");
                if attempt < max {
                    self.wait(delay.min(self.settings.max_retry_after_seconds), deadline)
                        .await?;
                }
                // Running out of attempt numbers is running out of attempts.
                let Some(next) = attempt.checked_add(1) else {
                    break;
                };
                attempt = next;
                continue;
            }
            if status >= 500 {
                last_error = format!("HTTP {status}: {}", summarise_outcome(&response));
                tracing::warn!(
                    url = shown,
                    status,
                    attempt,
                    max,
                    "{name} returned a server error"
                );
                self.backoff(attempt, deadline).await?;
                // Running out of attempt numbers is running out of attempts.
                let Some(next) = attempt.checked_add(1) else {
                    break;
                };
                attempt = next;
                continue;
            }
            return Ok(response);
        }

        Err(DeliveryError::Transient(format!(
            "{name} failed after {max} attempt(s): {last_error}"
        )))
    }

    /// The wait before attempt `attempt + 1`: doubles each time.
    fn backoff_delay(&self, attempt: u32) -> f64 {
        // 2^(attempt - 1) as 2^attempt / 2: exact for a power of two, and
        // with no `attempt - 1` to underflow (`attempt` starts at 1).
        self.settings.backoff_seconds * 2_f64.powi(i32::try_from(attempt).unwrap_or(i32::MAX)) / 2.0
    }

    /// Wait the backoff, unless this was the last attempt.
    async fn backoff(&mut self, attempt: u32, deadline: Option<f64>) -> Result<(), DeliveryError> {
        if attempt < self.settings.max_attempts {
            self.wait(self.backoff_delay(attempt), deadline).await?;
        }
        Ok(())
    }

    /// Wait, refusing to pass the polling deadline.
    async fn wait(&mut self, delay: f64, deadline: Option<f64>) -> Result<(), DeliveryError> {
        if let Some(deadline) = deadline
            && self.clock.now() + delay >= deadline
        {
            return Err(DeliveryError::Transient(
                "Polling deadline would be exceeded while waiting to retry".to_string(),
            ));
        }
        self.clock.sleep(delay).await;
        Ok(())
    }
}

/// `Retry-After` in seconds. The HTTP-date form is not used by Promptly per
/// the IG, so it is ignored.
fn retry_after_seconds(response: &Response) -> Option<f64> {
    let seconds: f64 = response.header("retry-after")?.trim().parse().ok()?;
    (seconds > 0.0).then_some(seconds)
}

/// The failure class for a status that is not a success.
fn error_for_status(response: &Response) -> DeliveryError {
    let detail = summarise_outcome(response);
    if matches!(response.status, 401 | 403) {
        DeliveryError::Authorization {
            status: response.status,
            detail,
        }
    } else {
        DeliveryError::Permanent {
            status: response.status,
            detail,
        }
    }
}

/// A `200` only means the status call succeeded; the `OperationOutcome` says
/// whether processing did.
fn check_outcome(response: &Response) -> Result<(), DeliveryError> {
    let body = response.json().ok_or_else(|| {
        DeliveryError::Transient(
            "200 response did not contain a valid OperationOutcome".to_string(),
        )
    })?;
    if body.get("resourceType").and_then(serde_json::Value::as_str) != Some("OperationOutcome") {
        return Err(DeliveryError::Transient(
            "200 response did not contain an OperationOutcome".to_string(),
        ));
    }
    let has_error = body
        .get("issue")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|issues| {
            issues.iter().any(|issue| {
                matches!(
                    issue.get("severity").and_then(serde_json::Value::as_str),
                    Some("error" | "fatal")
                )
            })
        });
    if has_error {
        return Err(DeliveryError::Permanent {
            status: response.status,
            detail: summarise_outcome(response),
        });
    }
    Ok(())
}

/// Condense an `OperationOutcome` (or a raw body) into a short string.
fn summarise_outcome(response: &Response) -> String {
    let Some(body) = response.json() else {
        let text = response.text();
        let text = if text.is_empty() {
            hyper::StatusCode::from_u16(response.status)
                .ok()
                .and_then(|status| status.canonical_reason())
                .unwrap_or("no body")
                .to_string()
        } else {
            text
        };
        return text.chars().take(MAX_DETAIL_LENGTH).collect();
    };

    let parts: Vec<String> = body
        .get("issue")
        .and_then(serde_json::Value::as_array)
        .map_or_default(|issues| {
            issues
                .iter()
                .filter(|issue| issue.is_object())
                .map(|issue| {
                    let text = issue
                        .pointer("/details/text")
                        .and_then(serde_json::Value::as_str)
                        .filter(|text| !text.is_empty())
                        .or_else(|| issue.get("diagnostics").and_then(serde_json::Value::as_str))
                        .unwrap_or("");
                    let field = |name: &str| {
                        issue
                            .get(name)
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("?")
                    };
                    format!("{}/{}: {text}", field("severity"), field("code"))
                        .trim()
                        .to_string()
                })
                .collect()
        });

    let summary = if parts.is_empty() {
        body.to_string()
    } else {
        parts.join("; ")
    };
    summary.chars().take(MAX_DETAIL_LENGTH).collect()
}

/// `scheme://authority`, lower-cased. Empty when the URL has neither.
fn origin(url: &str) -> String {
    url.split_once("://")
        .map_or_else(String::new, |(scheme, rest)| {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
            format!(
                "{}://{}",
                scheme.to_ascii_lowercase(),
                authority.to_ascii_lowercase()
            )
        })
}

/// Resolve `location` against `base`, as `urljoin` does for the shapes a
/// `Content-Location` takes: absolute, scheme-relative, absolute-path or
/// relative-path.
fn join_url(base: &str, location: &str) -> String {
    let lower = location.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return location.to_string();
    }
    let base_origin = origin(base);
    if let Some(rest) = location.strip_prefix("//") {
        let scheme = base_origin.split("://").next().unwrap_or("https");
        return format!("{scheme}://{rest}");
    }
    if location.starts_with('/') {
        return format!("{base_origin}{location}");
    }
    // What follows the authority (`origin` lowercases it, so it is not matched
    // as a prefix); with no scheme there is no origin and the whole is the path.
    let after_origin = match base.split_once("://") {
        Some((_, rest)) => rest
            .find(['/', '?', '#'])
            .and_then(|at| rest.get(at..))
            .unwrap_or(""),
        None => base,
    };
    let path = after_origin.split(['?', '#']).next().unwrap_or("");
    let directory = path
        .rsplit_once('/')
        .map_or_else(|| "/".to_string(), |(directory, _)| format!("{directory}/"));
    format!("{base_origin}{directory}{location}")
}

/// Scripted doubles for this crate's tests.
#[cfg(test)]
#[allow(missing_docs, reason = "test doubles")]
#[allow(
    clippy::unnecessary_wraps,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::unused_async_trait_impl,
    reason = "scripted test doubles: the trait's shape, and a panic is a failed test"
)]
pub mod test_support {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::{Clock, Request, Response, Transport};
    use crate::http::TransportError;

    /// Answers from a script and records what it was sent.
    pub struct FakeTransport {
        script: VecDeque<Result<Response, TransportError>>,
        pub sent: Vec<Request>,
    }

    impl FakeTransport {
        pub fn new(script: Vec<Result<Response, TransportError>>) -> Self {
            Self {
                script: script.into(),
                sent: vec![],
            }
        }
    }

    impl Transport for FakeTransport {
        async fn send(&mut self, request: Request) -> Result<Response, TransportError> {
            self.sent.push(request);
            self.script
                .pop_front()
                .expect("the test scripted fewer responses than requests")
        }
    }

    /// A clock where sleeping just moves time forward.
    #[derive(Clone, Default)]
    pub struct FakeClock {
        now: Arc<Mutex<f64>>,
        pub sleeps: Arc<Mutex<Vec<f64>>>,
    }

    impl FakeClock {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn advance(&self, seconds: f64) {
            *self.now.lock().unwrap() += seconds;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> f64 {
            *self.now.lock().unwrap()
        }

        async fn sleep(&mut self, seconds: f64) {
            self.sleeps.lock().unwrap().push(seconds);
            self.advance(seconds);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unnecessary_wraps,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::unused_async_trait_impl,
    reason = "scripted test doubles: the trait's shape, and a panic is a failed test"
)]
mod tests {
    use super::test_support::{FakeClock, FakeTransport};
    use super::*;
    use crate::http::TransportError;
    use ihb_config::Secret;

    const ENDPOINT: &str = "https://promptly.example/fhir/r4/$process-message";

    fn reply(
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<Response, TransportError> {
        Ok(Response {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        })
    }

    fn outcome(severity: &str) -> String {
        format!(
            r#"{{"resourceType":"OperationOutcome","issue":[{{"severity":"{severity}","code":"processing","details":{{"text":"detail"}}}}]}}"#
        )
    }

    fn client(
        script: Vec<Result<Response, TransportError>>,
        auth: Auth,
    ) -> (FhirClient<FakeTransport, FakeClock>, FakeClock) {
        let clock = FakeClock::new();
        let client = FhirClient::new(
            ENDPOINT,
            auth,
            Settings::default(),
            FakeTransport::new(script),
            clock.clone(),
        )
        .unwrap();
        (client, clock)
    }

    fn key() -> Auth {
        Auth::api_key(Secret::new("k")).unwrap()
    }

    #[tokio::test]
    async fn a_200_with_a_clean_outcome_completes() {
        // spec/10-senders.md §8.2.
        let (mut c, _) = client(vec![reply(200, &[], &outcome("information"))], key());
        c.send_bundle("{}").await.unwrap();

        let sent = &c.transport.sent[0];
        assert_eq!(sent.method, Method::Post);
        assert!(sent.headers.contains(&("X-API-Key".into(), "k".into())));
        assert!(
            sent.headers
                .contains(&("Content-Type".into(), FHIR_JSON.into()))
        );
        assert!(sent.headers.contains(&("Accept".into(), FHIR_JSON.into())));
    }

    #[tokio::test]
    async fn a_202_is_polled_until_a_200_honouring_retry_after() {
        let (mut c, clock) = client(
            vec![
                reply(202, &[("Content-Location", "/fhir/r4/status/9")], ""),
                reply(202, &[("Retry-After", "3")], ""),
                reply(202, &[], ""),
                reply(200, &[], &outcome("information")),
            ],
            key(),
        );
        c.send_bundle("{}").await.unwrap();

        assert_eq!(
            c.transport.sent[1].url,
            "https://promptly.example/fhir/r4/status/9"
        );
        assert_eq!(c.transport.sent[1].method, Method::Get);
        assert!(c.transport.sent[1].body.is_none());
        // Retry-After first, then the 5 s default interval.
        assert_eq!(*clock.sleeps.lock().unwrap(), [3.0, 5.0]);
    }

    #[tokio::test]
    async fn an_error_outcome_is_permanent_even_on_a_200() {
        for severity in ["error", "fatal"] {
            let (mut c, _) = client(vec![reply(200, &[], &outcome(severity))], key());
            let error = c.send_bundle("{}").await.unwrap_err();
            assert_eq!(
                error,
                DeliveryError::Permanent {
                    status: 200,
                    detail: format!("{severity}/processing: detail")
                }
            );
        }
        let (mut c, _) = client(vec![reply(200, &[], &outcome("warning"))], key());
        assert!(
            c.send_bundle("{}").await.is_ok(),
            "a warning is not a failure"
        );
    }

    #[tokio::test]
    async fn a_200_that_is_not_an_operation_outcome_is_transient() {
        for body in ["nope", r#"{"resourceType":"Bundle"}"#] {
            let (mut c, _) = client(vec![reply(200, &[], body)], key());
            assert!(matches!(
                c.send_bundle("{}").await.unwrap_err(),
                DeliveryError::Transient(_)
            ));
        }
    }

    #[tokio::test]
    async fn client_errors_are_permanent_and_never_retried() {
        for status in [400_u16, 404, 409, 422] {
            let (mut c, _) = client(vec![reply(status, &[], &outcome("error"))], key());
            let error = c.send_bundle("{}").await.unwrap_err();
            assert!(
                matches!(error, DeliveryError::Permanent { status: s, .. } if s == status),
                "{status}"
            );
            assert_eq!(c.transport.sent.len(), 1, "{status}");
        }
    }

    #[tokio::test]
    async fn a_403_and_an_unrefreshable_401_are_authorization_failures() {
        for status in [401_u16, 403] {
            let (mut c, _) = client(vec![reply(status, &[], "")], key());
            assert!(matches!(
                c.send_bundle("{}").await.unwrap_err(),
                DeliveryError::Authorization { .. }
            ));
            assert_eq!(c.transport.sent.len(), 1);
        }
    }

    #[tokio::test]
    async fn a_401_with_oauth_refreshes_the_token_once_then_succeeds() {
        let auth = Auth::oauth2(
            "https://auth.example/t",
            "i",
            Secret::new("s"),
            "x",
            Duration::from_secs(5),
        )
        .unwrap();
        let (mut c, _) = client(
            vec![
                reply(200, &[], r#"{"access_token":"old"}"#),
                reply(401, &[], ""),
                reply(200, &[], r#"{"access_token":"new"}"#),
                reply(200, &[], &outcome("information")),
            ],
            auth,
        );
        c.send_bundle("{}").await.unwrap();

        let bearers: Vec<_> = c
            .transport
            .sent
            .iter()
            .filter_map(|r| r.headers.iter().find(|(k, _)| k == "Authorization"))
            .map(|(_, v)| v.clone())
            .collect();
        assert_eq!(bearers, ["Bearer old", "Bearer new"]);
    }

    #[tokio::test]
    async fn a_second_401_after_the_refresh_is_an_authorization_failure() {
        let auth = Auth::oauth2(
            "https://a/t",
            "i",
            Secret::new("s"),
            "x",
            Duration::from_secs(5),
        )
        .unwrap();
        let (mut c, _) = client(
            vec![
                reply(200, &[], r#"{"access_token":"a"}"#),
                reply(401, &[], ""),
                reply(200, &[], r#"{"access_token":"b"}"#),
                reply(401, &[], ""),
            ],
            auth,
        );
        assert!(matches!(
            c.send_bundle("{}").await.unwrap_err(),
            DeliveryError::Authorization { status: 401, .. }
        ));
    }

    #[tokio::test]
    async fn a_5xx_backs_off_exponentially_then_gives_up_transient() {
        let (mut c, clock) = client(
            vec![
                reply(503, &[], ""),
                reply(500, &[], ""),
                reply(502, &[], ""),
            ],
            key(),
        );
        let error = c.send_bundle("{}").await.unwrap_err();

        let DeliveryError::Transient(text) = error else {
            panic!("expected transient");
        };
        assert!(
            text.starts_with("POST failed after 3 attempt(s): HTTP 502"),
            "{text}"
        );
        // 2 s then 4 s; no wait after the last attempt.
        assert_eq!(*clock.sleeps.lock().unwrap(), [2.0, 4.0]);
    }

    #[tokio::test]
    async fn a_network_error_is_retried_and_can_recover() {
        let (mut c, _) = client(
            vec![
                Err(TransportError("connection refused".into())),
                reply(200, &[], &outcome("information")),
            ],
            key(),
        );
        c.send_bundle("{}").await.unwrap();
        assert_eq!(c.transport.sent.len(), 2);
    }

    #[tokio::test]
    async fn a_429_waits_for_retry_after_capped_at_the_maximum() {
        let (mut c, clock) = client(
            vec![
                reply(429, &[("Retry-After", "500")], ""),
                reply(429, &[], ""),
                reply(200, &[], &outcome("information")),
            ],
            key(),
        );
        c.send_bundle("{}").await.unwrap();
        // The cap (60), then the backoff for attempt 2 (4).
        assert_eq!(*clock.sleeps.lock().unwrap(), [60.0, 4.0]);
    }

    #[tokio::test]
    async fn polling_past_the_timeout_is_transient() {
        // Retry-After 50 against a 120 s timeout: waits at 0 and 50, and at
        // 100 the next wait would pass the deadline.
        let mut script = vec![reply(202, &[("Content-Location", "/s")], "")];
        for _ in 0..4 {
            script.push(reply(
                202,
                &[("X-Progress", "queued"), ("Retry-After", "50")],
                "",
            ));
        }
        let (mut c, _) = client(script, key());
        let error = c.send_bundle("{}").await.unwrap_err();
        assert_eq!(
            error,
            DeliveryError::Transient(
                "Processing not complete after 120s (X-Progress: queued)".into()
            )
        );
    }

    #[tokio::test]
    async fn a_poll_that_lands_on_the_deadline_is_transient_too() {
        // 5 s steps reach exactly 120 s, so the next request finds no time left.
        let mut script = vec![reply(202, &[("Content-Location", "/s")], "")];
        for _ in 0..40 {
            script.push(reply(202, &[], ""));
        }
        let (mut c, _) = client(script, key());
        let DeliveryError::Transient(text) = c.send_bundle("{}").await.unwrap_err() else {
            panic!("expected transient");
        };
        assert!(
            text.starts_with("Polling deadline reached before GET"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_202_without_a_location_is_transient() {
        let (mut c, _) = client(vec![reply(202, &[], "")], key());
        assert!(matches!(
            c.send_bundle("{}").await.unwrap_err(),
            DeliveryError::Transient(_)
        ));
    }

    #[tokio::test]
    async fn a_status_url_on_another_origin_is_refused_before_credentials_are_sent() {
        // spec/10-senders.md §8.4.
        for location in [
            "https://evil.example/status",
            "http://promptly.example/status",
            "//evil.example/status",
        ] {
            let (mut c, _) = client(
                vec![reply(202, &[("Content-Location", location)], "")],
                key(),
            );
            let error = c.send_bundle("{}").await.unwrap_err();
            assert_eq!(
                error,
                DeliveryError::Permanent {
                    status: 202,
                    detail: "Status URL origin differs from the configured endpoint".into()
                },
                "{location}"
            );
            assert_eq!(c.transport.sent.len(), 1, "no second request: {location}");
        }
    }

    #[test]
    fn the_endpoint_must_be_https_unless_insecure_is_allowed() {
        let make = |url: &str, insecure: bool| {
            FhirClient::new(
                url,
                key(),
                Settings {
                    allow_insecure_http: insecure,
                    ..Settings::default()
                },
                FakeTransport::new(vec![]),
                FakeClock::new(),
            )
            .map(|_| ())
        };
        assert!(make("https://p/x", false).is_ok());
        assert!(make("http://localhost/x", false).is_err());
        assert!(make("http://localhost/x", true).is_ok());
        assert!(make("ftp://p/x", true).is_err());
    }

    #[test]
    fn urls_are_joined_the_way_a_content_location_needs() {
        let base = "https://h.example:8443/fhir/r4/$process-message?x=1";
        assert_eq!(join_url(base, "/s/1"), "https://h.example:8443/s/1");
        assert_eq!(join_url(base, "s/1"), "https://h.example:8443/fhir/r4/s/1");
        assert_eq!(join_url(base, "https://o/s"), "https://o/s");
        assert_eq!(join_url(base, "//o/s"), "https://o/s");
        assert_eq!(origin("HTTPS://Host.Example/a"), "https://host.example");
    }

    #[test]
    fn a_summary_is_bounded_and_falls_back_to_the_raw_body() {
        let long = format!(
            r#"{{"issue":[{{"severity":"error","code":"x","diagnostics":"{}"}}]}}"#,
            "d".repeat(2000)
        );
        let summary = summarise_outcome(&Response {
            status: 400,
            headers: vec![],
            body: long.into_bytes(),
        });
        assert_eq!(summary.chars().count(), MAX_DETAIL_LENGTH);

        let plain = summarise_outcome(&Response {
            status: 400,
            headers: vec![],
            body: b"gateway said no".to_vec(),
        });
        assert_eq!(plain, "gateway said no");

        let empty = summarise_outcome(&Response {
            status: 404,
            headers: vec![],
            body: vec![],
        });
        assert_eq!(empty, "Not Found");

        let no_issues = summarise_outcome(&Response {
            status: 400,
            headers: vec![],
            body: br#"{"a":1}"#.to_vec(),
        });
        assert_eq!(no_issues, r#"{"a":1}"#);
    }
}

#[cfg(test)]
impl<C: Clock> FhirClient<test_support::FakeTransport, C> {
    /// How many requests the fake transport has seen.
    pub fn transport_requests(&self) -> usize {
        self.transport.sent.len()
    }
}
