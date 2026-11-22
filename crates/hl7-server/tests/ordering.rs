//! The order of §5, and what each step's failure does.
//!
//! Implements `spec/07-hl7-server.md` §9.3 and §9.7.
//!
//! §5 says its ordering is normative "because each step's failure mode
//! depends on it". A test that only checks the happy path proves none of
//! that: every ordering produces the same result when nothing fails. So
//! these tests break one step at a time and assert on what survived.
//!
//! The claim they exist to defend is §5.3 — **a message that was not
//! forwarded is never acknowledged**. Everything else here is secondary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use hl7_server::config::Config;
use hl7_server::pipeline::{Mint, Pipeline};
use ihb_bus::error::BusError;
use ihb_bus::fake::FakeBus;
use ihb_bus::message::Outgoing;
use ihb_bus::sink::MessageSink;

/// The egress queue every test here uses.
const EGRESS: &str = "pre-phw-transform";

/// The message-store queue.
const STORE: &str = "messagestore";

/// A valid PHW message: right version, allowed sender, plausible date of
/// birth, a registered message type.
const GOOD: &str = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|MSGID1|P|2.5\r\
                    EVN||20250502092900\r\
                    PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r\
                    PV1||U";

/// Fixed values, so a whole message can be asserted on
/// (`spec/17-testing-and-quality.md` §5.1).
struct FixedMint;

impl Mint for FixedMint {
    fn correlation_id(&self) -> String {
        "fixed-correlation-id".to_string()
    }
    fn received_at(&self) -> String {
        "2026-08-20T09:15:00+00:00".to_string()
    }
    fn ack_timestamp(&self) -> String {
        "20260820091500".to_string()
    }
}

/// A sink that records the order sends arrived in, across both queues.
///
/// The fake bus records *what* was sent to each entity, which is enough for
/// most tests. It cannot answer "did the store send happen before the bus
/// send", because they land in different entities and each keeps its own
/// order. §5.1 is exactly that question, so it needs one shared log.
#[derive(Clone)]
struct Recorder {
    /// The entity this sink stands for.
    entity: String,
    /// Every send, in the order it happened, across all sinks sharing this.
    log: SendLog,
    /// The underlying fake, so the message itself can still be inspected.
    bus: FakeBus,
    /// An error to return instead of sending.
    fails: Option<BusError>,
}

impl Recorder {
    fn new(bus: &FakeBus, entity: &str, log: &SendLog) -> Self {
        Self {
            entity: entity.to_string(),
            log: Arc::clone(log),
            bus: bus.clone(),
            fails: None,
        }
    }

    fn failing(mut self, error: BusError) -> Self {
        self.fails = Some(error);
        self
    }
}

impl MessageSink for Recorder {
    async fn send(&self, message: Outgoing) -> Result<(), BusError> {
        self.log.lock().unwrap().push(self.entity.clone());
        if let Some(error) = self.fails.clone() {
            return Err(error);
        }
        self.bus.sender(&self.entity).send(message).await
    }

    fn entity(&self) -> &str {
        &self.entity
    }
}

/// The PHW deployment.
fn config() -> Config {
    Config::from_pairs([
        ("WORKFLOW_ID", "phw-to-mpi"),
        ("MICROSERVICE_ID", "hl7-server-phw"),
        ("HEALTH_BOARD", "PHW"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_QUEUE_NAME", EGRESS),
        ("EGRESS_SESSION_ID", "phw-to-mpi"),
        ("MESSAGE_STORE_QUEUE_NAME", STORE),
        ("HL7_VERSION", "2.5"),
        ("SENDING_APP", "252"),
        ("HL7_VALIDATION_FLOW", "phw"),
    ])
    .expect("the test configuration is complete")
}

/// The order in which sends happened, across every sink.
type SendLog = Arc<Mutex<Vec<String>>>;

/// A pipeline under test, its bus, and its send log.
type Harness = (Pipeline<Recorder, Recorder, FixedMint>, FakeBus, SendLog);

/// A pipeline over a fresh fake bus, plus the shared order log.
fn pipeline() -> Harness {
    let bus = FakeBus::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let egress = Recorder::new(&bus, EGRESS, &log);
    let store = Recorder::new(&bus, STORE, &log);
    let pipeline = Pipeline::with_mint(config(), egress, Some(store), FixedMint);
    (pipeline, bus, log)
}

#[tokio::test]
async fn a_good_message_is_stored_then_forwarded_then_acknowledged() {
    // spec/07-hl7-server.md §5 — steps 10, 11, 12, in that order.
    let (pipeline, bus, log) = pipeline();

    let ack = pipeline
        .handle(GOOD)
        .await
        .expect("a valid message is accepted");

    assert_eq!(
        *log.lock().unwrap(),
        [STORE, EGRESS],
        "§5.1 — the audit copy is written before the message is handed on"
    );

    assert!(ack.starts_with("MSH|^~\\&|100|100|252|252|20260820091500||ACK^A31^ACK|MSGID1|P|2.5"));
    assert!(ack.ends_with("MSA|AA|MSGID1"));

    let forwarded = bus.all_sent(EGRESS);
    assert_eq!(forwarded.len(), 1);
    assert_eq!(forwarded[0].message_id.as_deref(), Some("MSGID1"), "§6.3");
    assert_eq!(forwarded[0].session_id.as_deref(), Some("phw-to-mpi"));
}

/// A pipeline with no `SENDING_APP` allow-list, so `MSH-3` is free to vary:
/// these tests are about the `SourceSystem` property, not sender validation.
fn pipeline_open_to_any_sender() -> Harness {
    let mut pairs = vec![
        ("WORKFLOW_ID", "phw-to-mpi"),
        ("MICROSERVICE_ID", "hl7-server-phw"),
        ("HEALTH_BOARD", "PHW"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_QUEUE_NAME", EGRESS),
        ("EGRESS_SESSION_ID", "phw-to-mpi"),
        ("MESSAGE_STORE_QUEUE_NAME", STORE),
        ("HL7_VERSION", "2.5"),
        ("HL7_VALIDATION_FLOW", "phw"),
    ];
    pairs.retain(|(name, _)| *name != "SENDING_APP");
    let config = Config::from_pairs(pairs).expect("complete without an allow-list");
    let bus = FakeBus::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let egress = Recorder::new(&bus, EGRESS, &log);
    let store = Recorder::new(&bus, STORE, &log);
    (
        Pipeline::with_mint(config, egress, Some(store), FixedMint),
        bus,
        log,
    )
}

#[tokio::test]
async fn source_system_is_the_first_component_of_a_composite_msh_3() {
    // Upstream 69548eb (#465): `APP^OID^ISO` is not an application name.
    let (pipeline, bus, _) = pipeline_open_to_any_sender();
    let composite = GOOD.replace("|252|252|", "|COMP_APP^1.2.3.4^ISO|252|");

    pipeline.handle(&composite).await.unwrap();

    assert_eq!(
        bus.all_sent(EGRESS)[0].properties["SourceSystem"],
        "COMP_APP"
    );
}

#[tokio::test]
async fn source_system_is_empty_when_msh_3_is_empty() {
    let (pipeline, bus, _) = pipeline_open_to_any_sender();
    let empty = GOOD.replace("|252|252|", "||252|");

    pipeline.handle(&empty).await.unwrap();

    assert_eq!(bus.all_sent(EGRESS)[0].properties["SourceSystem"], "");
}

#[tokio::test]
async fn the_tracking_metadata_of_section_six_rides_on_the_message() {
    // spec/07-hl7-server.md §6.1, via spec/03-message-bus.md §4.
    let (pipeline, bus, _) = pipeline();

    pipeline.handle(GOOD).await.unwrap();

    let properties = &bus.all_sent(EGRESS)[0].properties;
    assert_eq!(properties["CorrelationId"], "fixed-correlation-id");
    assert_eq!(properties["MessageReceivedAt"], "2026-08-20T09:15:00+00:00");
    assert_eq!(properties["SourceSystem"], "252", "MSH-3");
    assert_eq!(
        properties["WorkflowID"], "phw-to-mpi",
        "note the capital ID"
    );
}

#[tokio::test]
async fn a_service_bus_failure_produces_no_acknowledgement() {
    // spec/07-hl7-server.md §5.3, and §9.7. THE test of this file.
    //
    // A source system that receives an AA will never send that message
    // again. If this ever passes while the message was not forwarded, a
    // patient record has been silently destroyed.
    let bus = FakeBus::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let egress = Recorder::new(&bus, EGRESS, &log).failing(BusError::Send {
        destination: EGRESS.to_string(),
        attempts: 3,
        reason: "the broker is unreachable".to_string(),
        disposition: ihb_bus::retry::Disposition::Retry,
    });
    let store = Recorder::new(&bus, STORE, &log);
    let pipeline = Pipeline::with_mint(config(), egress, Some(store), FixedMint);

    let result = pipeline.handle(GOOD).await;

    assert!(result.is_err(), "no acknowledgement may be produced");
    assert!(bus.all_sent(EGRESS).is_empty(), "and nothing was forwarded");

    let error = result.unwrap_err();
    assert!(
        error
            .reason
            .starts_with("Failed to send message MSGID1 to Service Bus:"),
        "§5.3's exact wording: {error}"
    );

    // The audit copy still exists, because step 10 ran first. That is what
    // makes the failure diagnosable rather than invisible.
    assert_eq!(bus.all_sent(STORE).len(), 1, "§5.1");
}

#[tokio::test]
async fn a_message_store_failure_still_produces_an_acknowledgement() {
    // spec/07-hl7-server.md §5.2, and §9.7. The mirror of the test above:
    // the audit copy is worth less than the clinical message, so losing it
    // must not stop the message.
    let bus = FakeBus::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let egress = Recorder::new(&bus, EGRESS, &log);
    let store = Recorder::new(&bus, STORE, &log).failing(BusError::Send {
        destination: STORE.to_string(),
        attempts: 3,
        reason: "the store queue is full".to_string(),
        disposition: ihb_bus::retry::Disposition::Retry,
    });
    let pipeline = Pipeline::with_mint(config(), egress, Some(store), FixedMint);

    let ack = pipeline
        .handle(GOOD)
        .await
        .expect("a store failure is logged, not fatal");

    assert!(ack.ends_with("MSA|AA|MSGID1"));
    assert_eq!(bus.all_sent(EGRESS).len(), 1, "the message went on");
    assert!(bus.all_sent(STORE).is_empty(), "the audit copy was lost");
}

#[tokio::test]
async fn the_store_event_carries_the_raw_and_the_xml() {
    // spec/06-validation.md §8.3 — the XmlPayload comes from the flow
    // validation of §3.4, which for a PHW message is a real rendering.
    let (pipeline, bus, _) = pipeline();

    pipeline.handle(GOOD).await.unwrap();

    let stored = &bus.all_sent(STORE)[0];
    let event = ihb_bus::store::StoreEvent::from_json(
        std::str::from_utf8(&stored.body).expect("the store event is UTF-8"),
    )
    .expect("the store event is the JSON the store service reads");

    assert_eq!(event.raw_payload, GOOD, "in full and unredacted");
    assert_eq!(event.correlation_id, "fixed-correlation-id");
    assert_eq!(event.processing_component, "hl7-server-phw");
    assert_eq!(event.target_system.as_deref(), Some("MPI"));
    assert_eq!(event.session_id, "phw-to-mpi", "so replay can restore it");

    let xml = event.xml_payload.expect("a PHW message renders");
    assert!(xml.contains("ADT_A05"), "{xml}");
    assert!(xml.contains("SURNAME"), "{xml}");
}

#[tokio::test]
async fn a_message_rejected_by_its_flow_schema_is_stored_with_a_null_payload() {
    // spec/07-hl7-server.md §3.4.4 step 4 — and it is worth being precise
    // about why: a message the hub refused is exactly the one an operator
    // will want to read, so it is persisted even though it is not
    // forwarded.
    let (pipeline, bus, log) = pipeline();

    // A registered message type carrying a segment its structure does not
    // allow, so it passes §7 and §3.1–§3.3 and fails only at §3.4.
    let bad = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|MSGID2|P|2.5\r\
               EVN||20250502092900\r\
               PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r\
               ZZZ|not a segment this structure knows";

    let result = pipeline.handle(bad).await;

    assert!(result.is_err(), "the message is refused");
    assert!(bus.all_sent(EGRESS).is_empty(), "and not forwarded");

    assert_eq!(*log.lock().unwrap(), [STORE], "stored, and only stored");

    let stored = &bus.all_sent(STORE)[0];
    let event =
        ihb_bus::store::StoreEvent::from_json(std::str::from_utf8(&stored.body).unwrap()).unwrap();

    assert_eq!(event.raw_payload, bad);
    assert!(
        event.xml_payload.is_none(),
        "§3.4.4 — a null XmlPayload, because there is no valid rendering"
    );
}

#[tokio::test]
async fn every_rejection_path_forwards_nothing_and_acknowledges_nothing() {
    // spec/07-hl7-server.md §4.3 — one table, so a new rejection path
    // cannot be added without deciding what it does here.
    let cases: [(&str, &str); 5] = [
        ("not an HL7 message at all", "§7.2 — unparseable"),
        (
            "MSH|^~\\&|252|252|100|100|20250505232332||ORU^R01^ORU_R01|M1|P|2.5",
            "§7.1 — an unregistered message type",
        ),
        (
            "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.4\r\
             PID|||1||S^F||19990101",
            "§3.1 — the wrong version",
        ),
        (
            "MSH|^~\\&|999|999|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
             PID|||1||S^F||19990101",
            "§3.2 — a sending application not on the list",
        ),
        (
            "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
             PID|||1||S^F||",
            "§3.3.1 — no date of birth",
        ),
    ];

    for (message, why) in cases {
        let (pipeline, bus, _) = pipeline();

        assert!(pipeline.handle(message).await.is_err(), "{why}");
        assert!(bus.all_sent(EGRESS).is_empty(), "{why}: nothing forwarded");
    }
}

#[tokio::test]
async fn a_deployment_with_no_message_store_still_works() {
    // spec/02-configuration.md §3 — MESSAGE_STORE_QUEUE_NAME absent means
    // no store sender, not a broken service. Several flows run this way.
    let bus = FakeBus::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut pairs: Vec<(String, String)> = vec![
        ("WORKFLOW_ID", "phw-to-mpi"),
        ("MICROSERVICE_ID", "hl7-server-phw"),
        ("HEALTH_BOARD", "PHW"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_QUEUE_NAME", EGRESS),
        ("EGRESS_SESSION_ID", "phw-to-mpi"),
        ("HL7_VALIDATION_FLOW", "phw"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    pairs.push(("HL7_VERSION".to_string(), "2.5".to_string()));

    let config = Config::from_pairs(pairs).unwrap();
    let egress = Recorder::new(&bus, EGRESS, &log);
    let pipeline: Pipeline<Recorder, Recorder, FixedMint> =
        Pipeline::with_mint(config, egress, None, FixedMint);

    let ack = pipeline
        .handle(GOOD)
        .await
        .expect("no store is not a failure");

    assert!(ack.ends_with("MSA|AA|MSGID1"));
    assert_eq!(*log.lock().unwrap(), [EGRESS], "only the one send");
}

#[tokio::test]
async fn the_mpi_flow_adds_its_routing_properties() {
    // spec/07-hl7-server.md §6.2, and §2.2.1: the mpi flow also suppresses
    // flow-schema validation, so this doubles as the test that it does.
    let bus = FakeBus::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let config = Config::from_pairs([
        ("WORKFLOW_ID", "mpi-to-topic"),
        ("MICROSERVICE_ID", "hl7-server-mpi"),
        ("HEALTH_BOARD", "MPI"),
        ("PEER_SERVICE", "TOPIC"),
        ("EGRESS_TOPIC_NAME", EGRESS),
        ("EGRESS_SESSION_ID", "mpi-outbound"),
        ("HL7_VALIDATION_FLOW", "mpi"),
    ])
    .unwrap();

    let egress = Recorder::new(&bus, EGRESS, &log);
    let pipeline: Pipeline<Recorder, Recorder, FixedMint> =
        Pipeline::with_mint(config, egress, None, FixedMint);

    let outbound = "MSH|^~\\&|100|100|252|252|20250505232332||ADT^A31^ADT_A05|MPI1|P|2.5\r\
                    PID|1|8888888^^^252^PI|4444444444^^^NHS^NH";

    pipeline
        .handle(outbound)
        .await
        .expect("the MPI flow accepts it");

    let properties = &bus.all_sent(EGRESS)[0].properties;
    assert_eq!(properties["MessageType"], "A31");
    assert_eq!(properties["UpdateSources"], "|252|");
    assert_eq!(properties["AssigningAuthorities"], "|NHS|");
    assert_eq!(properties["CorrelationId"], "fixed-correlation-id");
}

#[tokio::test]
async fn the_mosaiq_flow_publishes_only_its_two_routing_properties() {
    // spec/07-hl7-server.md §6.2 item 3 (upstream 9e797ac): the tracking
    // properties plus SendingApplication and TriggerEvent, and none of the
    // standard set, which a Mosaiq topic filter was not written against.
    let bus = FakeBus::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let config = Config::from_pairs([
        ("WORKFLOW_ID", "mosaiq-to-mpi"),
        ("MICROSERVICE_ID", "hl7-server-mosaiq"),
        ("HEALTH_BOARD", "MOSAIQ"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_TOPIC_NAME", EGRESS),
        ("EGRESS_SESSION_ID", "mosaiq-to-mpi"),
        ("HL7_VALIDATION_FLOW", "mosaiq"),
        ("SENDING_APP", "285"),
        ("HL7_VERSION", "2.5"),
    ])
    .unwrap();

    let egress = Recorder::new(&bus, EGRESS, &log);
    let pipeline: Pipeline<Recorder, Recorder, FixedMint> =
        Pipeline::with_mint(config, egress, None, FixedMint);

    let inbound = "MSH|^~\\&|285|285|100|100|20250505232332||ADT^A28^ADT_A05|MOS1|P|2.5\r\
                   EVN||20250502102000\r\
                   PID|1||123456^^^Hospital^MR||DOE^JOHN||19900101|M";

    pipeline.handle(inbound).await.expect("Mosaiq accepts it");

    let properties = &bus.all_sent(EGRESS)[0].properties;
    assert_eq!(properties["SendingApplication"], "285");
    assert_eq!(properties["TriggerEvent"], "A28");
    assert!(!properties.contains_key("MessageType"));
    assert!(!properties.contains_key("UpdateSources"));
    assert!(!properties.contains_key("AssigningAuthorities"));
}
