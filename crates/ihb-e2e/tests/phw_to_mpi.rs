//! The PHW flow, end to end, over real Service Bus queues.
//!
//! Implements `tasks.md` P5.7.
//!
//! # What this proves that nothing else does
//!
//! Every service has its own tests, and each is thorough. All of them use
//! the fake bus. This one runs the real chain —
//!
//! ```text
//! MLLP socket → hl7-server → local-inthub-phw-transformer-ingress
//!             → hl7-phw-transformer → local-inthub-mpi-sender-ingress
//!             → hl7-sender → MLLP socket → hl7-mock-receiver
//! ```
//!
//! — through the actual Service Bus emulator, with sessions, peek-lock and
//! settlement. Three things can only fail here:
//!
//! 1. **A queue name or session that does not match between two services.**
//!    Each service's own tests use whatever name that test chose.
//! 2. **A property that one hop writes and the next cannot read.** The
//!    correlation id crossing three hops is asserted below, and it is the
//!    whole audit trail.
//! 3. **A message that one service renders and the next cannot parse.**
//!
//! # Running it
//!
//! ```sh
//! podman compose -f local/docker-compose.yml up -d
//! cargo test -p ihb-e2e --features emulator --test phw_to_mpi -- --test-threads=1
//! ```
//!
//! Behind the `emulator` feature so a normal `cargo test` does not fail on a
//! machine with no broker.

#![cfg(feature = "emulator")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Every `azservicebus` future is about 32 KB; `ihb_bus::live` records why
// that is allowed rather than boxed away. This file is the place that
// caveat bites, and `FLOW_STACK` below is the answer.
#![allow(clippy::large_futures)]

use std::sync::Arc;
use std::time::Duration;

use hl7_mock_receiver::server::serve as serve_mock;
use hl7_phw_transformer::Phw;
use hl7_sender::{Destination, MllpClient};
use hl7_server::config::Config as ServerConfig;
use hl7_server::listener::serve as serve_ingress;
use hl7_server::pipeline::Pipeline;
use ihb_bus::connection::Endpoint;
use ihb_bus::live::LiveBus;
use ihb_bus::message::Outcome;
use ihb_runtime::shutdown::Shutdown;
use ihb_sender::{Context, Throttle, process_one};
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;
use ihb_transformer::config::Config as TransformerConfig;
use ihb_transformer::{Outcome as Handled, Runtime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The emulator's own connection string.
const EMULATOR: &str = "Endpoint=sb://127.0.0.1;\
     SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=SAS_KEY_VALUE;\
     UseDevelopmentEmulator=true;";

/// `local/phw-hl7-server.env` — the queue ingress writes to.
const PRE_TRANSFORM: &str = "local-inthub-phw-transformer-ingress";

/// `local/phw-hl7-transformer.env` — the queue the transformer writes to.
const POST_TRANSFORM: &str = "local-inthub-mpi-sender-ingress";

/// The message store, which ingress also writes to.
const STORE: &str = "local-inthub-messagestore";

/// The queue the mock receiver forwards to; non-session, per
/// `spec/01-architecture.md` §4.1.
const MOCK_EGRESS: &str = "local-inthub-mpi-mock-receiver";

/// The PHW flow's session (`spec/01-architecture.md` §2.3.2).
const SESSION: &str = "phw-to-mpi";

/// The PHW sample, as the wire carries it.
fn sample() -> String {
    include_str!("../../../local/sample_messages/phw-to-mpi.sample.hl7")
        .trim_end()
        .replace("\r\n", "\n")
        .replace('\n', "\r")
}

/// A message with a control id unique to this run, so a queue left dirty by
/// a previous run cannot make this one pass.
fn unique_sample(marker: &str) -> String {
    sample().replace("202505052323364444444444", marker)
}

/// Connect to the local emulator, or fail the test saying how to start it.
///
/// Panics rather than skipping: this file is behind the `emulator` feature,
/// so reaching here means the caller asked for the broker, and a silent
/// pass with no broker is the failure mode `AGENTS.md` warns about. The
/// emulator's port opens before its broker answers
/// (`spec/17-testing-and-quality.md` §9.2.1e), so a failure here right
/// after `compose up` may just mean "not ready yet".
async fn bus() -> LiveBus {
    let endpoint = Endpoint::from_config(Some(EMULATOR), None).expect("the endpoint parses");
    LiveBus::connect(&endpoint)
        .await
        .expect("the emulator is running; see this file's documentation")
}

/// Receive from `receiver` until a message carrying `marker` arrives.
///
/// Everything received on the way is settled, so a queue left dirty by an
/// earlier run is cleaned up rather than getting in the way.
///
/// Gives up after three receives (each up to the bus's own sixty-second
/// window on an idle queue, `spec/03-message-bus.md` §5.1.2), panicking
/// with `what` so the failure names the hop that never delivered.
///
/// # Why there is no timeout around the receive
///
/// There was, and it was wrong. `LiveReceiver::receive` blocks until a
/// message arrives or its own window expires, and establishing the AMQP
/// link on a cold queue can take longer than a few seconds. Wrapping it in
/// a shorter `tokio::time::timeout` **cancels** the in-flight receive and
/// starts a new one on the next attempt, so a queue with messages waiting
/// reports empty forever.
///
/// That cost an hour of looking in the wrong place, and it is recorded at
/// `spec/03-message-bus.md` §5.5. The rule: let a receive run to its own
/// timeout.
async fn receive_until(
    receiver: &ihb_bus::live::LiveReceiver,
    marker: &str,
    what: &str,
) -> ihb_bus::message::Incoming {
    for _ in 0..3 {
        let batch = receiver.receive(50).await.expect("receiving works");

        let mut found = None;
        for message in &batch {
            let body = String::from_utf8_lossy(&message.body);
            if body.contains(marker) {
                found = Some(message.clone());
            }
            receiver.settle(message, Outcome::Accepted).await.unwrap();
        }
        if let Some(message) = found {
            return message;
        }
    }
    panic!("timed out waiting for {what}");
}

/// The stack this flow needs.
///
/// Every `azservicebus` call produces a future of roughly 32 KB
/// (`ihb_bus::live`, "On `clippy::large_futures`"), and in a debug build
/// the polling frames are larger again. This test awaits a dozen of them in
/// one linear body — the whole flow, in order, which is the point of it —
/// and that does not fit in the 2 MB a test thread gets by default.
///
/// A real service does not hit this: each of its `azservicebus` calls is
/// one step of a loop, not one frame of a dozen-step function, and the
/// runtime's worker threads are configured by its own `main`. So this is a
/// property of writing the whole flow as a single function, which is the
/// clearest way to write it, rather than a defect in the services.
const FLOW_STACK: usize = 32 * 1024 * 1024;

/// The whole PHW chain in one test; the body is [`flow`], run on a thread
/// with a large stack (see [`FLOW_STACK`]).
#[test]
fn a_phw_message_travels_from_a_socket_to_the_mock_receiver() {
    // Its own thread, with a stack big enough for the whole flow. See
    // `FLOW_STACK`.
    std::thread::Builder::new()
        .stack_size(FLOW_STACK)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime")
                .block_on(flow());
        })
        .expect("a thread")
        .join()
        .expect("the flow did not panic");
}

/// The flow itself.
///
/// Long, and deliberately so. Every step is one hop of the journey, in the
/// order a message makes it, and splitting it into helpers would hide the
/// one thing this test exists to show: what talks to what, and in which
/// order. A reader who wants to know how a PHW message reaches MPI can
/// read this function top to bottom.
#[expect(
    clippy::too_many_lines,
    reason = "the whole flow in one function is what makes it readable"
)]
async fn flow() {
    let bus = bus().await;

    // No draining. Every assertion below looks for this run's own marker
    // and settles whatever else it meets on the way, which is both faster
    // than draining four queues and immune to a message another test left
    // behind.
    let marker = "E2E00000000000000000001";
    let message = unique_sample(marker);

    let shutdown = Shutdown::new();

    // ---- The mock receiver, standing in for MPI. --------------------
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_address = mock_listener.local_addr().unwrap();
    drop(mock_listener);

    let mock_sender = bus.sender(MOCK_EGRESS).await.unwrap();
    let mock = Arc::new(hl7_mock_receiver::server::Receiver::new(
        mock_sender,
        None,
        1024 * 1024,
    ));
    let mock_task = {
        let watcher = shutdown.watcher();
        tokio::spawn(async move { serve_mock(mock_address, mock, watcher).await })
    };

    // ---- The ingress server. ----------------------------------------
    let ingress_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ingress_address = ingress_listener.local_addr().unwrap();
    drop(ingress_listener);

    let server_config = ServerConfig::from_pairs([
        ("WORKFLOW_ID", "phw-to-mpi"),
        ("MICROSERVICE_ID", "phw_hl7_server"),
        ("HEALTH_BOARD", "PHW"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_QUEUE_NAME", PRE_TRANSFORM),
        ("EGRESS_SESSION_ID", SESSION),
        ("MESSAGE_STORE_QUEUE_NAME", STORE),
        ("HL7_VERSION", "2.5"),
        ("SENDING_APP", "252"),
        ("HL7_VALIDATION_FLOW", "phw"),
    ])
    .unwrap();

    let pipeline = Arc::new(Pipeline::new(
        server_config,
        bus.sender(PRE_TRANSFORM).await.unwrap(),
        Some(bus.sender(STORE).await.unwrap()),
    ));
    let ingress_task = {
        let watcher = shutdown.watcher();
        tokio::spawn(async move { serve_ingress(ingress_address, pipeline, watcher).await })
    };

    // ---- Send the message, and read the acknowledgement. ------------
    //
    // Both listeners bind inside their own tasks, so the port is not
    // guaranteed to be accepting the instant `spawn` returns.
    await_listening(ingress_address, "the ingress server").await;
    await_listening(mock_address, "the mock receiver").await;

    let mut stream = TcpStream::connect(ingress_address).await.unwrap();
    stream
        .write_all(&hl7_2_mllp::encode(message.as_bytes()))
        .await
        .unwrap();

    let ack = read_frame(&mut stream).await.expect("ingress acknowledges");
    assert!(ack.contains(&format!("MSA|AA|{marker}")), "{ack}");
    drop(stream);

    // ---- The transformer. -------------------------------------------
    let transformer_config = TransformerConfig::from_pairs([
        ("WORKFLOW_ID", "phw-to-mpi"),
        ("MICROSERVICE_ID", "phw_hl7_transformer"),
        ("HEALTH_BOARD", "PHW"),
        ("PEER_SERVICE", "MPI"),
        ("INGRESS_QUEUE_NAME", PRE_TRANSFORM),
        ("INGRESS_SESSION_ID", SESSION),
        ("EGRESS_QUEUE_NAME", POST_TRANSFORM),
        ("EGRESS_SESSION_ID", "mpi"),
    ])
    .unwrap();

    let mut runtime = Runtime::new(
        transformer_config,
        Phw::default(),
        bus.sender(POST_TRANSFORM).await.unwrap(),
    );

    let pre = accept_session(&bus, PRE_TRANSFORM, SESSION).await;
    let batch = pre.receive(10).await.unwrap();
    assert!(!batch.is_empty(), "the ingress server forwarded nothing");

    // The correlation id minted at ingress — the value the whole audit
    // trail rests on (`spec/03-message-bus.md` §4).
    let correlation_id = batch[0].properties["CorrelationId"].clone();
    assert!(!correlation_id.is_empty());

    let outcomes = runtime.process_batch(&batch).await;
    assert_eq!(outcomes, vec![Handled::Handled; batch.len()]);
    for message in &batch {
        pre.settle(message, Outcome::Accepted).await.unwrap();
    }
    pre.close().await.unwrap();

    // ---- The sender. ------------------------------------------------
    let post = accept_session(&bus, POST_TRANSFORM, "mpi").await;
    let batch = post.receive(10).await.unwrap();
    assert!(!batch.is_empty(), "the transformer forwarded nothing");

    assert_eq!(
        batch[0].properties["CorrelationId"], correlation_id,
        "the correlation id crossed the transformer unchanged"
    );

    let events = EventLogger::new("phw-to-mpi", "mpi_hl7_sender");
    let metrics = Metrics::new("phw-to-mpi", "mpi_hl7_sender", "PHW", "MPI");
    let context: Context<'_, ihb_bus::live::LiveSender> = Context {
        store: None,
        store_session_id: "mpi".to_string(),
        microservice_id: "mpi_hl7_sender".to_string(),
        peer_service: "MPI".to_string(),
        events: &events,
        metrics: &metrics,
    };

    let mut client = MllpClient::new(Destination {
        host: mock_address.ip().to_string(),
        port: mock_address.port(),
        ack_timeout: Duration::from_secs(10),
    });
    let mut throttle = Throttle::per_minute(None).unwrap();

    let handled = process_one(&mut client, &batch[0], &mut throttle, &context).await;
    assert!(
        handled.is_delivered(),
        "the mock receiver acknowledged the message"
    );

    for message in &batch {
        post.settle(message, Outcome::Accepted).await.unwrap();
    }
    post.close().await.unwrap();

    // ---- What reached the far end. ----------------------------------
    let arrived = bus.queue_receiver(MOCK_EGRESS).await.unwrap();
    let delivered = receive_until(&arrived, marker, "the mock receiver's queue").await;
    let final_message = String::from_utf8_lossy(&delivered.body).into_owned();

    // The transformer normalised `MSH-7`; everything else is the message
    // PHW sent. `spec/09-transformers.md` §3.4.
    assert!(
        final_message.contains("|20250505232332|"),
        "the timestamp was normalised on the way: {final_message}"
    );
    assert!(
        final_message.contains(marker),
        "and it is this run's message: {final_message}"
    );
    assert!(
        final_message.contains("MYSURNAME^MYFNAME"),
        "with the patient's details intact: {final_message}"
    );

    arrived.close().await.unwrap();

    // ---- And the audit copy ingress wrote. --------------------------
    let stored = bus.queue_receiver(STORE).await.unwrap();
    let seen = receive_until(&stored, marker, "the store event").await;

    let event = ihb_bus::store::StoreEvent::from_json(std::str::from_utf8(&seen.body).unwrap())
        .expect("the store event is the JSON the store service reads");
    assert_eq!(event.correlation_id, correlation_id, "one id, end to end");
    assert!(event.raw_payload.contains(marker));
    assert!(event.xml_payload.is_some(), "a PHW message renders");

    stored.close().await.unwrap();

    // ---- Stop everything. -------------------------------------------
    //
    // The sender's connection closes first, and that ordering matters:
    // `MllpClient` keeps one connection for its lifetime
    // (`spec/10-senders.md` §3.1), and the mock receiver's shutdown waits
    // for every connection task to finish (`spec/11-mock-receivers.md`
    // §2, the same drain as `spec/07-hl7-server.md` §8.3). Leave the client
    // alive and the mock's read never returns, so its `serve` never does
    // either.
    drop(client);

    shutdown.stop();
    ingress_task.await.unwrap().unwrap();
    mock_task.await.unwrap().unwrap();
}

/// Wait until something is accepting on `address`.
async fn await_listening(address: std::net::SocketAddr, what: &str) {
    for _ in 0..200 {
        if TcpStream::connect(address).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{what} never started listening on {address}");
}

/// Accept a session, waiting out a lock a previous receiver still holds.
async fn accept_session(bus: &LiveBus, queue: &str, session: &str) -> ihb_bus::live::LiveReceiver {
    for _ in 0..40 {
        if let Ok(receiver) = bus.session_receiver(queue, session).await {
            return receiver;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the session {session} on {queue} never became available");
}

/// Read one MLLP frame's payload.
async fn read_frame(stream: &mut TcpStream) -> Option<String> {
    let mut framer = hl7_2_mllp::Framer::new();
    let mut buffer = vec![0_u8; 8192];

    for _ in 0..100 {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .ok()?
            .ok()?;
        if read == 0 {
            return None;
        }
        framer.push(&buffer[..read]);
        if let Ok(Some(frame)) = framer.next_frame() {
            return String::from_utf8(frame).ok();
        }
    }
    None
}
