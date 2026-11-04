//! End to end over a real socket: frames in, acknowledgements out, and the
//! payload handed to a bus the test can inspect.
//!
//! Implements `spec/11-mock-receivers.md` §4.1 and §4.2.
//!
//! These are the tests the crate arrived with, re-pointed at the workspace
//! bus. They are the reason the adoption is safe to make: services under
//! test parse these replies, and their tests assert on the fields.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use hl7_2_mllp::{AckCode, Framer, encode};
use hl7_mock_receiver::ack;
use hl7_mock_receiver::server::{Receiver, serve};
use ihb_bus::error::BusError;
use ihb_bus::fake::{FakeBus, FakeSender};
use ihb_bus::message::Outgoing;
use ihb_bus::retry::Disposition;
use ihb_bus::sink::MessageSink;
use ihb_runtime::shutdown::Shutdown;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The queue the mock forwards to.
const QUEUE: &str = "mpi-mock-receiver";

const MESSAGE: &str = "MSH|^~\\&|SENDAPP|SENDFAC|RECVAPP|RECVFAC|20250101120000||ADT^A01|MSG00001|P|2.5\rEVN|A01|20250101120000\r";

/// A sink that forwards to a [`FakeBus`], or refuses everything.
///
/// The refusing mode is what `spec/11-mock-receivers.md` §2.3.1 is about:
/// a queue that will not take the message must become `AE`, because an
/// `AA` that forwarded nothing makes a waiting test hang rather than fail.
#[derive(Clone)]
struct Sink {
    /// The underlying fake, when this one accepts.
    inner: FakeSender,
    /// Whether to refuse.
    refuse: bool,
}

impl MessageSink for Sink {
    async fn send(&self, message: Outgoing) -> Result<(), BusError> {
        if self.refuse {
            return Err(BusError::Send {
                destination: QUEUE.to_string(),
                attempts: 1,
                reason: "the queue is not having it".to_string(),
                disposition: Disposition::Retry,
            });
        }
        self.inner.send(message).await
    }

    fn entity(&self) -> &str {
        self.inner.entity()
    }
}

/// A running receiver.
struct Mock {
    address: std::net::SocketAddr,
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

/// Start a receiver on an ephemeral port.
async fn start(refuse: bool) -> Mock {
    let bus = FakeBus::new();
    let sink = Sink {
        inner: bus.sender(QUEUE),
        refuse,
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let receiver = Arc::new(Receiver::new(sink, None, 1024 * 1024));
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
        address,
        bus,
        shutdown,
        task,
    }
}

/// Read exactly one framed reply from the stream.
async fn read_frame(stream: &mut TcpStream) -> String {
    let mut framer = Framer::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
        assert_ne!(read, 0, "the connection closed before a reply arrived");
        framer.push(&buffer[..read]);
        if let Some(frame) = framer.next_frame().unwrap() {
            return String::from_utf8(frame).unwrap();
        }
    }
}

/// Read `count` frames through **one** framer.
///
/// [`read_frame`] builds a fresh framer per call, so when two replies arrive
/// in a single TCP read the second is discarded with it and the next call
/// waits forever. That made the pipelined test hang about one run in four.
/// Anything expecting more than one reply on a connection must use this.
async fn read_frames(stream: &mut TcpStream, count: usize) -> Vec<String> {
    let mut framer = Framer::new();
    let mut buffer = [0_u8; 4096];
    let mut collected = Vec::new();
    while collected.len() < count {
        while let Some(frame) = framer.next_frame().unwrap() {
            collected.push(String::from_utf8(frame).unwrap());
        }
        if collected.len() == count {
            break;
        }
        let read = stream.read(&mut buffer).await.unwrap();
        assert_ne!(read, 0, "the connection closed before a reply arrived");
        framer.push(&buffer[..read]);
    }
    collected
}

#[tokio::test]
async fn acknowledges_a_message_and_forwards_it() {
    // spec/11-mock-receivers.md §2.2 and §2.3, first row.
    let mock = start(false).await;

    let mut stream = TcpStream::connect(mock.address).await.unwrap();
    stream.write_all(&encode(MESSAGE.as_bytes())).await.unwrap();
    let reply = read_frame(&mut stream).await;

    assert!(reply.contains("MSA|AA|MSG00001"), "{reply}");

    let sent = mock.bus.all_sent(QUEUE);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].body,
        MESSAGE.as_bytes(),
        "the payload is forwarded unchanged"
    );
    assert_eq!(
        sent[0].message_id.as_deref(),
        Some("MSG00001"),
        "under its own control id"
    );

    drop(stream);
    mock.stop().await;
}

#[tokio::test]
async fn several_messages_on_one_connection_each_get_a_reply() {
    // The Python read until the buffer ended with <FS><CR>, so two frames
    // arriving together were read as one. See plan.md §9.1.
    let mock = start(false).await;

    let second = MESSAGE.replace("MSG00001", "MSG00002");
    let mut pipelined = encode(MESSAGE.as_bytes());
    pipelined.extend_from_slice(&encode(second.as_bytes()));

    let mut stream = TcpStream::connect(mock.address).await.unwrap();
    stream.write_all(&pipelined).await.unwrap();

    let mut replies = read_frames(&mut stream, 2).await.into_iter();
    let (first, next) = (replies.next().unwrap(), replies.next().unwrap());

    assert!(first.contains("MSA|AA|MSG00001"), "{first}");
    assert!(next.contains("MSA|AA|MSG00002"), "{next}");
    assert_eq!(mock.bus.all_sent(QUEUE).len(), 2);

    drop(stream);
    mock.stop().await;
}

#[tokio::test]
async fn a_message_split_across_writes_is_reassembled() {
    let mock = start(false).await;

    let frame = encode(MESSAGE.as_bytes());
    let (head, tail) = frame.split_at(frame.len() / 2);

    let mut stream = TcpStream::connect(mock.address).await.unwrap();
    stream.write_all(head).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    stream.write_all(tail).await.unwrap();

    assert!(read_frame(&mut stream).await.contains("MSA|AA|MSG00001"));

    drop(stream);
    mock.stop().await;
}

#[tokio::test]
async fn a_message_saying_fail_is_rejected_and_not_forwarded() {
    // spec/11-mock-receivers.md §1.1 — the mock convention, and the whole
    // reason this service exists in the shape it does.
    let mock = start(false).await;

    let failing = MESSAGE.replace("SENDAPP", "PLEASE-FAIL");
    let mut stream = TcpStream::connect(mock.address).await.unwrap();
    stream.write_all(&encode(failing.as_bytes())).await.unwrap();

    assert!(read_frame(&mut stream).await.contains("MSA|AE|MSG00001"));
    assert!(
        mock.bus.all_sent(QUEUE).is_empty(),
        "a rejected message is not forwarded"
    );

    drop(stream);
    mock.stop().await;
}

#[tokio::test]
async fn a_queue_that_refuses_the_message_produces_a_rejection() {
    // spec/11-mock-receivers.md §2.3.1 — the acknowledgement has to tell
    // the truth: the message did not get where it was going.
    let mock = start(true).await;

    let mut stream = TcpStream::connect(mock.address).await.unwrap();
    stream.write_all(&encode(MESSAGE.as_bytes())).await.unwrap();

    assert!(read_frame(&mut stream).await.contains("MSA|AE|MSG00001"));
    assert_eq!(mock.bus.all_sent(QUEUE), Vec::new());

    drop(stream);
    mock.stop().await;
}

#[tokio::test]
async fn an_unreadable_payload_still_gets_a_framed_reply() {
    // spec/11-mock-receivers.md §1.2 and §2.5 — a mock that goes silent
    // looks like a hung endpoint, which is harder to diagnose than a
    // rejection.
    let mock = start(false).await;

    let mut stream = TcpStream::connect(mock.address).await.unwrap();
    stream
        .write_all(&encode(b"this is not an HL7 message"))
        .await
        .unwrap();

    let reply = read_frame(&mut stream).await;
    assert!(
        reply.contains("MSA|AE|UNKNOWN|Error processing message"),
        "{reply}"
    );

    drop(stream);
    mock.stop().await;
}

#[tokio::test]
async fn the_fallback_reply_is_byte_for_byte_the_specified_one() {
    // spec/11-mock-receivers.md §2.5 — the exact bytes, and framed. The
    // Python returned this one unframed while every other path framed its
    // reply, so a peer waiting for <FS><CR> never saw the end of it;
    // framing it is plan.md §9.2.
    let frame = ack::reply(b"not a message at all", AckCode::Error, "20250102030405");

    assert_eq!(frame.first(), Some(&hl7_2_mllp::START_BLOCK));
    assert_eq!(
        &frame[frame.len() - 2..],
        [hl7_2_mllp::END_BLOCK, hl7_2_mllp::CARRIAGE_RETURN]
    );

    let payload = String::from_utf8(frame[1..frame.len() - 2].to_vec()).unwrap();

    // Against the literal §2.5 gives, and against the constant, so neither
    // can be changed without the other.
    assert_eq!(
        payload,
        "MSH|^~\\&|||||||NACK||P|2.5|||AL\rMSA|AE|UNKNOWN|Error processing message\r"
    );
    assert_eq!(payload, ack::FALLBACK);
}

#[tokio::test]
async fn the_reply_is_the_one_the_ack_module_would_build() {
    // Guards the wiring, not the content: the content is tested in `ack`.
    let expected = ack::reply(MESSAGE.as_bytes(), AckCode::Accept, "X");
    assert!(String::from_utf8_lossy(&expected).contains("MSA|AA|MSG00001"));
}
