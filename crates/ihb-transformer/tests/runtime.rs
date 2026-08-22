//! The runtime of §1, against a fake bus.
//!
//! Implements `spec/09-transformers.md` §6.7.
//!
//! Two claims, and both are about things that are invisible when they go
//! wrong:
//!
//! - **The incoming application properties are forwarded unchanged**
//!   (§1.3.1). A transformer that rebuilt them would produce a perfectly
//!   valid message with a fresh correlation id, and the audit trail from
//!   the hospital's socket to MPI would simply stop at this hop. Nothing
//!   would error.
//! - **A failure abandons rather than completes** (§1.7), and takes the
//!   rest of the batch with it. A transformer that completed a message it
//!   could not map would discard it silently.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use hl7_2::Message;
use ihb_bus::fake::{FakeBus, FakeSender};
use ihb_bus::message::{Incoming, Outgoing};
use ihb_bus::metadata::{Properties, Tracking};
use ihb_bus::sink::MessageSink;
use ihb_transformer::config::Config;
use ihb_transformer::{Outcome, Runtime, TransformError, Transformer};

/// The queue transformed messages go to.
const EGRESS: &str = "mpi-sender-ingress";

/// The queue they arrive on.
const INGRESS: &str = "phw-transformer-ingress";

const MESSAGE: &str = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
                       EVN||20250502092900\r\
                       PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M";

/// A transformer that renames the sending application, or refuses.
struct Renamer {
    /// Whether to fail every message.
    refuse: bool,
}

impl Transformer for Renamer {
    fn name(&self) -> &'static str {
        "Test"
    }

    fn transform(&mut self, message: &Message) -> Result<Message, TransformError> {
        if self.refuse {
            return Err(TransformError::field("MSH-3.1", "refusing, for the test"));
        }
        let mut out = message.clone();
        out.set("MSH-3.1", "MAPPED")?;
        Ok(out)
    }
}

fn config() -> Config {
    Config::from_pairs([
        ("WORKFLOW_ID", "phw-to-mpi"),
        ("MICROSERVICE_ID", "hl7-phw-transformer"),
        ("HEALTH_BOARD", "PHW"),
        ("PEER_SERVICE", "MPI"),
        ("INGRESS_QUEUE_NAME", INGRESS),
        ("EGRESS_QUEUE_NAME", EGRESS),
        ("EGRESS_SESSION_ID", "phw-to-mpi"),
    ])
    .expect("the test configuration is complete")
}

/// The tracking metadata an ingress server would have minted.
fn tracking() -> Properties {
    Tracking {
        correlation_id: "minted-at-ingress".into(),
        received_at: "2026-08-21T09:15:00+00:00".into(),
        source_system: "252".into(),
        workflow_id: "phw-to-mpi".into(),
    }
    .to_properties()
}

/// A received message carrying `properties`, via the fake bus so the shape
/// is the one a real receive produces.
async fn received(body: &str, properties: Properties) -> Incoming {
    let bus = FakeBus::new();
    bus.sender(INGRESS)
        .send(
            Outgoing::new(body)
                .with_message_id("M1")
                .with_properties(properties),
        )
        .await
        .unwrap();
    bus.receive(INGRESS, 1).pop().expect("one message")
}

/// A runtime over a fresh fake bus.
fn runtime(refuse: bool) -> (Runtime<Renamer, FakeSender>, FakeBus) {
    let bus = FakeBus::new();
    let runtime = Runtime::new(config(), Renamer { refuse }, bus.sender(EGRESS));
    (runtime, bus)
}

#[tokio::test]
async fn the_incoming_properties_are_forwarded_unchanged() {
    // spec/09-transformers.md §1.3.1 and §6.7. The whole point of the hop
    // preserving them is that one correlation id spans the journey.
    let (mut runtime, bus) = runtime(false);
    let message = received(MESSAGE, tracking()).await;

    let outcomes = runtime.process_batch(std::slice::from_ref(&message)).await;
    assert_eq!(outcomes, vec![Outcome::Handled]);

    let sent = bus.all_sent(EGRESS);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].properties,
        tracking(),
        "forwarded as received, not rebuilt"
    );
    assert_eq!(sent[0].properties["CorrelationId"], "minted-at-ingress");
}

#[tokio::test]
async fn the_transformed_message_is_the_one_forwarded() {
    let (mut runtime, bus) = runtime(false);
    let message = received(MESSAGE, tracking()).await;

    runtime.process_batch(std::slice::from_ref(&message)).await;

    let body = String::from_utf8(bus.all_sent(EGRESS)[0].body.clone()).unwrap();
    assert!(body.starts_with("MSH|^~\\&|MAPPED|"), "{body}");
}

#[tokio::test]
async fn no_message_id_is_set_on_the_way_out() {
    // spec/09-transformers.md §1.3.2 — only ingress sets one. A
    // transformer that set it too would give MPI a message id minted by
    // the wrong hop.
    let (mut runtime, bus) = runtime(false);
    let message = received(MESSAGE, tracking()).await;

    runtime.process_batch(std::slice::from_ref(&message)).await;

    assert_eq!(bus.all_sent(EGRESS)[0].message_id, None);
    assert_eq!(
        bus.all_sent(EGRESS)[0].session_id.as_deref(),
        Some("phw-to-mpi"),
        "but the egress session is set from configuration"
    );
}

#[tokio::test]
async fn a_transform_failure_is_not_handled_and_forwards_nothing() {
    // spec/09-transformers.md §1.6's first row and §6.7.
    let (mut runtime, bus) = runtime(true);
    let message = received(MESSAGE, tracking()).await;

    let outcomes = runtime.process_batch(std::slice::from_ref(&message)).await;

    assert_eq!(outcomes.len(), 1);
    assert!(!outcomes[0].is_handled());
    let Outcome::NotHandled { reason } = &outcomes[0] else {
        panic!("expected a failure")
    };
    assert!(
        reason.starts_with("Failed to transform Test message:"),
        "§1.6's exact wording: {reason}"
    );

    assert!(bus.all_sent(EGRESS).is_empty());
}

#[tokio::test]
async fn a_failure_stops_the_rest_of_the_batch() {
    // spec/09-transformers.md §1.7, and the reason it exists: within a
    // session, order is the contract (spec/01 §6.2). A message that jumped
    // ahead of one that failed would reach MPI out of sequence — an older
    // update overwriting a newer one.
    let (mut runtime, bus) = runtime(true);

    let batch = vec![
        received(MESSAGE, tracking()).await,
        received(MESSAGE, tracking()).await,
        received(MESSAGE, tracking()).await,
    ];

    let outcomes = runtime.process_batch(&batch).await;

    assert_eq!(
        outcomes.len(),
        1,
        "the two behind it are not attempted, so they have no outcome"
    );
    assert!(!outcomes[0].is_handled());
    assert!(bus.all_sent(EGRESS).is_empty());
}

#[tokio::test]
async fn a_good_batch_is_processed_in_order() {
    let (mut runtime, bus) = runtime(false);

    let batch = vec![
        received(&MESSAGE.replace("|M1|", "|A|"), tracking()).await,
        received(&MESSAGE.replace("|M1|", "|B|"), tracking()).await,
        received(&MESSAGE.replace("|M1|", "|C|"), tracking()).await,
    ];

    let outcomes = runtime.process_batch(&batch).await;

    assert_eq!(outcomes.len(), 3);
    assert!(outcomes.iter().all(Outcome::is_handled));

    let ids: Vec<String> = bus
        .all_sent(EGRESS)
        .iter()
        .map(|message| {
            let body = String::from_utf8(message.body.clone()).unwrap();
            ihb_fields::read(&ihb_validation::parse(&body).unwrap(), "MSH-10")
        })
        .collect();

    assert_eq!(ids, ["A", "B", "C"], "order is preserved");
}

#[tokio::test]
async fn a_message_with_no_properties_is_still_processed() {
    // spec/09-transformers.md §1.3 step 3 — the absent case is a warn, not
    // a failure. A message can reach a transformer without properties if it
    // was replayed or hand-injected, and dropping it would be worse.
    let (mut runtime, bus) = runtime(false);
    let message = received(MESSAGE, Properties::new()).await;

    let outcomes = runtime.process_batch(std::slice::from_ref(&message)).await;

    assert_eq!(outcomes, vec![Outcome::Handled]);
    assert!(bus.all_sent(EGRESS)[0].properties.is_empty());
}

#[tokio::test]
async fn a_body_that_is_not_utf8_is_refused() {
    // `spec/index.md` §0.6 forbids forwarding it. The Python decoded with
    // `errors="replace"` and mapped the replacement characters, which sends
    // MPI a corrupted record rather than none.
    let bus = FakeBus::new();
    bus.sender(INGRESS)
        .send(Outgoing::new(vec![0xFF, 0xFE, 0xFD]))
        .await
        .unwrap();
    let message = bus.receive(INGRESS, 1).pop().unwrap();

    let (mut runtime, egress) = runtime(false);
    let outcomes = runtime.process_batch(std::slice::from_ref(&message)).await;

    assert!(!outcomes[0].is_handled());
    assert!(egress.all_sent(EGRESS).is_empty());
}
