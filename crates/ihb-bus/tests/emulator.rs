//! Against a real broker.
//!
//! Implements `spec/03-message-bus.md` §7.3.
//!
//! The fake bus of §7.1 covers what services do with messages. It cannot
//! cover what Service Bus does with them — sessions, lock renewal, delivery
//! counts, subscriptions — and those are where the estate's assumptions
//! live. These tests need a broker.
//!
//! # Running them
//!
//! ```sh
//! podman compose -f local/docker-compose.yml up -d
//! cargo test -p ihb-bus --features emulator --test emulator
//! ```
//!
//! Behind the `emulator` feature so a normal `cargo test` does not fail on a
//! machine with no broker. They **do** run in CI, where one is available
//! (`spec/17-testing-and-quality.md` §9.2) — a test that only ever runs
//! locally is a test that stops running.

#![cfg(feature = "emulator")]
// An integration test is its own crate, so the crate-level allow in
// `src/lib.rs` does not reach it. Asserting is what this file is for.
// See spec/17-testing-and-quality.md §2.3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Every `azservicebus` future is about 32 KB. `src/live.rs` records why that
// is allowed rather than boxed away; a test crate does not inherit a
// module-level allow, so it is repeated here for the same reason.
#![allow(clippy::large_futures)]

use std::time::Duration;

use ihb_bus::message::{Outcome, Outgoing};
use ihb_bus::metadata::Tracking;
use ihb_bus::{Endpoint, LiveBus, LiveReceiver};

/// The emulator's own connection string, as it prints on startup.
const EMULATOR: &str = "Endpoint=sb://127.0.0.1;\
     SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=SAS_KEY_VALUE;\
     UseDevelopmentEmulator=true;";

/// A session-enabled queue from `local/ServiceBusEmulatorConfig.json`.
const SESSION_QUEUE: &str = "local-inthub-phw-transformer-ingress";

/// A queue with no session (`spec/01-architecture.md` §4.1). Only this and
/// `local-inthub-mpi-mock-receiver` are.
const PLAIN_QUEUE: &str = "local-inthub-messagestore";

/// The MPI outbound topic.
const TOPIC: &str = "mpihl7input";

/// The subscription on [`TOPIC`].
const SUBSCRIPTION: &str = "mpihl7input-test-subscription";

/// A session id nobody else in this file uses.
///
/// These tests used to share one session, `phw-to-mpi`. A session-enabled
/// queue hands out **one lock per session at a time**, so two tests naming
/// the same session either fought over the lock or read each other's
/// messages — and `cargo test` runs them in parallel. It failed a different
/// test on each run, which is the worst kind of flake: the one that looks
/// like a real defect somewhere else.
///
/// The session id is arbitrary to the broker, so the fix is to stop sharing
/// it. `label` keeps the failure legible when a test does fail.
fn session(label: &str) -> String {
    format!("emulator-{label}")
}

/// A message id unique to this run.
///
/// These tests share queues, so a **fixed** id is poison: an interrupted
/// run leaves its message behind — abandoned, with a raised delivery
/// count — and the next run finds that one instead of its own. A test
/// asserting "this is the first delivery" then fails with a delivery count
/// of three and no explanation.
///
/// That is not hypothetical. It is what a cancelled CI run does, and
/// `.github/workflows/gate.yml` sets `cancel-in-progress`. See
/// `spec/17-testing-and-quality.md` §9.2.1b.
fn message_id(label: &str) -> String {
    format!(
        "{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
            % 1_000_000_000
    )
}

/// Serialises the tests that **receive** from [`PLAIN_QUEUE`].
///
/// A non-session queue gives any message to any receiver, so two tests
/// draining it concurrently take each other's messages. Unlike the session
/// case there is no id to vary: the queue is the unit of isolation, so the
/// tests that consume from it take turns. Tests on other queues are
/// unaffected and still run in parallel.
static PLAIN_QUEUE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The emulator's AMQP port, checked before anything tries to speak AMQP.
const EMULATOR_PORT: u16 = 5672;

/// Fail immediately, and legibly, when the emulator is not running.
///
/// Without this the AMQP client retries a connection nobody is listening
/// for, and the suite spends **minutes** timing out before reporting a
/// failure that names a test rather than the missing container. The first
/// time that happened the run took eight minutes and blamed
/// `abandoning_redelivers_with_a_raised_delivery_count`, which was
/// entirely healthy — the emulator had been torn down underneath it.
///
/// A TCP connect is the cheapest question that distinguishes the two, so
/// it is asked first. See `spec/17-testing-and-quality.md` §9.2.1e.
fn require_emulator() {
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};

    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, EMULATOR_PORT));
    assert!(
        TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_ok(),
        "nothing is listening on {address}, so the Service Bus emulator is \
         not running. Start it with:\n\n    \
         podman compose -f local/docker-compose.yml --profile bus up -d\n\n\
         This is not a defect in the code under test."
    );
}

/// Connect to the emulator.
async fn bus() -> LiveBus {
    require_emulator();

    let endpoint = Endpoint::from_config(Some(EMULATOR), None).expect("the endpoint parses");
    assert!(
        endpoint.is_plaintext(),
        "the emulator must be reached over plaintext, and only the emulator"
    );

    LiveBus::connect(&endpoint)
        .await
        .expect("the emulator is running; see this file's documentation")
}

/// Accept a session, waiting out a lock held by a previous receiver.
///
/// `spec/03-message-bus.md` §8.1.3: a session lock is not released the
/// instant `dispose` returns, so a test that reuses a session has to wait —
/// which is exactly what a service does on reconnect, and why that error is
/// classified retryable.
async fn accept_session(bus: &LiveBus, queue: &str, session: &str) -> LiveReceiver {
    for attempt in 0..40 {
        match bus.session_receiver(queue, session).await {
            Ok(receiver) => return receiver,
            Err(error) if attempt < 39 => {
                assert!(
                    error.is_retryable(),
                    "a held session must be classified retryable: {error}"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) => panic!("the session never became available: {error}"),
        }
    }
    unreachable!()
}

/// Settle everything, so a run leaves the queue as it found it.
async fn drain(receiver: &LiveReceiver) {
    loop {
        let delivered = receiver.receive(50).await.unwrap();
        if delivered.is_empty() {
            break;
        }
        for message in &delivered {
            receiver.settle(message, Outcome::Accepted).await.unwrap();
        }
    }
}

#[tokio::test]
async fn a_message_survives_a_session_round_trip_with_its_metadata() {
    // spec/03-message-bus.md §4 — the correlation id crossing a hop
    // unchanged is what the whole audit trail rests on.
    let bus = bus().await;

    let tracking = Tracking {
        correlation_id: "emulator-round-trip".into(),
        received_at: "2026-08-20T09:15:00+00:00".into(),
        source_system: "252".into(),
        workflow_id: "phw-to-mpi".into(),
    };

    let session = session("round-trip");
    let round_trip_id = message_id("RT");
    let sender = bus.sender(SESSION_QUEUE).await.unwrap();
    sender
        .send(
            Outgoing::new("MSH|^~\\&|252|252|100|100|20260729||ADT^A28|RT1|P|2.5")
                .with_message_id(round_trip_id.clone())
                .with_session(&session)
                .with_properties(tracking.to_properties()),
        )
        .await
        .unwrap();

    let receiver = accept_session(&bus, SESSION_QUEUE, &session).await;
    let delivered = receiver.receive(10).await.unwrap();

    let message = delivered
        .iter()
        .find(|m| m.message_id.as_deref() == Some(round_trip_id.as_str()))
        .expect("the message came back");

    assert_eq!(message.properties["CorrelationId"], "emulator-round-trip");
    assert_eq!(message.properties["WorkflowID"], "phw-to-mpi");
    assert_eq!(message.session_id.as_deref(), Some(session.as_str()));

    for message in &delivered {
        receiver.settle(message, Outcome::Accepted).await.unwrap();
    }
    receiver.close().await.unwrap();
}

#[tokio::test]
async fn abandoning_redelivers_with_a_raised_delivery_count() {
    // spec/10-senders.md §2.2 depends on this: the store event is emitted on
    // the first attempt only, so a retried message is not stored twice.
    let _turn = PLAIN_QUEUE_LOCK.lock().await;
    let bus = bus().await;

    let id = message_id("AB");
    let sender = bus.sender(PLAIN_QUEUE).await.unwrap();
    sender
        .send(Outgoing::new("abandon-me").with_message_id(id.clone()))
        .await
        .unwrap();

    let receiver = bus.queue_receiver(PLAIN_QUEUE).await.unwrap();

    let first = receiver.receive(10).await.unwrap();
    let target = first
        .iter()
        .find(|m| m.message_id.as_deref() == Some(id.as_str()))
        .expect("received once");
    assert!(
        target.is_first_delivery(),
        "delivery_count {:?}",
        target.delivery_count
    );

    for message in &first {
        let outcome = if message.message_id.as_deref() == Some(id.as_str()) {
            Outcome::Deferred
        } else {
            Outcome::Accepted
        };
        receiver.settle(message, outcome).await.unwrap();
    }

    let second = receiver.receive(10).await.unwrap();
    let again = second
        .iter()
        .find(|m| m.message_id.as_deref() == Some(id.as_str()))
        .expect("redelivered after abandon");

    assert!(
        !again.is_first_delivery(),
        "a redelivered message must not look like a first attempt: {:?}",
        again.delivery_count
    );

    for message in &second {
        receiver.settle(message, Outcome::Accepted).await.unwrap();
    }
    receiver.close().await.unwrap();
}

#[tokio::test]
async fn a_topic_reaches_its_subscription() {
    // spec/01-architecture.md §2.1 — the MPI outbound flow.
    let bus = bus().await;

    let sender = bus.sender(TOPIC).await.unwrap();
    sender
        .send(
            Outgoing::new("to-the-topic")
                .with_message_id("TOP1")
                .with_session("mpi-outbound"),
        )
        .await
        .unwrap();

    let receiver = bus
        .subscription_receiver(TOPIC, SUBSCRIPTION, "mpi-outbound")
        .await
        .expect("the subscription accepts a session");

    assert_eq!(
        receiver.entity(),
        format!("{TOPIC}/{SUBSCRIPTION}"),
        "spec/03-message-bus.md §2.2 — a subscription names itself this way"
    );

    let delivered = receiver.receive(10).await.unwrap();
    assert!(
        delivered
            .iter()
            .any(|m| m.message_id.as_deref() == Some("TOP1"))
    );

    for message in &delivered {
        receiver.settle(message, Outcome::Accepted).await.unwrap();
    }
    receiver.close().await.unwrap();
}

#[tokio::test]
async fn a_session_lock_is_renewed_while_a_batch_is_held() {
    // spec/03-message-bus.md §5.1.3 and §8.1.2 — the renewal `azservicebus`
    // does not provide. This holds the session past a renewal interval and
    // then settles, which is the thing that would fail if renewal were not
    // happening.
    let bus = bus().await;

    let session = session("renewal");
    let sender = bus.sender(SESSION_QUEUE).await.unwrap();
    sender
        .send(
            Outgoing::new("held-a-while")
                .with_message_id("HOLD1")
                .with_session(&session),
        )
        .await
        .unwrap();

    let receiver = accept_session(&bus, SESSION_QUEUE, &session).await;
    let delivered = receiver.receive(10).await.unwrap();
    assert_ne!(delivered.len(), 0);

    let renewer = receiver
        .renew_session_lock()
        .expect("a session receiver has a lock to renew");

    // Longer than one renewal interval, so at least one fires while the
    // "handler" is running. The handler holds no lock, which is what lets
    // renewal happen at all.
    tokio::time::sleep(Duration::from_secs(25)).await;

    // spec/03-message-bus.md §5.1.4 — stop the renewer BEFORE settling and
    // closing, so no renewal is in flight against a receiver being torn
    // down. `close` refuses if the renewer is still holding the receiver,
    // which makes the ordering enforced rather than remembered.
    let ended = renewer.stop().await;
    assert!(
        ihb_bus::live::renewal_was_clean(ended),
        "renewal ended badly: {ended:?}"
    );

    for message in &delivered {
        receiver
            .settle(message, Outcome::Accepted)
            .await
            .expect("the session lock was still held, so settling works");
    }
    receiver.close().await.unwrap();
}

#[tokio::test]
async fn closing_while_the_renewer_still_holds_the_receiver_is_refused() {
    // spec/03-message-bus.md §5.1.4, enforced rather than documented: the
    // ordering the Python spent a nine-line comment explaining is a type
    // error here.
    let bus = bus().await;
    let receiver = accept_session(&bus, SESSION_QUEUE, &session("closing")).await;

    let renewer = receiver.renew_session_lock().unwrap();

    let error = receiver
        .close()
        .await
        .expect_err("closing must be refused while the renewer holds the receiver");
    assert!(error.to_string().contains("stop the lock renewer first"));

    renewer.stop().await;
}

#[tokio::test]
async fn a_batch_send_delivers_every_message() {
    let _turn = PLAIN_QUEUE_LOCK.lock().await;
    // spec/03-message-bus.md §3.3 — batching with auto-split.
    let bus = bus().await;

    let receiver = bus.queue_receiver(PLAIN_QUEUE).await.unwrap();
    drain(&receiver).await;

    let sender = bus.sender(PLAIN_QUEUE).await.unwrap();
    let messages: Vec<Outgoing> = (0..25)
        .map(|i| Outgoing::new(format!("batch-{i}")).with_message_id(format!("B{i}")))
        .collect();

    assert_eq!(sender.send_batch(messages).await.unwrap(), 25);

    let mut seen = 0;
    for _ in 0..5 {
        let delivered = receiver.receive(25).await.unwrap();
        if delivered.is_empty() {
            break;
        }
        seen += delivered.len();
        for message in &delivered {
            receiver.settle(message, Outcome::Accepted).await.unwrap();
        }
        if seen >= 25 {
            break;
        }
    }

    assert_eq!(seen, 25, "every batched message came back");
    receiver.close().await.unwrap();
}

#[tokio::test]
async fn an_empty_queue_returns_nothing_rather_than_failing() {
    // spec/03-message-bus.md §5.1.2 — an empty result is not an error, and a
    // receive loop depends on that being quiet.
    //
    // Against a non-session queue: only `local-inthub-messagestore` and
    // `local-inthub-mpi-mock-receiver` are, per
    // `local/ServiceBusEmulatorConfig.json`.
    //
    // This uses [`PLAIN_QUEUE`] — the other one — because `buswatch`'s
    // emulator tests own `local-inthub-mpi-mock-receiver`, and a lock in
    // *this* binary cannot isolate against *that* one: `cargo test` runs
    // the two concurrently. See spec/17-testing-and-quality.md §9.2.1c.
    let _turn = PLAIN_QUEUE_LOCK.lock().await;
    let bus = bus().await;
    let receiver = bus.queue_receiver(PLAIN_QUEUE).await.unwrap();

    drain(&receiver).await;

    assert_eq!(receiver.receive(10).await.unwrap().len(), 0);
    receiver.close().await.unwrap();
}

#[tokio::test]
async fn a_non_session_receiver_on_a_session_queue_is_refused() {
    // Every pipeline queue requires a session
    // (`spec/01-architecture.md` §2.3.1), so reaching one without a session
    // id is a configuration error. The broker refuses it outright, and this
    // pins that it surfaces as a clear failure rather than an empty receive
    // that would look like an idle queue.
    let bus = bus().await;

    let error = bus
        .queue_receiver(SESSION_QUEUE)
        .await
        .expect_err("a session-required queue refuses a sessionless receiver");

    assert!(
        error.to_string().contains("requires sessions"),
        "the reason should name the cause: {error}"
    );
}
