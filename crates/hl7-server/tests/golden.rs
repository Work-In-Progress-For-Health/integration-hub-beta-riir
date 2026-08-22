//! Every flow's sample message, through the whole server, against the
//! Python's own output.
//!
//! Implements `spec/07-hl7-server.md` §9.8.
//!
//! Two claims, and they are not equally important.
//!
//! **The forwarded bytes are the received bytes.** This is the one that
//! matters clinically: `hl7-server` does not transform, it validates and
//! forwards, so whatever MPI eventually receives must be byte-for-byte what
//! the source system sent. Asserted for all six samples.
//!
//! **The `XmlPayload` carries what the Python's did.** The message store's
//! XML is an audit and diagnostic record. It is compared against the XML the
//! Python actually produced, captured in
//! `ihb-validation/tests/corpus/python-verdicts.json` (`spec/06-validation.md`
//! §7).
//!
//! # Why the comparison is canonical rather than byte-for-byte
//!
//! The two serialise the same content differently and always will:
//!
//! | | Python (`xmlschema`) | Rust (`hl7-2-from-er7-into-xml`) |
//! | --- | --- | --- |
//! | Declaration | none | `<?xml version="1.0" encoding="UTF-8"?>` |
//! | Namespace | `ns0:` prefix on every element | default `xmlns=` |
//! | Whitespace | none | indented, one element per line |
//!
//! None of that changes what the document says, and neither form is more
//! correct. So the comparison reduces both to a sequence of
//! (local name, text) and compares that — which is exactly the information
//! a reader of the audit record is after.
//!
//! It is recorded as a deviation nonetheless
//! (`spec/06-validation.md` §8.4), because a consumer that string-matches
//! on `<ns0:PID.3>` in the stored column would stop finding it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use hl7_server::config::Config;
use hl7_server::pipeline::{Mint, Pipeline};
use ihb_bus::fake::{FakeBus, FakeSender};

/// The egress queue.
const EGRESS: &str = "egress";

/// The message-store queue.
const STORE: &str = "messagestore";

/// The Python's captured behaviour (`spec/06-validation.md` §7).
///
/// Read from `ihb-validation`'s corpus rather than copied, so there is one
/// capture of the Python in the workspace and not two that can disagree.
const CORPUS: &str = include_str!("../../ihb-validation/tests/corpus/python-verdicts.json");

/// Fixed values, so the whole store event can be asserted on.
struct FixedMint;

impl Mint for FixedMint {
    fn correlation_id(&self) -> String {
        "fixed-correlation-id".to_string()
    }
    fn received_at(&self) -> String {
        "2026-08-21T09:15:00+00:00".to_string()
    }
    fn ack_timestamp(&self) -> String {
        "20260821091500".to_string()
    }
}

/// One flow's deployment, as `spec/01-architecture.md` §2.2 configures it.
struct Deployment {
    /// The sample file's name, which is also its corpus key prefix.
    sample: &'static str,
    /// `HL7_VALIDATION_FLOW`.
    flow: &'static str,
    /// `HL7_VERSION`.
    version: &'static str,
    /// `SENDING_APP`, empty where the flow does not restrict it.
    sending_app: &'static str,
    /// The corpus's flow name, where it differs from `flow`.
    ///
    /// `mpi-outbound-pharmacy.sample.hl7` has no `mpi` schema group to be
    /// captured against — `spec/01-architecture.md` §2.2.1 says the `mpi`
    /// flow suppresses flow-schema validation entirely — so the Python
    /// capture ran it through `mosaiq`. `None` means there is no comparable
    /// Python XML and only the forwarded bytes are checked.
    corpus_flow: Option<&'static str>,
}

/// Every flow this server is deployed as.
const DEPLOYMENTS: &[Deployment] = &[
    Deployment {
        sample: "phw-to-mpi.sample.hl7",
        flow: "phw",
        version: "2.5",
        sending_app: "252",
        corpus_flow: Some("phw"),
    },
    Deployment {
        sample: "paris-to-mpi.sample.hl7",
        flow: "paris",
        version: "2.5.1",
        sending_app: "169",
        corpus_flow: Some("paris"),
    },
    Deployment {
        sample: "chemocare-to-mpi.sample.hl7",
        flow: "chemo",
        version: "2.4",
        sending_app: "245,212,192,224",
        corpus_flow: Some("chemo"),
    },
    Deployment {
        sample: "pims-to-mpi.sample.hl7",
        flow: "pims",
        version: "2.3.1",
        // Per-site, so the allow-list is not set for this flow.
        sending_app: "",
        corpus_flow: Some("pims"),
    },
    Deployment {
        sample: "mosaiq-to-mpi.sample.hl7",
        flow: "mosaiq",
        version: "2.5",
        sending_app: "285",
        corpus_flow: Some("mosaiq"),
    },
    Deployment {
        sample: "mpi-outbound-pharmacy.sample.hl7",
        flow: "mpi",
        version: "2.5",
        sending_app: "100",
        corpus_flow: None,
    },
];

/// The sample messages, as the wire carries them.
///
/// `include_str!` needs a literal, so they are listed rather than read from
/// the directory — which also means adding a sample without adding it here
/// is visible rather than silent.
fn sample(name: &str) -> String {
    let raw = match name {
        "phw-to-mpi.sample.hl7" => {
            include_str!("../../../local/sample_messages/phw-to-mpi.sample.hl7")
        }
        "paris-to-mpi.sample.hl7" => {
            include_str!("../../../local/sample_messages/paris-to-mpi.sample.hl7")
        }
        "chemocare-to-mpi.sample.hl7" => {
            include_str!("../../../local/sample_messages/chemocare-to-mpi.sample.hl7")
        }
        "pims-to-mpi.sample.hl7" => {
            include_str!("../../../local/sample_messages/pims-to-mpi.sample.hl7")
        }
        "mosaiq-to-mpi.sample.hl7" => {
            include_str!("../../../local/sample_messages/mosaiq-to-mpi.sample.hl7")
        }
        "mpi-outbound-pharmacy.sample.hl7" => {
            include_str!("../../../local/sample_messages/mpi-outbound-pharmacy.sample.hl7")
        }
        other => panic!("no sample named {other}"),
    };
    // The files on disk use `\n`; the wire uses `\r` (`spec/index.md` §0.6).
    raw.trim_end().replace("\r\n", "\n").replace('\n', "\r")
}

/// Run one sample through the whole pipeline.
async fn run(deployment: &Deployment) -> (FakeBus, String, String) {
    let mut pairs: Vec<(String, String)> = [
        ("WORKFLOW_ID", "golden"),
        ("MICROSERVICE_ID", "hl7-server-golden"),
        ("HEALTH_BOARD", "GOLD"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_QUEUE_NAME", EGRESS),
        ("EGRESS_SESSION_ID", "golden-session"),
        ("MESSAGE_STORE_QUEUE_NAME", STORE),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    pairs.push(("HL7_VALIDATION_FLOW".into(), deployment.flow.into()));
    pairs.push(("HL7_VERSION".into(), deployment.version.into()));
    if !deployment.sending_app.is_empty() {
        pairs.push(("SENDING_APP".into(), deployment.sending_app.into()));
    }

    let config = Config::from_pairs(pairs).expect("the deployment configuration is complete");
    let bus = FakeBus::new();
    let pipeline: Pipeline<FakeSender, FakeSender, FixedMint> = Pipeline::with_mint(
        config,
        bus.sender(EGRESS),
        Some(bus.sender(STORE)),
        FixedMint,
    );

    let message = sample(deployment.sample);
    let ack = pipeline
        .handle(&message)
        .await
        .unwrap_or_else(|error| panic!("{} should be accepted: {error}", deployment.sample));

    (bus, message, ack)
}

/// Reduce a v2.xml document to the information it carries.
///
/// A sequence of (local element name, text). Namespace prefixes, the XML
/// declaration and inter-element whitespace are dropped, because the two
/// implementations differ on all three and none of them changes meaning.
fn canonical(xml: &str) -> Vec<(String, String)> {
    let mut elements: Vec<(String, String)> = Vec::new();
    let mut rest = xml.trim_start();

    // Drop a leading declaration, which only one of the two writes.
    if rest.starts_with("<?")
        && let Some(at) = rest.find("?>")
    {
        rest = &rest[at + 2..];
    }

    while let Some(open) = rest.find('<') {
        let text = rest[..open].trim();
        if !text.is_empty()
            && let Some(last) = elements.last_mut()
        {
            last.1 = text.to_string();
        }

        rest = &rest[open..];
        let Some(close) = rest.find('>') else { break };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];

        // Closing tags, declarations and comments carry no element.
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }

        let name = tag
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        let local = name.rsplit(':').next().unwrap_or(name);
        elements.push((local.to_string(), String::new()));
    }

    elements
}

/// The Python's XML for one corpus case.
fn python_xml(sample: &str, flow: &str) -> String {
    let corpus: serde_json::Value = serde_json::from_str(CORPUS).expect("the corpus parses");
    corpus[format!("{sample}|{flow}|as-is")]["xml"]
        .as_str()
        .unwrap_or_else(|| panic!("no Python capture for {sample}|{flow}|as-is"))
        .to_string()
}

/// The store event a run produced.
fn store_event(bus: &FakeBus) -> ihb_bus::store::StoreEvent {
    let stored = bus.all_sent(STORE);
    assert_eq!(stored.len(), 1, "exactly one store event");
    ihb_bus::store::StoreEvent::from_json(std::str::from_utf8(&stored[0].body).unwrap())
        .expect("the store event is the JSON the store service reads")
}

/// Differences between the Rust's and the Python's XML, as
/// `element-name@index` with both texts.
///
/// Returned as a set so the assertion can be "these differences and no
/// others", which fails both when a new one appears and when a known one is
/// fixed. A list of known exceptions that is never checked for staleness
/// becomes a list of bugs nobody is looking at.
fn differences(rust: &str, python: &str) -> BTreeSet<String> {
    let rust = canonical(rust);
    let python = canonical(python);

    assert_eq!(
        rust.len(),
        python.len(),
        "the two documents should have the same number of elements"
    );

    rust.iter()
        .zip(python.iter())
        .enumerate()
        .filter(|(_, (r, p))| r != p)
        .map(|(index, (r, p))| {
            assert_eq!(r.0, p.0, "element {index} should have the same name");
            format!("{}@{index}: rust={:?} python={:?}", r.0, r.1, p.1)
        })
        .collect()
}

#[tokio::test]
async fn every_sample_is_forwarded_byte_for_byte() {
    // spec/07-hl7-server.md §9.8, and the claim that matters most: this
    // server validates and forwards, it does not transform. Whatever MPI
    // eventually receives is what the source system sent.
    for deployment in DEPLOYMENTS {
        let (bus, message, ack) = run(deployment).await;

        let sent = bus.all_sent(EGRESS);
        assert_eq!(sent.len(), 1, "{}", deployment.sample);
        assert_eq!(
            String::from_utf8_lossy(&sent[0].body),
            message,
            "{} must be forwarded unchanged",
            deployment.sample
        );

        // And it was acknowledged, which §5.3 says may only happen after
        // the forward succeeded.
        assert!(ack.contains("MSA|AA|"), "{}: {ack}", deployment.sample);
    }
}

#[tokio::test]
async fn the_message_id_is_the_control_id_for_every_flow() {
    // spec/07-hl7-server.md §6.3. Per flow rather than once, because the
    // control id is read from `MSH-10` and the samples differ in how much
    // of the header they populate.
    for deployment in DEPLOYMENTS {
        let (bus, message, _) = run(deployment).await;

        let expected = message
            .split('\r')
            .next()
            .and_then(|msh| msh.split('|').nth(9))
            .expect("every sample has an MSH-10");

        assert_eq!(
            bus.all_sent(EGRESS)[0].message_id.as_deref(),
            Some(expected),
            "{}",
            deployment.sample
        );
    }
}

#[tokio::test]
async fn the_stored_xml_carries_what_the_pythons_did() {
    // spec/07-hl7-server.md §9.8 and spec/06-validation.md §8.4.
    //
    // The two known differences are listed exactly. Each is a real
    // divergence in content, not a serialisation artefact, and each is
    // recorded in the specification; see the constants below.
    for deployment in DEPLOYMENTS {
        let Some(corpus_flow) = deployment.corpus_flow else {
            continue;
        };

        let (bus, _, _) = run(deployment).await;
        let event = store_event(&bus);
        let ours = event
            .xml_payload
            .unwrap_or_else(|| panic!("{} should produce an XmlPayload", deployment.sample));

        let theirs = python_xml(deployment.sample, corpus_flow);
        let found = differences(&ours, &theirs);

        let expected: BTreeSet<String> = known_differences(deployment.sample)
            .iter()
            .map(ToString::to_string)
            .collect();

        assert_eq!(
            found, expected,
            "{}: the differences from the Python's XML are not the ones on record",
            deployment.sample
        );
    }
}

/// The differences from the Python's XML that are known and accepted.
///
/// Both are recorded in `spec/06-validation.md` §8.4 with the reasoning.
/// This list is asserted to be exact, so it cannot quietly grow.
fn known_differences(sample: &str) -> &'static [&'static str] {
    match sample {
        // `spec/06-validation.md` §8.4.1 — the chemocare sample's `MSH-9`
        // is `ADT^A28`, with no structure component. The Rust writes the
        // resolved structure into `MSH-9.3` before converting, because the
        // converter names the document's root element from `MSH-9` and
        // would otherwise produce `<ADT_A28>` — a root no schema knows.
        // The Python left the component empty and named the root
        // separately.
        "chemocare-to-mpi.sample.hl7" => &[r#"MSG.3@26: rust="ADT_A05" python="""#],

        // `spec/06-validation.md` §8.4.2 — the pims sample's `PID-5` is
        // `test^test^""^^MISS`. HL7's `""` is an explicit null, meaning
        // "delete this value", which is not the same as an absent value.
        // The Python carried the two characters through into the XML; the
        // Rust's converter renders them as an empty element.
        //
        // The **forwarded** message keeps them: this server sends the raw
        // ER7, and the test above asserts it is byte-for-byte unchanged.
        // Only the audit rendering loses the distinction.
        "pims-to-mpi.sample.hl7" => &[r#"XPN.3@87: rust="" python="\"\"""#],

        _ => &[],
    }
}

#[tokio::test]
async fn the_mpi_flow_stores_xml_without_schema_validation() {
    // spec/01-architecture.md §2.2.1 — the `mpi` flow suppresses
    // flow-schema validation, so its XmlPayload comes from the
    // schema-unaware conversion of spec/07 §3.5. There is no Python XSD
    // capture to compare against, which is why `corpus_flow` is `None`;
    // what can be asserted is that an XmlPayload is still produced, because
    // §3.5's failure is non-fatal and could silently yield none.
    let deployment = DEPLOYMENTS
        .iter()
        .find(|d| d.flow == "mpi")
        .expect("the mpi flow is deployed");

    let (bus, _, _) = run(deployment).await;
    let event = store_event(&bus);

    let xml = event
        .xml_payload
        .expect("§3.5 should still produce an XmlPayload for the mpi flow");
    assert!(xml.contains("urn:hl7-org:v2xml"), "{xml}");

    // And the routing properties of §6.2 are on the forwarded message.
    let properties = &bus.all_sent(EGRESS)[0].properties;
    assert_eq!(properties["MessageType"], "A31");
    assert!(
        properties.contains_key("UpdateSources"),
        "the mpi flow adds all five routing properties"
    );
}

#[tokio::test]
async fn every_store_event_names_its_component_and_session() {
    // spec/12-message-store.md, via spec/07 §5 step 10. Replay restores a
    // message onto the session named here, so a wrong value sends a
    // replayed message down the wrong flow.
    for deployment in DEPLOYMENTS {
        let (bus, message, _) = run(deployment).await;
        let event = store_event(&bus);

        assert_eq!(event.raw_payload, message, "{}", deployment.sample);
        assert_eq!(event.correlation_id, "fixed-correlation-id");
        assert_eq!(event.processing_component, "hl7-server-golden");
        assert_eq!(event.target_system.as_deref(), Some("MPI"));
        assert_eq!(event.session_id, "golden-session", "{}", deployment.sample);
        assert_eq!(
            event.message_received_at, "2026-08-21T09:15:00+00:00",
            "{}",
            deployment.sample
        );
    }
}

#[test]
fn the_canonical_form_ignores_only_what_it_should() {
    // The comparison above is only as trustworthy as this function. Two
    // documents that differ in every way the two implementations actually
    // differ must compare equal; two that differ in content must not.
    let python = r#"<ns0:ADT_A05 xmlns:ns0="urn:hl7-org:v2xml"><ns0:MSH><ns0:MSH.1>|</ns0:MSH.1></ns0:MSH></ns0:ADT_A05>"#;
    let rust = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                <ADT_A05 xmlns=\"urn:hl7-org:v2xml\">\n  <MSH>\n    <MSH.1>|</MSH.1>\n  </MSH>\n</ADT_A05>";

    assert_eq!(
        canonical(python),
        canonical(rust),
        "prefix, declaration and indentation"
    );

    let different = rust.replace("<MSH.1>|</MSH.1>", "<MSH.1>^</MSH.1>");
    assert_ne!(canonical(python), canonical(&different), "but not the text");

    let renamed = rust.replace("MSH.1", "MSH.2");
    assert_ne!(canonical(python), canonical(&renamed), "and not the names");
}
