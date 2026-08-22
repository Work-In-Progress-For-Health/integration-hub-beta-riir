//! `soap-sender` against `http-mock-receiver`, end to end.
//!
//! Implements `spec/11-mock-receivers.md` §4.7.
//!
//! The two halves were written from opposite sides of the same
//! specification — §4.4's response table and §3.6's response shape — and
//! nothing has yet checked that they agree. This is that check: the real
//! sender, over a real socket, against the real mock.
//!
//! It is the most valuable test in either crate, because both could be
//! individually correct against their own reading of the spec and still not
//! interoperate.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use http_mock_receiver::server::{Receiver, serve};
use ihb_bus::fake::{FakeBus, FakeSender};
use ihb_runtime::shutdown::Shutdown;
use ihb_sender::Delivery;
use soap_sender::{Endpoint, SoapClient};
use tokio::net::TcpStream;

/// The queue the mock forwards to.
const QUEUE: &str = "mpi-mock-receiver";

const MESSAGE: &str = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|MSGID1|P|2.5\r\
                       PID|||8888888^^^252^PI||SURNAME^FORENAME";

/// A running mock, and the bus it forwards to.
struct Mock {
    url: String,
    bus: FakeBus,
    shutdown: Shutdown,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Mock {
    async fn stop(self) {
        self.shutdown.stop();
        self.task.await.unwrap().unwrap();
    }
}

/// Start a mock receiver on an ephemeral port.
async fn mock(forwarding: bool) -> Mock {
    let bus = FakeBus::new();
    let sink: Option<FakeSender> = forwarding.then(|| bus.sender(QUEUE));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let receiver = Arc::new(Receiver {
        sink,
        session_id: Some("mpi".to_string()),
    });
    let shutdown = Shutdown::new();
    let watcher = shutdown.watcher();
    let task = tokio::spawn(async move { serve(address, receiver, watcher).await });

    for _ in 0..200 {
        if TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Mock {
        url: format!("http://{address}/soap"),
        bus,
        shutdown,
        task,
    }
}

/// A sender pointed at the mock.
fn sender(mock: &Mock) -> SoapClient {
    SoapClient::new(Endpoint {
        url: mock.url.clone(),
        timeout: Duration::from_secs(5),
        api_key: None,
    })
}

#[tokio::test]
async fn a_message_goes_from_the_sender_to_the_mocks_queue() {
    // spec/11-mock-receivers.md §4.7 — the whole path.
    let mock = mock(true).await;
    let mut sender = sender(&mock);

    let verdict = sender
        .deliver(MESSAGE)
        .await
        .expect("the exchange succeeds");

    assert!(
        verdict.is_accepted(),
        "the mock's §3.6 response must satisfy the sender's §4.4 table"
    );

    let forwarded = mock.bus.all_sent(QUEUE);
    assert_eq!(forwarded.len(), 1);
    assert_eq!(
        String::from_utf8_lossy(&forwarded[0].body),
        MESSAGE,
        "the payload survives the envelope round trip unchanged"
    );
    assert_eq!(forwarded[0].message_id.as_deref(), Some("MSGID1"));
    assert_eq!(forwarded[0].session_id.as_deref(), Some("mpi"));

    mock.stop().await;
}

#[tokio::test]
async fn the_fail_trigger_produces_a_fault_the_sender_rejects() {
    // §1.1 and §4.7 — the unhappy path, driven without a malformed
    // message. This is the test that proves §3.7's fault is one §4.4's row
    // 2 actually catches.
    let mock = mock(true).await;
    let mut sender = sender(&mock);

    let failing = MESSAGE.replace("SURNAME", "PLEASE-FAIL");
    let verdict = sender
        .deliver(&failing)
        .await
        .expect("the exchange completes");

    assert!(!verdict.is_accepted(), "the sender must reject a fault");
    assert!(
        mock.bus.all_sent(QUEUE).is_empty(),
        "and nothing is forwarded"
    );

    mock.stop().await;
}

#[tokio::test]
async fn an_escaped_payload_arrives_unescaped() {
    // spec/10-senders.md §4.2 escapes three characters; §3.4 unescapes
    // them. Neither half is checked by the other's tests, and a mismatch
    // would send MPI `&amp;` where the sender meant `&`.
    let mock = mock(true).await;
    let mut sender = sender(&mock);

    let awkward = "MSH|^~\\&|252|252|100|100|20250505||ADT^A31^ADT_A05|M&<>1|P|2.5\r\
                   PID|||1||O'BRIEN^\"J\"";

    assert!(sender.deliver(awkward).await.unwrap().is_accepted());

    let forwarded = mock.bus.all_sent(QUEUE);
    assert_eq!(
        String::from_utf8_lossy(&forwarded[0].body),
        awkward,
        "every character survives, escaped or not"
    );

    mock.stop().await;
}

#[tokio::test]
async fn a_mock_with_no_queue_still_acknowledges() {
    // §3.9 — log-only mode, which is how this runs on a laptop.
    let mock = mock(false).await;
    let mut sender = sender(&mock);

    assert!(sender.deliver(MESSAGE).await.unwrap().is_accepted());

    mock.stop().await;
}

#[tokio::test]
async fn a_request_with_no_payload_is_faulted_rather_than_accepted() {
    // §3.4's third step, through the sender's eyes: a mock that answered
    // `AA` to a request it could not read would let a broken sender look
    // healthy.
    let mock = mock(true).await;

    // Posted directly, because the sender always produces a payload.
    let body = "<soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\">\
                <soapenv:Body/></soapenv:Envelope>";
    let response = post(&mock.url, body).await;

    assert!(response.contains("Fault"), "{response}");
    assert!(mock.bus.all_sent(QUEUE).is_empty());

    mock.stop().await;
}

#[tokio::test]
async fn a_body_that_is_not_xml_is_still_answered() {
    // §3.2 — deliberately forgiving. A mock that refuses malformed input
    // tells you your harness is malformed, which you knew.
    let mock = mock(true).await;

    let response = post(&mock.url, "this is not xml at all").await;

    assert!(!response.is_empty(), "the mock always answers");
    assert!(response.contains("Envelope"), "{response}");

    mock.stop().await;
}

#[tokio::test]
async fn a_soap_twelve_request_is_answered_in_its_own_namespace() {
    // §3.3 — a 1.2 caller that received a 1.1 envelope would reject it.
    let mock = mock(true).await;

    let body = "<soapenv:Envelope xmlns:soapenv=\"http://www.w3.org/2003/05/soap-envelope\">\
                <soapenv:Body><hl7Message>MSH|^~\\&amp;|252|252|100|100|1||ADT^A31|M2|P|2.5\
                </hl7Message></soapenv:Body></soapenv:Envelope>";
    let response = post(&mock.url, body).await;

    assert!(
        response.contains("http://www.w3.org/2003/05/soap-envelope"),
        "{response}"
    );
    assert!(response.contains("<Status>AA</Status>"), "{response}");
    assert_eq!(
        mock.bus.all_sent(QUEUE)[0].message_id.as_deref(),
        Some("M2")
    );

    mock.stop().await;
}

/// POST a body and return the response body.
async fn post(url: &str, body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let address = url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .expect("the url has an authority")
        .to_string();

    let mut stream = TcpStream::connect(&address).await.unwrap();
    let request = format!(
        "POST /soap HTTP/1.1\r\nHost: {address}\r\nContent-Type: text/xml\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    let mut buffer = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buffer)).await;

    let text = String::from_utf8_lossy(&buffer).into_owned();
    text.split_once("\r\n\r\n")
        .map_or_else(|| text.clone(), |(_, body)| body.to_string())
}
