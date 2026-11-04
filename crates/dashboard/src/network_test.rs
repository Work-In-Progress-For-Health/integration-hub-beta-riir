//! Connectivity testing: DNS, TCP connect and latency.
//!
//! Implements `spec/15-dashboard.md` §10.4.1–§10.4.4 (upstream `4b4f589`).
//!
//! # Sockets and nothing else
//!
//! No subprocess, no `ping`, no raw socket: this runs as an ordinary
//! unprivileged container, so a TCP connect is all it can offer, and a
//! hop-by-hop traceroute is out of scope for exactly that reason.
//!
//! # What reaches a log line
//!
//! §10.4.10. Neither the host nor the port. They arrive from a text box,
//! are validated before any socket sees them, and are not echoed in an
//! error.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use serde_json::Value;
use time::OffsetDateTime;

use crate::alarm::documents::NETWORK_TEST_HISTORY;
use crate::alarm::store::{AlarmStore, StoreError};

/// The Cosmos partition for history (§10.4.4).
pub const PARTITION: &str = "network-test";

/// Samples kept per endpoint (§10.4.4).
pub const MAX_HISTORY_SAMPLES: usize = 200;

/// Connect attempts per test unless asked otherwise (§10.4.1).
pub const DEFAULT_ATTEMPTS: usize = 4;

/// Each connect's timeout (§10.4.1).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// DNS resolution's timeout (§10.4.1).
pub const DEFAULT_DNS_TIMEOUT: Duration = Duration::from_secs(3);

/// A host or port that failed validation. Carries nothing: the message is
/// fixed so a response cannot echo what was typed (§10.4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTarget;

impl std::fmt::Display for InvalidTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Invalid host or port.")
    }
}

/// Whether a label is a valid RFC 1123 hostname label.
fn is_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Validate a host: an IP address or a hostname (§10.4.3).
///
/// Syntactic only — reachability is the test's business.
///
/// # Errors
///
/// [`InvalidTarget`].
///
/// # Examples
///
/// ```
/// use dashboard::network_test::validate_host;
///
/// assert_eq!(validate_host(" Example.org ").unwrap(), "Example.org");
/// assert!(validate_host("10.0.0.1").is_ok());
/// assert!(validate_host("::1").is_ok());
/// assert!(validate_host("a b").is_err());
/// assert!(validate_host("host;rm -rf /").is_err());
/// ```
pub fn validate_host(host: &str) -> Result<String, InvalidTarget> {
    let host = host.trim();
    if host.is_empty() || host.len() > 253 {
        return Err(InvalidTarget);
    }
    if host.parse::<IpAddr>().is_ok() {
        return Ok(host.to_string());
    }
    if host.split('.').all(is_label) {
        Ok(host.to_string())
    } else {
        Err(InvalidTarget)
    }
}

/// Validate a port from JSON or a query string: 1 to 65535 (§10.4.3).
///
/// # Errors
///
/// [`InvalidTarget`].
pub fn validate_port(port: &Value) -> Result<u16, InvalidTarget> {
    let number = match port {
        Value::Number(n) => n.as_i64(),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    };

    number
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| *n >= 1)
        .ok_or(InvalidTarget)
}

/// One TCP connect.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Attempt {
    /// Whether it connected.
    pub success: bool,
    /// Milliseconds to connect.
    pub latency_ms: Option<f64>,
    /// Why not.
    pub error: Option<String>,
}

/// DNS resolution (§10.4.2).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Dns {
    /// Whether it resolved.
    pub resolved: bool,
    /// The addresses, sorted.
    pub addresses: Vec<String>,
    /// Milliseconds.
    pub resolve_time_ms: f64,
    /// Why not; absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A test's result (§10.4.2).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TestResult {
    /// The validated host.
    pub host: String,
    /// The validated port.
    pub port: u16,
    /// Resolution.
    pub dns: Dns,
    /// One per attempt.
    pub attempts: Vec<Attempt>,
    /// Connects that succeeded.
    pub success_count: usize,
    /// Connects that failed.
    pub failure_count: usize,
    /// Percentage failed, one decimal place.
    pub loss_percent: f64,
    /// Fastest connect.
    pub min_latency_ms: Option<f64>,
    /// Mean connect.
    pub avg_latency_ms: Option<f64>,
    /// Slowest connect.
    pub max_latency_ms: Option<f64>,
    /// Any attempt connected.
    pub success: bool,
    /// Unix seconds.
    pub timestamp: f64,
    /// Set when the result could not be saved to history (§10.4.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_warning: Option<String>,
}

/// How a test runs. The defaults are §10.4.1's; tests shorten them.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Connect attempts, at least 1.
    pub attempts: usize,
    /// Each connect's timeout.
    pub timeout: Duration,
    /// Resolution's timeout.
    pub dns_timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            attempts: DEFAULT_ATTEMPTS,
            timeout: DEFAULT_TIMEOUT,
            dns_timeout: DEFAULT_DNS_TIMEOUT,
        }
    }
}

fn millis(elapsed: Duration) -> f64 {
    (elapsed.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

async fn resolve(host: &str, timeout: Duration) -> Dns {
    let started = Instant::now();

    let outcome = tokio::time::timeout(timeout, tokio::net::lookup_host((host, 0))).await;
    let (resolved, addresses, error) = match outcome {
        Ok(Ok(found)) => {
            let mut addresses: Vec<String> = found.map(|a| a.ip().to_string()).collect();
            addresses.sort();
            addresses.dedup();
            (true, addresses, None)
        }
        Ok(Err(error)) => (false, Vec::new(), Some(error.to_string())),
        Err(_) => (
            false,
            Vec::new(),
            Some(format!(
                "DNS resolution timed out after {}s",
                timeout.as_secs_f64()
            )),
        ),
    };

    Dns {
        resolved,
        addresses,
        resolve_time_ms: millis(started.elapsed()),
        error,
    }
}

async fn connect_once(address: IpAddr, port: u16, timeout: Duration) -> Attempt {
    let started = Instant::now();

    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect((address, port))).await {
        Ok(Ok(_stream)) => Attempt {
            success: true,
            latency_ms: Some(millis(started.elapsed())),
            error: None,
        },
        Ok(Err(error)) => Attempt {
            success: false,
            latency_ms: None,
            error: Some(error.to_string()),
        },
        Err(_) => Attempt {
            success: false,
            latency_ms: None,
            error: Some("Connection timed out".to_string()),
        },
    }
}

/// Run a test (§10.4.1).
///
/// # Errors
///
/// [`InvalidTarget`] — before any socket is touched.
pub async fn run(
    host: &str,
    port: &Value,
    settings: Settings,
    now: OffsetDateTime,
) -> Result<TestResult, InvalidTarget> {
    let host = validate_host(host)?;
    let port = validate_port(port)?;
    let attempts = settings.attempts.max(1);

    let dns = resolve(&host, settings.dns_timeout).await;

    let results: Vec<Attempt> = if let Some(address) = dns
        .addresses
        .first()
        .and_then(|a| a.parse::<IpAddr>().ok())
        .filter(|_| dns.resolved)
    {
        // The address resolved once, not the name every time (§10.4.1).
        let mut results = Vec::with_capacity(attempts);
        for _ in 0..attempts {
            results.push(connect_once(address, port, settings.timeout).await);
        }
        results
    } else {
        // Every attempt would repeat the same failure, possibly slowly, so
        // it is reported rather than retried.
        let error = dns
            .error
            .clone()
            .unwrap_or_else(|| "DNS resolution failed".to_string());
        (0..attempts)
            .map(|_| Attempt {
                success: false,
                latency_ms: None,
                error: Some(error.clone()),
            })
            .collect()
    };

    let latencies: Vec<f64> = results.iter().filter_map(|a| a.latency_ms).collect();
    let failure_count = results.iter().filter(|a| !a.success).count();
    let round2 = |n: f64| (n * 100.0).round() / 100.0;

    #[allow(clippy::cast_precision_loss)]
    let loss_percent =
        ((failure_count as f64 / results.len() as f64) * 100.0 * 10.0).round() / 10.0;
    #[allow(clippy::cast_precision_loss)]
    let average = (!latencies.is_empty())
        .then(|| round2(latencies.iter().sum::<f64>() / latencies.len() as f64));

    Ok(TestResult {
        host,
        port,
        dns,
        success_count: latencies.len(),
        failure_count,
        loss_percent,
        min_latency_ms: latencies.iter().copied().reduce(f64::min).map(round2),
        avg_latency_ms: average,
        max_latency_ms: latencies.iter().copied().reduce(f64::max).map(round2),
        success: !latencies.is_empty(),
        timestamp: now.unix_timestamp_nanos() as f64 / 1e9,
        attempts: results,
        history_warning: None,
    })
}

/// The history document's id (§10.4.4).
fn history_id(host: &str, port: u16) -> String {
    format!("history:{host}:{port}")
}

/// Append a sample to an endpoint's history (§10.4.4).
///
/// # Errors
///
/// [`StoreError`] — the caller turns it into `history_warning`.
pub async fn save_sample<S: AlarmStore>(store: &S, result: &TestResult) -> Result<(), StoreError> {
    // Nothing is kept, so there is nothing to warn about (§10.4.4).
    if !store.is_enabled() {
        return Ok(());
    }

    let id = history_id(&result.host, result.port);
    let mut samples = samples_of(
        store
            .documents(PARTITION)
            .await?
            .iter()
            .find(|d| document_is(d, &result.host, result.port)),
    );
    samples.push(serde_json::json!({
        "timestamp": result.timestamp,
        "success": result.success,
        "avg_latency_ms": result.avg_latency_ms,
        "min_latency_ms": result.min_latency_ms,
        "max_latency_ms": result.max_latency_ms,
        "loss_percent": result.loss_percent,
    }));
    if samples.len() > MAX_HISTORY_SAMPLES {
        samples.drain(..samples.len() - MAX_HISTORY_SAMPLES);
    }

    store
        .upsert_document(
            PARTITION,
            &id,
            NETWORK_TEST_HISTORY,
            &serde_json::json!({"host": result.host, "port": result.port, "samples": samples}),
        )
        .await
}

fn document_is(document: &Value, host: &str, port: u16) -> bool {
    document.get("host").and_then(Value::as_str) == Some(host)
        && document.get("port").and_then(Value::as_u64) == Some(u64::from(port))
}

fn samples_of(document: Option<&Value>) -> Vec<Value> {
    document
        .and_then(|d| d.get("samples"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// An endpoint's history, oldest first (§10.4.4).
///
/// # Errors
///
/// [`StoreError`].
pub async fn history<S: AlarmStore>(
    store: &S,
    host: &str,
    port: u16,
) -> Result<Vec<Value>, StoreError> {
    Ok(samples_of(
        store
            .documents(PARTITION)
            .await?
            .iter()
            .find(|d| document_is(d, host, port)),
    ))
}

/// Remove an endpoint's history, and with it the endpoint from the list
/// (§10.4.4).
///
/// # Errors
///
/// [`StoreError`].
pub async fn delete_history<S: AlarmStore>(
    store: &S,
    host: &str,
    port: u16,
) -> Result<(), StoreError> {
    store
        .delete_document(PARTITION, &history_id(host, port))
        .await
}

/// A tested endpoint and its latest sample (§10.4.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Tested {
    /// Host.
    pub host: String,
    /// Port.
    pub port: u16,
    /// The most recent sample.
    pub latest: Value,
}

/// Every endpoint that has been tested, most recent first. One with no
/// samples is omitted rather than shown with an empty "latest".
///
/// # Errors
///
/// [`StoreError`].
pub async fn list_tested<S: AlarmStore>(store: &S) -> Result<Vec<Tested>, StoreError> {
    let mut tested: Vec<Tested> = store
        .documents(PARTITION)
        .await?
        .iter()
        .filter_map(|document| {
            let host = document.get("host")?.as_str()?.to_string();
            let port = u16::try_from(document.get("port")?.as_u64()?).ok()?;
            let latest = samples_of(Some(document)).pop()?;
            Some(Tested { host, port, latest })
        })
        .collect();

    let when = |t: &Tested| {
        t.latest
            .get("timestamp")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    tested.sort_by(|a, b| when(b).total_cmp(&when(a)));

    Ok(tested)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::store::{Disabled, InMemory};
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-10-01 09:00 UTC);

    fn quick() -> Settings {
        Settings {
            attempts: 3,
            timeout: Duration::from_millis(500),
            dns_timeout: Duration::from_secs(2),
        }
    }

    #[test]
    fn hosts_are_validated_syntactically() {
        // spec/15-dashboard.md §10.4.3.
        for good in [
            "example.org",
            "a",
            "a-b.c-d.example",
            "10.1.2.3",
            "2001:db8::1",
            "xn--bcher-kva.example",
        ] {
            assert!(validate_host(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            " ",
            "-a.example",
            "a-.example",
            "a..b",
            ".a",
            "a.",
            "a b",
            "a_b.example",
            "http://x",
            "x/y",
            "x;y",
            "x\ny",
            "ü.example",
            &"a".repeat(64),
            &format!(
                "{}.{}.{}.{}.{}.{}",
                "a".repeat(60),
                "a".repeat(60),
                "a".repeat(60),
                "a".repeat(60),
                "a".repeat(10),
                "b"
            ),
        ] {
            assert!(validate_host(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ports_are_one_to_65535_from_a_number_or_a_string() {
        assert_eq!(validate_port(&serde_json::json!(2575)), Ok(2575));
        assert_eq!(validate_port(&serde_json::json!(" 443 ")), Ok(443));
        assert_eq!(validate_port(&serde_json::json!(65535)), Ok(65535));
        for bad in [
            serde_json::json!(0),
            serde_json::json!(65536),
            serde_json::json!(-1),
            serde_json::json!("x"),
            serde_json::json!(null),
            serde_json::json!(1.5),
            serde_json::json!([1]),
        ] {
            assert!(validate_port(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_refusal_names_nothing_that_was_typed() {
        // §10.4.3.
        assert_eq!(InvalidTarget.to_string(), "Invalid host or port.");
    }

    #[tokio::test]
    async fn a_listening_port_connects_and_is_timed() {
        // §10.4.1, §10.4.2 -- against a real socket on loopback.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let result = run("127.0.0.1", &serde_json::json!(port), quick(), NOW)
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.success_count, 3);
        assert_eq!(result.failure_count, 0);
        assert!((result.loss_percent - 0.0).abs() < f64::EPSILON);
        assert!(result.min_latency_ms.unwrap() <= result.avg_latency_ms.unwrap() + 0.01);
        assert!(result.avg_latency_ms.unwrap() <= result.max_latency_ms.unwrap() + 0.01);
        assert!(result.dns.resolved);
        assert_eq!(result.dns.addresses, ["127.0.0.1"]);
        assert!(
            result
                .attempts
                .iter()
                .all(|a| a.success && a.error.is_none())
        );
    }

    #[tokio::test]
    async fn a_closed_port_fails_every_attempt() {
        // Bind and drop to find a port nothing is listening on.
        let port = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };

        let result = run("127.0.0.1", &serde_json::json!(port), quick(), NOW)
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(result.failure_count, 3);
        assert!((result.loss_percent - 100.0).abs() < f64::EPSILON);
        assert_eq!(result.min_latency_ms, None);
        assert_eq!(result.avg_latency_ms, None);
        assert!(result.attempts.iter().all(|a| a.error.is_some()));
    }

    #[tokio::test]
    async fn a_name_that_does_not_resolve_is_reported_without_connecting() {
        // §10.4.1. `.invalid` is reserved by RFC 2606 and never resolves.
        let result = run("nothing.invalid", &serde_json::json!(443), quick(), NOW)
            .await
            .unwrap();

        assert!(!result.dns.resolved);
        assert!(result.dns.error.is_some());
        assert_eq!(result.attempts.len(), 3);
        assert!(result.attempts.iter().all(|a| a.error == result.dns.error));
    }

    #[tokio::test]
    async fn an_invalid_target_never_reaches_a_socket() {
        assert_eq!(
            run("bad host", &serde_json::json!(80), quick(), NOW)
                .await
                .unwrap_err(),
            InvalidTarget
        );
        assert_eq!(
            run("example.org", &serde_json::json!(0), quick(), NOW)
                .await
                .unwrap_err(),
            InvalidTarget
        );
    }

    #[tokio::test]
    async fn at_least_one_attempt_is_made() {
        let result = run(
            "127.0.0.1",
            &serde_json::json!(1),
            Settings {
                attempts: 0,
                ..quick()
            },
            NOW,
        )
        .await
        .unwrap();

        assert_eq!(result.attempts.len(), 1);
    }

    #[test]
    fn the_result_has_the_keys_the_page_reads() {
        // §10.4.2.
        let result = TestResult {
            host: "h".into(),
            port: 1,
            dns: Dns {
                resolved: true,
                addresses: vec![],
                resolve_time_ms: 0.0,
                error: None,
            },
            attempts: vec![],
            success_count: 0,
            failure_count: 0,
            loss_percent: 0.0,
            min_latency_ms: None,
            avg_latency_ms: None,
            max_latency_ms: None,
            success: false,
            timestamp: 0.0,
            history_warning: None,
        };

        let json = serde_json::to_value(&result).unwrap();
        for key in [
            "host",
            "port",
            "dns",
            "attempts",
            "success_count",
            "failure_count",
            "loss_percent",
            "min_latency_ms",
            "avg_latency_ms",
            "max_latency_ms",
            "success",
            "timestamp",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert!(
            json.get("history_warning").is_none(),
            "only when there is one"
        );
        assert!(json["dns"].get("error").is_none(), "only on failure");
    }

    fn sample_result(host: &str, port: u16, timestamp: f64) -> TestResult {
        TestResult {
            host: host.to_string(),
            port,
            dns: Dns {
                resolved: true,
                addresses: vec![],
                resolve_time_ms: 0.0,
                error: None,
            },
            attempts: vec![],
            success_count: 1,
            failure_count: 0,
            loss_percent: 0.0,
            min_latency_ms: Some(1.0),
            avg_latency_ms: Some(2.0),
            max_latency_ms: Some(3.0),
            success: true,
            timestamp,
            history_warning: None,
        }
    }

    #[tokio::test]
    async fn history_accumulates_is_typed_and_is_capped() {
        // §10.4.4, §10.1.2.
        let store = InMemory::new();

        for i in 0..(MAX_HISTORY_SAMPLES + 5) {
            save_sample(&store, &sample_result("example.org", 443, i as f64))
                .await
                .unwrap();
        }

        let stored = store
            .raw_document(PARTITION, "history:example.org:443")
            .unwrap();
        assert_eq!(stored["type"], "network_test_history");

        let samples = history(&store, "example.org", 443).await.unwrap();
        assert_eq!(samples.len(), MAX_HISTORY_SAMPLES);
        assert_eq!(
            samples.last().unwrap()["timestamp"],
            (MAX_HISTORY_SAMPLES + 4) as f64
        );
        assert_eq!(samples[0]["timestamp"], 5.0, "the oldest were dropped");
    }

    #[tokio::test]
    async fn the_list_is_most_recent_first_and_delete_removes_the_endpoint() {
        let store = InMemory::new();
        save_sample(&store, &sample_result("old.example", 80, 10.0))
            .await
            .unwrap();
        save_sample(&store, &sample_result("new.example", 80, 20.0))
            .await
            .unwrap();

        let tested = list_tested(&store).await.unwrap();
        assert_eq!(tested[0].host, "new.example");
        assert_eq!(tested[1].host, "old.example");

        delete_history(&store, "new.example", 80).await.unwrap();
        assert_eq!(list_tested(&store).await.unwrap().len(), 1);
        assert!(history(&store, "new.example", 80).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_same_host_on_two_ports_is_two_endpoints() {
        let store = InMemory::new();
        save_sample(&store, &sample_result("h.example", 80, 1.0))
            .await
            .unwrap();
        save_sample(&store, &sample_result("h.example", 443, 2.0))
            .await
            .unwrap();

        assert_eq!(history(&store, "h.example", 80).await.unwrap().len(), 1);
        assert_eq!(list_tested(&store).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn with_nothing_persisted_saving_is_not_an_error() {
        // §10.4.4 -- nothing was ever going to be kept.
        assert!(
            save_sample(&Disabled, &sample_result("h", 1, 1.0))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn with_persistence_failing_saving_is_an_error_for_the_caller_to_warn_about() {
        let store = InMemory::new();
        store.fail_writes("throttled");

        assert!(
            save_sample(&store, &sample_result("h", 1, 1.0))
                .await
                .is_err()
        );
    }
}
