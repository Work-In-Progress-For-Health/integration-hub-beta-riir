//! A real listener, a real socket, the real sample messages.
//!
//! Implements `spec/07-hl7-server.md` §9.4, §9.5 and §9.6.
//!
//! Everything below `handle` is exercised here — framing, the size limit,
//! pipelining, UTF-8, the reply bytes — against a `TcpStream` rather than a
//! mock. The bus is the fake one, because what is under test is the socket,
//! not Azure; `tests/emulator.rs` in `ihb-bus` covers the other half.
//!
//! # The sample messages are the ones in production
//!
//! `local/sample_messages/*.hl7` were copied unmodified from the Python
//! repository, and they are what the source systems actually send —
//! including the details that look like mistakes. `phw-to-mpi.sample.hl7`
//! has `MSH-7` as `2025-05-05 23:23:32`, which is not an HL7 timestamp;
//! `spec/06-validation.md` §3.3.2a is about exactly that. A test written
//! against a tidied-up message would prove nothing about this estate.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use hl7_server::config::Config;
use hl7_server::listener::serve;
use hl7_server::pipeline::Pipeline;
use ihb_bus::fake::{FakeBus, FakeSender};
use ihb_runtime::shutdown::Shutdown;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The egress queue.
const EGRESS: &str = "pre-phw-transform";

/// MLLP's start block, `<VT>`.
const VT: u8 = 0x0B;

/// MLLP's end block, `<FS>`.
const FS: u8 = 0x1C;

/// A running server, and what is needed to talk to it and inspect it.
struct Server {
    /// Where it is listening.
    address: std::net::SocketAddr,
    /// What it sent to.
    bus: FakeBus,
    /// Stops the accept loop.
    shutdown: Shutdown,
    /// The serve task, so a test can await the drain of §8.3.
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Server {
    /// Stop accepting, drain, and assert the listener ended cleanly.
    async fn stop(self) {
        self.shutdown.stop();
        self.task
            .await
            .expect("the listener task did not panic")
            .expect("the listener ended cleanly");
    }
}

/// Start a server on an ephemeral port.
async fn start(extra: &[(&str, &str)]) -> Server {
    let mut pairs: Vec<(String, String)> = [
        ("WORKFLOW_ID", "phw-to-mpi"),
        ("MICROSERVICE_ID", "hl7-server-phw"),
        ("HEALTH_BOARD", "PHW"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_QUEUE_NAME", EGRESS),
        ("EGRESS_SESSION_ID", "phw-to-mpi"),
        ("HL7_VALIDATION_FLOW", "phw"),
        ("HL7_VERSION", "2.5"),
        ("SENDING_APP", "252"),
        // Port 0 asks the OS for a free one, so tests do not collide.
        ("PORT", "0"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    for (key, value) in extra {
        pairs.retain(|(name, _)| name != key);
        pairs.push(((*key).to_string(), (*value).to_string()));
    }

    let config = Config::from_pairs(pairs).expect("the test configuration is complete");
    let bus = FakeBus::new();
    let egress = bus.sender(EGRESS);
    let store = bus.sender("messagestore");

    // Bound here rather than inside `serve`, so the test knows the port
    // before the server starts using it.
    let listener = tokio::net::TcpListener::bind(config.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let pipeline: Arc<Pipeline<FakeSender, FakeSender>> =
        Arc::new(Pipeline::new(config, egress, Some(store)));

    let shutdown = Shutdown::new();
    let watcher = shutdown.watcher();
    let task = tokio::spawn(async move { serve(address, pipeline, watcher).await });

    // Wait for the bind rather than sleeping a fixed amount.
    for _ in 0..200 {
        if TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Server {
        address,
        bus,
        shutdown,
        task,
    }
}

/// Wrap a payload in an MLLP frame.
fn frame(payload: &str) -> Vec<u8> {
    let mut bytes = vec![VT];
    bytes.extend_from_slice(payload.as_bytes());
    bytes.extend_from_slice(&[FS, b'\r']);
    bytes
}

/// Read one MLLP frame's payload, or `None` if the peer closed first.
async fn read_frame(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut byte = [0_u8; 1];

    loop {
        // A closed peer, a read error and a timeout all mean the same
        // thing here: no reply is coming. §4.3 makes that a result, not a
        // failure, so they share an arm.
        match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte)).await {
            Ok(Ok(1..)) => buffer.push(byte[0]),
            _ => return None,
        }
        if buffer.len() >= 2 && buffer[buffer.len() - 2] == FS {
            let payload = &buffer[1..buffer.len() - 2];
            return Some(String::from_utf8_lossy(payload).into_owned());
        }
    }
}

/// The PHW sample, as the source system sends it.
fn phw_sample() -> String {
    let raw = include_str!("../../../local/sample_messages/phw-to-mpi.sample.hl7");
    // The file on disk uses `\n`; the wire uses `\r`. `spec/index.md` §0.6.
    raw.trim_end().replace("\r\n", "\n").replace('\n', "\r")
}

#[tokio::test]
async fn the_phw_sample_message_is_acknowledged_and_forwarded() {
    // spec/07-hl7-server.md §9.4 — the whole path, over a real socket.
    let server = start(&[]).await;
    let message = phw_sample();

    let mut stream = TcpStream::connect(server.address).await.unwrap();
    stream.write_all(&frame(&message)).await.unwrap();

    let ack = read_frame(&mut stream)
        .await
        .expect("the sample message is acknowledged");

    // §4 — the addressing is reversed and the control id comes back.
    assert!(ack.starts_with("MSH|^~\\&|100|100|252|252|"), "{ack}");
    assert!(
        ack.contains("|ACK^A31^ACK|202505052323364444444444|P|2.5"),
        "{ack}"
    );
    assert!(ack.ends_with("MSA|AA|202505052323364444444444"), "{ack}");

    // And the message itself reached the queue, unchanged.
    let sent = server.bus.all_sent(EGRESS);
    assert_eq!(sent.len(), 1);
    assert_eq!(String::from_utf8_lossy(&sent[0].body), message);
    assert_eq!(
        sent[0].message_id.as_deref(),
        Some("202505052323364444444444"),
        "§6.3 — the message id is MSH-10"
    );

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn two_frames_in_one_write_receive_two_acknowledgements() {
    // spec/07-hl7-server.md §2.5 and §9.6 — where this deliberately differs
    // from the Python, which read until its buffer *ended* with <FS><CR> and
    // answered two frames once. See plan.md §9.1.
    let server = start(&[]).await;

    let first = phw_sample();
    let second = first.replace("202505052323364444444444", "SECOND0000000000000000");

    let mut both = frame(&first);
    both.extend_from_slice(&frame(&second));

    let mut stream = TcpStream::connect(server.address).await.unwrap();
    stream.write_all(&both).await.unwrap();

    let one = read_frame(&mut stream)
        .await
        .expect("the first is answered");
    let two = read_frame(&mut stream)
        .await
        .expect("the second is answered");

    assert!(one.contains("MSA|AA|202505052323364444444444"), "{one}");
    assert!(two.contains("MSA|AA|SECOND0000000000000000"), "{two}");

    assert_eq!(server.bus.all_sent(EGRESS).len(), 2, "both were forwarded");

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn an_oversized_frame_gets_a_generic_ae_and_the_connection_closes() {
    // spec/07-hl7-server.md §2.3 and §9.5.
    let server = start(&[("MAX_MESSAGE_SIZE_BYTES", "2048")]).await;

    let mut stream = TcpStream::connect(server.address).await.unwrap();

    // Deliberately larger than the limit and never terminated, so the
    // failure is the size rather than anything about the content.
    let mut oversized = vec![VT];
    oversized.extend(std::iter::repeat_n(b'A', 4096));
    stream.write_all(&oversized).await.unwrap();

    let nack = read_frame(&mut stream)
        .await
        .expect("§4.3.2 — an oversized message gets a generic AE");
    assert!(nack.contains("|ACK^^ACK|"), "{nack}");
    assert!(nack.contains("MSA|AE|"), "{nack}");
    assert!(nack.contains("exceeds maximum allowed size"), "{nack}");
    assert!(
        server.bus.all_sent(EGRESS).is_empty(),
        "and is not forwarded"
    );

    server.stop().await;
}

#[tokio::test]
async fn a_connection_that_does_not_start_with_a_start_block_is_dropped() {
    // spec/07-hl7-server.md §2.4 — closed without a reply and, notably,
    // without an event: this is a port scan, not a failed message.
    let server = start(&[]).await;

    let mut stream = TcpStream::connect(server.address).await.unwrap();
    stream.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();

    assert!(read_frame(&mut stream).await.is_none(), "no reply");
    assert_eq!(server.bus.all_sent(EGRESS), Vec::new());

    server.stop().await;
}

#[tokio::test]
async fn a_frame_that_is_not_utf8_is_refused() {
    // spec/07-hl7-server.md §2.2 — invalid UTF-8 is a rejection and the
    // connection closes. `spec/index.md` §0.6 forbids forwarding it.
    let server = start(&[]).await;

    let mut stream = TcpStream::connect(server.address).await.unwrap();
    let mut bad = vec![VT];
    bad.extend_from_slice(b"MSH|^~\\&|252|");
    bad.push(0xFF); // never valid in UTF-8
    bad.extend_from_slice(&[FS, b'\r']);
    stream.write_all(&bad).await.unwrap();

    let nack = read_frame(&mut stream)
        .await
        .expect("§4.3.2 — a frame that is not UTF-8 gets a generic AE");
    assert!(nack.contains("MSA|AE|"), "{nack}");
    assert_eq!(server.bus.all_sent(EGRESS), Vec::new());

    server.stop().await;
}

#[tokio::test]
async fn a_message_split_across_writes_is_reassembled() {
    // Not a numbered clause, but the reason `Framer` is fed incrementally:
    // TCP does not preserve write boundaries, and a large HL7 message
    // routinely arrives in several segments. A server that assumed one read
    // per message would work in testing and fail on the real network.
    let server = start(&[]).await;
    let message = phw_sample();
    let framed = frame(&message);

    let mut stream = TcpStream::connect(server.address).await.unwrap();
    for piece in framed.chunks(17) {
        stream.write_all(piece).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    let ack = read_frame(&mut stream)
        .await
        .expect("reassembled and answered");
    assert!(ack.contains("MSA|AA|202505052323364444444444"), "{ack}");

    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn a_rejected_message_gets_an_ar_echoing_its_header_and_the_connection_closes() {
    // spec/07-hl7-server.md §4.3 and §4.3.1, over the socket rather than in
    // the pipeline: the NACK has to survive all the way out to the peer,
    // because that is how the source system learns not to retry.
    let server = start(&[]).await;

    let wrong_version = phw_sample().replace("|P|2.5|", "|P|2.4|");

    let mut stream = TcpStream::connect(server.address).await.unwrap();
    stream.write_all(&frame(&wrong_version)).await.unwrap();

    let nack = read_frame(&mut stream).await.expect("an AR");
    assert!(nack.contains("MSA|AR|202505052323364444444444|"), "{nack}");
    assert!(nack.contains("Message has wrong version"), "{nack}");
    assert!(
        read_frame(&mut stream).await.is_none(),
        "and then the connection closes"
    );
    assert_eq!(server.bus.all_sent(EGRESS), Vec::new());

    server.stop().await;
}

#[tokio::test]
async fn shutdown_stops_accepting_and_drains() {
    // spec/07-hl7-server.md §8.3 — in-flight messages complete. The
    // ordering here is what stops shutdown producing the one failure §5.3
    // forbids.
    let server = start(&[]).await;
    let mut stream = TcpStream::connect(server.address).await.unwrap();
    stream.write_all(&frame(&phw_sample())).await.unwrap();

    let ack = read_frame(&mut stream)
        .await
        .expect("answered before shutdown");
    assert!(ack.contains("MSA|AA|"), "{ack}");

    let address = server.address;
    server.stop().await;

    // Once `stop` has returned the listener is gone, so a new connection
    // must not be accepted into a server that is no longer draining.
    let after = TcpStream::connect(address).await;
    if let Ok(mut stream) = after {
        stream.write_all(&frame(&phw_sample())).await.ok();
        assert!(
            read_frame(&mut stream).await.is_none(),
            "a message arriving after shutdown must not be acknowledged"
        );
    }
}

#[tokio::test]
async fn a_message_at_exactly_the_limit_is_still_accepted() {
    // spec/07-hl7-server.md §2.3 — "would exceed", so the limit itself is
    // allowed. An off-by-one here rejects the largest legitimate message a
    // flow can send, which is the hardest kind of bug to see in testing
    // because it only bites the outliers.
    let message = phw_sample();
    let framed_size = message.len() + 3; // <VT> … <FS><CR>

    let server = start(&[("MAX_MESSAGE_SIZE_BYTES", &framed_size.to_string())]).await;

    let mut stream = TcpStream::connect(server.address).await.unwrap();
    stream.write_all(&frame(&message)).await.unwrap();

    let ack = read_frame(&mut stream)
        .await
        .expect("a message exactly at the limit is accepted");
    assert!(ack.contains("MSA|AA|"), "{ack}");

    drop(stream);
    server.stop().await;
}
