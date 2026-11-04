//! The SOAP sender tabs against a throwaway local listener.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §4.11 and
//! §8.1: nothing here touches a real receiver.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use integration_hub_tester::plugin::Plugin;
use integration_hub_tester::plugins::senders::SoapSenderPlugin;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const A01: &str = "MSH|^~\\&|SENDAPP|SENDFAC|RECVAPP|RECVFAC|20250703120000||ADT^A01^ADT_A01|MSG000001|P|2.5\rPID|1";

/// Answer one request with `status` and `body`; hand back what was posted.
async fn serve_once(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/soap", listener.local_addr().unwrap());
    let std_listener = listener.into_std().unwrap();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = TcpListener::from_std(std_listener).unwrap();
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buffer = [0u8; 4096];
            // Read until the headers and the declared body are in.
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                received.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&received).into_owned();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("content-length: ").map(str::to_owned))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if text.len() >= split + 4 + length || n == 0 {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: text/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&received).into_owned()
        })
    });
    (url, handle)
}

fn url_of(
    runtime: &tokio::runtime::Runtime,
    status: u16,
    body: &'static str,
) -> (String, std::thread::JoinHandle<String>) {
    runtime.block_on(serve_once(status, body))
}

#[test]
fn an_accepting_receiver_is_ok_and_gets_the_real_envelope() {
    // §4.11 and §3.1: the post carries `soap_sender::envelope::wrap`'s bytes.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (url, server) = url_of(
        &rt,
        200,
        "<Envelope><Body><Status>AA</Status></Body></Envelope>",
    );
    drop(rt); // `run` makes its own runtime; the listener lives on its thread.

    let outcome = SoapSenderPlugin::queue().with_url(url).run(A01).unwrap();
    let posted = server.join().unwrap();

    assert!(outcome.ok, "{}\n{}", outcome.summary, outcome.output);
    assert!(
        outcome.summary.contains("control_id=MSG000001"),
        "{}",
        outcome.summary
    );
    assert!(posted.starts_with("POST /soap"), "{posted}");
    assert!(
        posted
            .to_lowercase()
            .contains("content-type: text/xml; charset=utf-8"),
        "{posted}"
    );
    assert!(posted.contains("<hl7Message>MSH|"), "{posted}");
}

#[test]
fn a_fault_is_a_rejected_outcome_not_an_error() {
    // §4.11 and spec/10-senders.md §4.4.1: a body with a Fault is a failure
    // whatever the status.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (url, server) = url_of(
        &rt,
        500,
        "<Envelope><Body><Fault><faultstring>no</faultstring></Fault></Body></Envelope>",
    );
    drop(rt);

    let outcome = SoapSenderPlugin::subscription()
        .with_url(url)
        .run(A01)
        .unwrap();
    server.join().unwrap();

    assert!(!outcome.ok);
    assert!(outcome.summary.contains("HTTP 500"), "{}", outcome.summary);
    assert!(outcome.output.contains("Fault"), "{}", outcome.output);
}

#[test]
fn an_unreachable_receiver_says_how_to_start_one() {
    // §4.11. Bind then drop to find a port nothing listens on.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let outcome = SoapSenderPlugin::queue()
        .with_url(format!("http://127.0.0.1:{port}/soap"))
        .run(A01)
        .unwrap();
    assert!(!outcome.ok);
    assert!(
        outcome.output.contains("MOCK RECEIVER NOT REACHABLE"),
        "{}",
        outcome.output
    );
    assert!(outcome.output.contains("mock soap"), "{}", outcome.output);
    assert!(
        outcome.output.contains("SOAP ENVELOPE SENT"),
        "the envelope is still shown"
    );
}

#[test]
fn the_two_tabs_share_one_implementation_and_differ_in_name() {
    // §4.11.
    let queue = SoapSenderPlugin::queue();
    let sub = SoapSenderPlugin::subscription();
    assert_eq!(queue.id(), "soap-sender");
    assert_eq!(sub.id(), "soap-subscription-sender");
    assert_eq!(queue.samples(), sub.samples());
    assert_ne!(queue.input_label(), sub.input_label());
}
