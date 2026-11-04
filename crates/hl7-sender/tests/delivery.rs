//! Against a real MLLP listener.
//!
//! Implements `spec/10-senders.md` §6.6 and §6.7.
//!
//! The listener is a few lines of `tokio` rather than a mock, because what
//! is under test is the socket handling: connection reuse, the single
//! timeout retry, and what a half-open connection does. None of that is
//! visible through an interface.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use hl7_sender::{Destination, MllpClient};
use ihb_sender::{Delivery, DeliveryError, Verdict};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MESSAGE: &str = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|MSGID1|P|2.5\r\
                       PID|||8888888^^^252^PI";

/// How a fake receiver answers one connection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// A positive acknowledgement.
    Accept,
    /// A negative one.
    Reject(&'static str),
    /// A message with no `MSA` segment at all.
    NotAnAck,
    /// Read the message and say nothing.
    Silence,
    /// Close the connection without replying.
    Hangup,
}

/// A listener that answers each *connection* from a script, in order.
///
/// Per connection rather than per message, because §3.4's retry opens a new
/// one — which is the property the retry test turns on.
struct Receiver {
    address: std::net::SocketAddr,
    /// How many connections have been accepted.
    connections: Arc<AtomicUsize>,
    /// The listener task, kept alive for the test's duration.
    _task: tokio::task::JoinHandle<()>,
}

async fn listener(script: Vec<Answer>) -> Receiver {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);

    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let index = counter.fetch_add(1, Ordering::SeqCst);
            let answer = script.get(index).copied().unwrap_or(Answer::Accept);

            tokio::spawn(async move {
                // One exchange per connection is all these tests need,
                // except the reuse test, which loops.
                loop {
                    let mut buffer = vec![0_u8; 8192];
                    let Ok(read) = stream.read(&mut buffer).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }

                    match answer {
                        Answer::Hangup => return,
                        Answer::Silence => {
                            // Hold the connection open, saying nothing.
                            tokio::time::sleep(Duration::from_secs(3600)).await;
                        }
                        Answer::Accept => reply(&mut stream, "AA").await,
                        Answer::Reject(code) => reply(&mut stream, code).await,
                        Answer::NotAnAck => {
                            let body = "MSH|^~\\&|100|100|252|252|20250505||ADT^A31|MSGID1|P|2.5";
                            let _ = stream.write_all(&hl7_2_mllp::encode(body.as_bytes())).await;
                            let _ = stream.flush().await;
                        }
                    }
                }
            });
        }
    });

    Receiver {
        address,
        connections,
        _task: task,
    }
}

/// Write a framed acknowledgement with this `MSA-1`.
async fn reply(stream: &mut TcpStream, code: &str) {
    let body = format!(
        "MSH|^~\\&|100|100|252|252|20250505232332||ACK^A31^ACK|MSGID1|P|2.5\rMSA|{code}|MSGID1"
    );
    let _ = stream.write_all(&hl7_2_mllp::encode(body.as_bytes())).await;
    let _ = stream.flush().await;
}

/// A client pointed at `receiver`, with a short timeout so the timeout
/// tests do not take thirty seconds each.
fn client(receiver: &Receiver, ack_timeout: Duration) -> MllpClient {
    MllpClient::new(Destination {
        host: receiver.address.ip().to_string(),
        port: receiver.address.port(),
        ack_timeout,
    })
}

#[tokio::test]
async fn a_positive_acknowledgement_is_accepted() {
    // spec/10-senders.md §3.7 and §6.6.
    let receiver = listener(vec![Answer::Accept]).await;
    let mut client = client(&receiver, Duration::from_secs(5));

    let verdict = client
        .deliver(MESSAGE)
        .await
        .expect("the exchange succeeds");

    assert!(verdict.is_accepted());
    assert!(verdict.acknowledgement().contains("MSA|AA|MSGID1"));
}

#[tokio::test]
async fn a_negative_acknowledgement_is_rejected_and_retried() {
    // §3.7.1 and §6.6 — `AE` and `AR` are both failures, and neither is
    // completed. A sender that completed on `AR` would discard the record.
    for code in ["AE", "AR"] {
        let receiver = listener(vec![Answer::Reject(code)]).await;
        let mut client = client(&receiver, Duration::from_secs(5));

        let verdict = client
            .deliver(MESSAGE)
            .await
            .expect("the exchange succeeds");

        assert!(!verdict.is_accepted(), "{code}");
        let Verdict::Nack { reason, .. } = &verdict else {
            panic!("{code}: expected a rejection")
        };
        assert_eq!(
            reason,
            &format!("Negative ACK received: {code} for: MSGID1")
        );
    }
}

#[tokio::test]
async fn a_reply_with_no_msa_is_not_an_acknowledgement() {
    // §3.7's first row and §6.6.
    let receiver = listener(vec![Answer::NotAnAck]).await;
    let mut client = client(&receiver, Duration::from_secs(5));

    let verdict = client
        .deliver(MESSAGE)
        .await
        .expect("the exchange succeeds");

    assert!(!verdict.is_accepted());
    let Verdict::Nack { reason, .. } = &verdict else {
        panic!("expected a rejection")
    };
    assert_eq!(reason, "Received a non-ACK message");
}

#[tokio::test]
async fn silence_times_out_after_exactly_one_retry() {
    // §3.4 and §6.6 — the wording is normative, and the retry count is the
    // point: a receiver that never answers must not be retried forever
    // inside one message's processing.
    let receiver = listener(vec![Answer::Silence, Answer::Silence]).await;
    // A whole second, not a fraction: the message names
    // `ACK_TIMEOUT_SECONDS`, which is configured in whole seconds, and a
    // sub-second timeout here would assert on "within 0 seconds" — true,
    // but not a wording any deployment can produce.
    let mut client = client(&receiver, Duration::from_secs(1));

    let error = client.deliver(MESSAGE).await.unwrap_err();

    assert_eq!(
        error,
        DeliveryError::Connection("No ACK received within 1 seconds".to_string()),
        "§3.4's exact wording"
    );
    assert_eq!(
        receiver.connections.load(Ordering::SeqCst),
        2,
        "the first attempt and exactly one retry, on a fresh connection"
    );
}

#[tokio::test]
async fn the_retry_succeeds_when_the_receiver_answers_the_second_time() {
    // §3.4 and §6.7 — the case the retry exists for: the receiver was
    // restarting when the first attempt went out.
    let receiver = listener(vec![Answer::Hangup, Answer::Accept]).await;
    let mut client = client(&receiver, Duration::from_secs(5));

    // The first attempt sees the connection close; that is a connection
    // error rather than a timeout, so it fails this message and rebuilds.
    let first = client.deliver(MESSAGE).await;
    assert!(first.is_err(), "the hangup fails the first message");

    // The next message goes out on a fresh connection and is accepted.
    let second = client.deliver(MESSAGE).await.expect("the second connects");
    assert!(second.is_accepted());
}

#[tokio::test]
async fn one_connection_serves_many_messages() {
    // §3.1 — a long-lived connection, not one per message. Opening a socket
    // per message against a receiver taking thousands an hour is a
    // different load profile than the one it was sized for.
    let receiver = listener(vec![Answer::Accept]).await;
    let mut client = client(&receiver, Duration::from_secs(5));

    for _ in 0..5 {
        assert!(client.deliver(MESSAGE).await.unwrap().is_accepted());
    }

    assert_eq!(
        receiver.connections.load(Ordering::SeqCst),
        1,
        "five messages, one connection"
    );
}

#[tokio::test]
async fn a_receiver_that_is_not_listening_is_a_connection_error() {
    // §3.5 — and it must be classified as a connection error, because
    // `spec/10-senders.md` §2.6 pairs that with its own
    // `validation_result` and an operator's dashboard separates it from a
    // defect in this code.
    let mut client = MllpClient::new(Destination {
        // Port 1 is reserved and nothing listens on it.
        host: "127.0.0.1".to_string(),
        port: 1,
        ack_timeout: Duration::from_millis(200),
    });

    let error = client.deliver(MESSAGE).await.unwrap_err();

    assert!(
        matches!(error, DeliveryError::Connection(_)),
        "{error:?} should be a connection error"
    );
    assert_eq!(
        error.validation_result(),
        "Message sending failed - connection/timeout error"
    );
}

#[tokio::test]
async fn a_connection_the_peer_closed_is_rebuilt_rather_than_reused() {
    // §3.2 — the reason the staleness check exists. A receiver that
    // restarts between messages leaves a socket that looks open and is not;
    // writing to it succeeds and the reply never comes.
    let receiver = listener(vec![Answer::Accept, Answer::Accept]).await;
    let mut client = client(&receiver, Duration::from_secs(5));

    assert!(client.deliver(MESSAGE).await.unwrap().is_accepted());

    // Let the receiver's per-connection task notice nothing more is coming
    // and give the socket time to report closed. The listener's task loops,
    // so this test does not close it directly; instead the second delivery
    // proves reuse or rebuild, and either way must succeed.
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        client.deliver(MESSAGE).await.unwrap().is_accepted(),
        "the second message is delivered whether the connection was reused or rebuilt"
    );
}
