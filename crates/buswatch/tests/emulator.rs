//! The live reader, against a real broker.
//!
//! Implements `spec/14-buswatch.md` §8.4's other half.
//!
//! The route tests of `routes.rs` use a fake, because the unreachable-queue
//! case cannot be arranged on a running emulator. What a fake cannot cover
//! is the part of BusWatch that talks AMQP:
//!
//! 1. **A peek does not consume** (§1.1, §4.3). If it did, opening BusWatch
//!    would take messages away from the service that was about to process
//!    them — the single worst thing a read-only tool could do.
//! 2. **A session-enabled queue is readable at all** (§4.4). Seven of the
//!    nine queues in the local stack require a session, and a plain
//!    receiver on one of them fails rather than reading nothing.
//! 3. **Clearing really empties a queue** (§3).
//!
//! # Running them
//!
//! ```sh
//! podman compose -f local/docker-compose.yml up -d
//! cargo test -p buswatch --features emulator --test emulator
//! ```

#![cfg(feature = "emulator")]
// An integration test is its own crate, so the crate-level allows do not
// reach it. See spec/17-testing-and-quality.md §2.3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Every `azservicebus` future is about 32 KB; `ihb_bus::live` records why
// that is allowed rather than boxed away.
#![allow(clippy::large_futures)]

use std::collections::BTreeSet;

use buswatch::live::LiveReader;
use buswatch::reader::QueueReader;
use ihb_bus::message::Outgoing;
use ihb_bus::{Endpoint, LiveBus};

/// The emulator's own connection string, as it prints on startup.
const EMULATOR: &str = "Endpoint=sb://127.0.0.1;\
     SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=SAS_KEY_VALUE;\
     UseDevelopmentEmulator=true;";

/// A queue with no session (`spec/01-architecture.md` §4.1).
const PLAIN_QUEUE: &str = "local-inthub-mpi-mock-receiver";

/// A session-enabled queue, for §4.4.
const SESSION_QUEUE: &str = "local-inthub-phw-transformer-ingress";

/// The session on it.
const SESSION: &str = "phw-to-mpi";

/// Serialises the tests that **consume from or seed** [`PLAIN_QUEUE`].
///
/// A non-session queue gives any message to any receiver, so two tests
/// draining it concurrently take each other's messages — and a `clear`
/// that another test has already emptied reports zero. This is the same
/// defect as `ihb-bus`'s emulator tests had, recorded as
/// `spec/17-testing-and-quality.md` §9.2.1: unlike the session case there
/// is no id to vary, so the queue itself is the unit of isolation.
///
/// It hid here because these tests were first run with `--test-threads=1`.
/// The full gate runs them in parallel, which is what found it.
static PLAIN_QUEUE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The emulator configuration, so the reader knows which queues are which.
const EMULATOR_CONFIG: &str = include_str!("../../../local/ServiceBusEmulatorConfig.json");

fn endpoint() -> Endpoint {
    Endpoint::from_config(Some(EMULATOR), None).expect("the emulator string parses")
}

/// A reader with §4.4's session queues discovered.
async fn reader() -> LiveReader {
    LiveReader::connect(&endpoint(), sessions())
        .await
        .expect("the emulator is reachable; see this file's documentation")
}

fn sessions() -> BTreeSet<String> {
    buswatch::config::discover_session_queues(EMULATOR_CONFIG)
}

/// Put `count` messages on a queue, each carrying a marker.
async fn seed(queue: &str, session: Option<&str>, marker: &str, count: usize) {
    let bus = LiveBus::connect(&endpoint()).await.expect("connected");
    let sender = bus.sender(queue).await.expect("a sender");

    for index in 0..count {
        let body = format!("MSH|^~\\&|252|252|100|100|20260821||ADT^A31|{marker}-{index}|P|2.5");
        let mut message = Outgoing::new(body);
        message
            .properties
            .insert("CorrelationId".to_string(), format!("{marker}-{index}"));
        if let Some(session) = session {
            message.session_id = Some(session.to_string());
        }
        sender.send(message).await.expect("the send works");
    }
}

/// Empty a queue, so a test starts from a known state.
async fn drain(queue: &str) {
    let reader = reader().await;
    reader.clear(queue).await.expect("the clear works");
    reader.close().await.expect("closed");
}

/// A marker unique to this run, so tests do not read each other's messages.
fn marker(label: &str) -> String {
    format!(
        "BW{label}{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos() % 1_000_000)
            .unwrap_or_default()
    )
}

#[tokio::test]
async fn a_peek_does_not_consume() {
    let _turn = PLAIN_QUEUE_LOCK.lock().await;
    // §1.1 and §4.3 — THE test of this file. BusWatch peeks; it does not
    // consume. If it did, opening the page would take messages away from
    // the service that was about to process them, and a developer looking
    // at why a message had not arrived would be the reason it never did.
    drain(PLAIN_QUEUE).await;
    let marker = marker("peek");
    seed(PLAIN_QUEUE, None, &marker, 3).await;

    let reader = reader().await;

    let first = reader.snapshot(PLAIN_QUEUE, 25).await;
    assert!(first.error.is_none(), "{:?}", first.error);
    let mine: Vec<_> = first
        .messages
        .iter()
        .filter(|message| message.preview.contains(&marker))
        .collect();
    assert_eq!(mine.len(), 3, "seeded 3, peeked {}", mine.len());

    // Again, and the same three are still there.
    let second = reader.snapshot(PLAIN_QUEUE, 25).await;
    let again = second
        .messages
        .iter()
        .filter(|message| message.preview.contains(&marker))
        .count();
    assert_eq!(again, 3, "the peek consumed messages");

    reader.close().await.expect("closed");
}

#[tokio::test]
async fn a_session_enabled_queue_is_readable() {
    // §4.4 — seven of the nine queues in the local stack require a session,
    // so a BusWatch that could not read them would be a BusWatch that
    // showed two queues.
    let marker = marker("sess");
    seed(SESSION_QUEUE, Some(SESSION), &marker, 2).await;

    let reader = reader().await;
    let snapshot = reader.snapshot(SESSION_QUEUE, 25).await;

    assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
    assert!(
        snapshot
            .messages
            .iter()
            .any(|message| message.preview.contains(&marker)),
        "seeded messages are not on the page: {:?}",
        snapshot.messages
    );

    // §4.2 — the session id is on the summary, which on a session queue is
    // the one field that says which flow a message belongs to.
    assert!(
        snapshot
            .messages
            .iter()
            .any(|message| message.session_id.as_deref() == Some(SESSION)),
        "no session id on any summary"
    );

    reader.close().await.expect("closed");
    drain(SESSION_QUEUE).await;
}

#[tokio::test]
async fn the_detail_view_finds_a_message_by_sequence_number() {
    let _turn = PLAIN_QUEUE_LOCK.lock().await;
    // §5.1 — the bounded scan, against a broker that assigns the sequence
    // numbers rather than a fake that invents them.
    drain(PLAIN_QUEUE).await;
    let marker = marker("detail");
    seed(PLAIN_QUEUE, None, &marker, 3).await;

    let reader = reader().await;
    let snapshot = reader.snapshot(PLAIN_QUEUE, 25).await;

    let wanted = snapshot
        .messages
        .iter()
        .find(|message| message.preview.contains(&format!("{marker}-1")))
        .expect("the seeded message is on the queue");

    let detail = reader
        .detail(PLAIN_QUEUE, wanted.sequence, 250)
        .await
        .expect("the message is within the limit");

    assert_eq!(detail.sequence, wanted.sequence);
    assert!(
        detail.body.contains(&format!("{marker}-1")),
        "{}",
        detail.body
    );

    // §5.3 — the application properties a service set, and the broker's own.
    assert!(
        detail
            .properties
            .iter()
            .any(|(name, value)| name == "CorrelationId" && value == &format!("{marker}-1")),
        "{:?}",
        detail.properties
    );
    assert!(
        detail
            .broker_properties
            .iter()
            .any(|(name, _)| name == "SequenceNumber"),
        "{:?}",
        detail.broker_properties
    );

    reader.close().await.expect("closed");
}

#[tokio::test]
async fn a_sequence_number_that_is_not_there_is_not_found() {
    let _turn = PLAIN_QUEUE_LOCK.lock().await;
    // §5.2's precondition: the reader says `None`, and the route turns that
    // into a 404 naming the limit.
    let reader = reader().await;

    let found = reader.detail(PLAIN_QUEUE, i64::MAX, 250).await;

    assert!(found.is_none());
    reader.close().await.expect("closed");
}

#[tokio::test]
async fn clearing_really_empties_a_queue() {
    let _turn = PLAIN_QUEUE_LOCK.lock().await;
    // §3, against a broker rather than a `Vec::clear`.
    let marker = marker("clear");
    seed(PLAIN_QUEUE, None, &marker, 5).await;

    let reader = reader().await;

    let cleared = reader.clear(PLAIN_QUEUE).await.expect("the clear works");
    assert!(cleared >= 5, "cleared {cleared}");

    let after = reader.snapshot(PLAIN_QUEUE, 25).await;
    assert!(after.error.is_none(), "{:?}", after.error);
    assert!(
        after.messages.is_empty(),
        "{} messages survived the clear",
        after.messages.len()
    );

    reader.close().await.expect("closed");
}

#[tokio::test]
async fn a_queue_that_does_not_exist_is_an_error_rather_than_a_panic() {
    // §6.2 — the page renders, with this queue carrying its error. The
    // fake covers the rendering; this covers what a real broker says.
    let reader = reader().await;

    let snapshot = reader.snapshot("local-inthub-no-such-queue", 25).await;

    assert!(snapshot.error.is_some(), "a missing queue read as fine");
    assert!(snapshot.messages.is_empty());
    assert_eq!(snapshot.queue, "local-inthub-no-such-queue");

    reader.close().await.expect("closed");
}

#[tokio::test]
async fn a_peek_is_bounded_by_the_peek_count() {
    let _turn = PLAIN_QUEUE_LOCK.lock().await;
    // §4.1 — `BUSWATCH_PEEK_COUNT` messages, not the queue. A queue with
    // ten thousand messages on it must not become a ten-thousand-row page.
    drain(PLAIN_QUEUE).await;
    let marker = marker("bound");
    seed(PLAIN_QUEUE, None, &marker, 8).await;

    let reader = reader().await;
    let snapshot = reader.snapshot(PLAIN_QUEUE, 3).await;

    assert_eq!(snapshot.messages.len(), 3, "{:?}", snapshot.messages);

    reader.close().await.expect("closed");
    drain(PLAIN_QUEUE).await;
}
