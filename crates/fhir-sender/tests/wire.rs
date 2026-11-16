//! The real transport against a local socket (`spec/10-senders.md` §8.8).
//!
//! The policy is tested with scripted doubles in the crate; this proves the
//! bytes: the headers that reach the peer, a relative `Content-Location`
//! resolved against the endpoint, and a redirect left unfollowed.

// Test code: a failed `unwrap` is a failed test, which is the point.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use fhir_sender::auth::Auth;
use fhir_sender::client::{FhirClient, Settings, TokioClock};
use fhir_sender::error::DeliveryError;
use fhir_sender::http::HyperTransport;
use ihb_config::Secret;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const OUTCOME: &str = r#"{"resourceType":"OperationOutcome","issue":[{"severity":"information","code":"informational"}]}"#;

/// Serve one canned response per connection, recording each raw request.
async fn serve(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    tokio::spawn(async move {
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                buffer.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&buffer).into_owned();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buffer.len() >= end + 4 + length || read == 0 {
                        break;
                    }
                }
                if read == 0 {
                    break;
                }
            }
            record
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buffer).into_owned());
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    (format!("http://{address}"), seen)
}

fn http(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn client(base: &str) -> FhirClient<HyperTransport, TokioClock> {
    FhirClient::new(
        &format!("{base}/fhir/r4/$process-message"),
        Auth::api_key(Secret::new("wire-key")).unwrap(),
        Settings {
            allow_insecure_http: true,
            poll_interval_seconds: 0.01,
            ..Settings::default()
        },
        HyperTransport::new().unwrap(),
        TokioClock::new(),
    )
    .unwrap()
}

#[tokio::test]
async fn a_bundle_is_posted_then_the_status_url_is_polled() {
    let (base, seen) = serve(vec![
        http(
            "202 Accepted",
            "content-location: /fhir/r4/status/7\r\n",
            "",
        ),
        http("200 OK", "content-type: application/fhir+json\r\n", OUTCOME),
    ])
    .await;

    client(&base)
        .send_bundle(r#"{"resourceType":"Bundle","type":"message"}"#)
        .await
        .unwrap();

    let seen = seen.lock().unwrap().clone();
    assert!(
        seen[0].starts_with("POST /fhir/r4/$process-message HTTP/1.1"),
        "{}",
        seen[0]
    );
    assert!(
        seen[0]
            .to_lowercase()
            .contains("content-type: application/fhir+json")
    );
    assert!(seen[0].contains("x-api-key: wire-key"));
    assert!(seen[0].ends_with(r#"{"resourceType":"Bundle","type":"message"}"#));
    assert!(
        seen[1].starts_with("GET /fhir/r4/status/7 HTTP/1.1"),
        "{}",
        seen[1]
    );
    assert!(
        seen[1].contains("x-api-key: wire-key"),
        "the poll is authenticated too"
    );
}

#[tokio::test]
async fn a_redirect_is_never_followed() {
    // spec/10-senders.md §8.4 — credentials go only where they were configured.
    let (base, seen) = serve(vec![http(
        "302 Found",
        "location: http://elsewhere.invalid/steal\r\n",
        "",
    )])
    .await;

    let error = client(&base).send_bundle("{}").await.unwrap_err();

    assert!(
        matches!(error, DeliveryError::Permanent { status: 302, .. }),
        "{error:?}"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_unreachable_endpoint_is_transient_after_its_attempts() {
    // Nothing listens on this port: bind and drop to find a free one.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut client = FhirClient::new(
        &format!("http://127.0.0.1:{port}/x"),
        Auth::api_key(Secret::new("k")).unwrap(),
        Settings {
            allow_insecure_http: true,
            max_attempts: 2,
            backoff_seconds: 0.01,
            ..Settings::default()
        },
        HyperTransport::new().unwrap(),
        TokioClock::new(),
    )
    .unwrap();

    let error = client.send_bundle("{}").await.unwrap_err();
    assert!(
        matches!(&error, DeliveryError::Transient(text) if text.starts_with("POST failed after 2 attempt(s)")),
        "{error:?}"
    );
}
