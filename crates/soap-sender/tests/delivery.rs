//! Against a real HTTP endpoint.
//!
//! Implements `spec/10-senders.md` §6.8.
//!
//! The spec names `wiremock`; this uses a few lines of `tokio` instead. The
//! rows being covered are "answer this status with this body", which does
//! not need a mocking framework, and adding one would put a dependency in
//! the build for something a `TcpListener` already does.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ihb_sender::{Delivery, DeliveryError};
use soap_sender::{Endpoint, SoapClient};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MESSAGE: &str = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|MSGID1|P|2.5";

/// How the endpoint answers one request.
#[derive(Clone)]
enum Answer {
    /// A status and a body.
    Respond(u16, String),
    /// Read the request and never answer.
    Silence,
}

/// A running endpoint.
struct Server {
    url: String,
    /// Every request body it received, in order.
    received: Arc<std::sync::Mutex<Vec<String>>>,
    /// How many requests it has seen.
    requests: Arc<AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

/// Start an endpoint that answers each request from a script, in order.
async fn endpoint(script: Vec<Answer>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let requests = Arc::new(AtomicUsize::new(0));

    let bodies = Arc::clone(&received);
    let counter = Arc::clone(&requests);

    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let script = script.clone();
            let bodies = Arc::clone(&bodies);
            let counter = Arc::clone(&counter);

            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = vec![0_u8; 8192];

                // Read until the body is complete enough to answer.
                loop {
                    let Ok(read) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..read]);

                    let text = String::from_utf8_lossy(&buffer).into_owned();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let declared: usize = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    if body.len() < declared {
                        continue;
                    }

                    bodies.lock().unwrap().push(body.to_string());
                    let index = counter.fetch_add(1, Ordering::SeqCst);

                    match script.get(index).cloned() {
                        Some(Answer::Silence) | None if script.is_empty() => return,
                        Some(Answer::Silence) => {
                            tokio::time::sleep(Duration::from_secs(3600)).await;
                            return;
                        }
                        Some(Answer::Respond(status, response)) => {
                            let head = format!(
                                "HTTP/1.1 {status} X\r\nContent-Type: text/xml\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                                response.len()
                            );
                            let _ = stream.write_all(head.as_bytes()).await;
                            let _ = stream.write_all(response.as_bytes()).await;
                            let _ = stream.flush().await;
                            return;
                        }
                        None => return,
                    }
                }
            });
        }
    });

    Server {
        url: format!("http://{address}/soap"),
        received,
        requests,
        _task: task,
    }
}

/// A client pointed at `server`.
fn client(server: &Server, timeout: Duration) -> SoapClient {
    SoapClient::new(Endpoint {
        url: server.url.clone(),
        timeout,
        api_key: None,
    })
}

/// Answer once with `status` and `body`.
async fn answering(status: u16, body: &str) -> Server {
    endpoint(vec![Answer::Respond(status, body.to_string())]).await
}

#[tokio::test]
async fn a_status_element_saying_yes_is_accepted() {
    // spec/10-senders.md §4.4's third row, and §6.8.
    for code in ["AA", "CA"] {
        let server = answering(
            200,
            &format!("<Envelope><Status>{code}</Status></Envelope>"),
        )
        .await;
        let mut client = client(&server, Duration::from_secs(5));

        assert!(
            client.deliver(MESSAGE).await.unwrap().is_accepted(),
            "{code}"
        );
    }
}

#[tokio::test]
async fn a_status_element_saying_anything_else_is_rejected() {
    // §4.4's fourth row.
    for code in ["AE", "AR", "XX"] {
        let server = answering(
            200,
            &format!("<Envelope><Status>{code}</Status></Envelope>"),
        )
        .await;
        let mut client = client(&server, Duration::from_secs(5));

        let verdict = client.deliver(MESSAGE).await.unwrap();
        assert!(!verdict.is_accepted(), "{code}");
    }
}

#[tokio::test]
async fn a_fault_is_rejected_whatever_the_status() {
    // §4.4's second row, and §4.4.1 — the reason it is checked before
    // parsing. Without it a fault with no `Status` element reads as
    // success.
    let fault = "<soapenv:Envelope><soapenv:Body><soapenv:Fault>\
                 <faultcode>Server</faultcode></soapenv:Fault></soapenv:Body></soapenv:Envelope>";

    for status in [200, 202, 500] {
        let server = answering(status, fault).await;
        let mut client = client(&server, Duration::from_secs(5));

        assert!(
            !client.deliver(MESSAGE).await.unwrap().is_accepted(),
            "{status}"
        );
    }
}

#[tokio::test]
async fn a_non_ok_status_is_rejected() {
    // §4.4's first row.
    for status in [400, 401, 404, 500, 503] {
        let server = answering(status, "<Status>AA</Status>").await;
        let mut client = client(&server, Duration::from_secs(5));

        assert!(
            !client.deliver(MESSAGE).await.unwrap().is_accepted(),
            "{status}"
        );
    }
}

#[tokio::test]
async fn a_minimal_empty_response_is_accepted() {
    // §4.4's last row and §4.4.1 — deliberate: some endpoints answer a bare
    // 200. The fault check above is what stops this swallowing a failure.
    for status in [200, 202] {
        let server = answering(status, "").await;
        let mut client = client(&server, Duration::from_secs(5));

        assert!(
            client.deliver(MESSAGE).await.unwrap().is_accepted(),
            "{status}"
        );
    }
}

#[tokio::test]
async fn the_envelope_that_goes_out_is_the_specified_one() {
    // §4.2 and §6.5 — asserted on what the endpoint actually received,
    // rather than on the wrapper in isolation.
    let server = answering(200, "<Status>AA</Status>").await;
    let mut client = client(&server, Duration::from_secs(5));

    client.deliver("PID|||1||O'BRIEN^\"J\"&<x>").await.unwrap();

    let body = server.received.lock().unwrap()[0].clone();

    assert!(body.contains("<soapenv:Envelope"), "{body}");
    assert!(body.contains("<SendHL7Message>"), "{body}");
    assert!(body.contains("&amp;"), "the ampersand is escaped: {body}");
    assert!(body.contains("&lt;x&gt;"), "the angle brackets are: {body}");
    assert!(body.contains("O'BRIEN"), "the apostrophe is not: {body}");
    assert!(body.contains("\"J\""), "nor the quotes: {body}");
}

#[tokio::test]
async fn a_timeout_retries_exactly_once_then_fails() {
    // §4.3 — the wording is normative, and the retry count is the point.
    let server = endpoint(vec![Answer::Silence, Answer::Silence]).await;
    let mut client = client(&server, Duration::from_secs(1));

    let error = client.deliver(MESSAGE).await.unwrap_err();

    assert_eq!(
        error,
        DeliveryError::Connection("No SOAP response within 1 seconds".to_string())
    );
    assert_eq!(
        server.requests.load(Ordering::SeqCst),
        2,
        "the first attempt and exactly one retry"
    );
}

#[tokio::test]
async fn the_retry_succeeds_when_the_endpoint_answers_the_second_time() {
    // §4.3 — the case the retry exists for.
    let server = endpoint(vec![
        Answer::Silence,
        Answer::Respond(200, "<Status>AA</Status>".to_string()),
    ])
    .await;
    let mut client = client(&server, Duration::from_secs(1));

    assert!(client.deliver(MESSAGE).await.unwrap().is_accepted());
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_endpoint_that_is_not_there_fails_without_retrying() {
    // §4.3 — a connection error fails immediately. The endpoint is not
    // there; trying again in the same millisecond will not change that, and
    // Service Bus redelivers with backoff.
    let mut client = SoapClient::new(Endpoint {
        url: "http://127.0.0.1:1/soap".to_string(),
        timeout: Duration::from_millis(200),
        api_key: None,
    });

    let error = client.deliver(MESSAGE).await.unwrap_err();

    assert!(
        error.to_string().starts_with("SOAP connection error:"),
        "§4.3's exact wording: {error}"
    );
    assert_eq!(
        error.validation_result(),
        "Message sending failed - connection/timeout error"
    );
}

#[tokio::test]
async fn the_wis_format_posts_the_wis_envelope() {
    // spec/10-senders.md §4.6.2, over a real socket, and §4.4 unchanged.
    let server = answering(200, "<Envelope><Status>AA</Status></Envelope>").await;
    let mut sender = client(&server, Duration::from_secs(5)).with_format(soap_sender::Format::Wis);

    let verdict = sender.deliver(MESSAGE).await.unwrap();

    assert!(verdict.is_accepted());
    let posted = server.received.lock().unwrap()[0].clone();
    assert!(posted.contains("CaptureFromFiorona"), "{posted}");
    assert!(!posted.contains("SendHL7Message"), "{posted}");
}

#[tokio::test]
async fn an_unconvertible_message_is_not_posted_under_the_wis_format() {
    // spec/10-senders.md §4.6.3 — an `Unexpected` failure, and no request.
    let server = answering(200, "").await;
    let mut sender = client(&server, Duration::from_secs(5)).with_format(soap_sender::Format::Wis);

    let error = sender.deliver("not HL7 at all").await.unwrap_err();

    assert!(matches!(error, DeliveryError::Unexpected(_)), "{error:?}");
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
}
