//! One inbound message to N classified sends, or a dead letter.
//!
//! Implements `spec/09-transformers.md` §10.4, §10.5 and §10.6. Upstream:
//! `hl7_lims_wrrs_transformer/message_processor.py`.
//!
//! Separated from the receive loop so it runs against `ihb_bus::FakeBus`
//! with no broker, as `spec/09` §6.7 asks of the shared runtime.

use ihb_bus::message::{Incoming, Outgoing};
use ihb_bus::metadata::{LogValues, Properties, correlation_id_for_event};
use ihb_bus::sink::MessageSink;
use ihb_telemetry::events::EventLogger;

use crate::classifier::{classify, trigger_event};
use crate::mapping::MappingTypes;

/// Application property: the matched rule. Contract with the Terraform
/// subscription filters (`§10.4`).
pub const MAPPING_TYPE_KEY: &str = "MappingType";

/// Application property: a new UUID per path (§10.4). Distinguishes the
/// copies of one inbound message in traces; it is not the Service Bus
/// `message_id`, which is deterministic for duplicate detection.
pub const PROCESSING_ID_KEY: &str = "ProcessingId";

/// Application property: the inbound Service Bus `message_id`, else `MSH-10`
/// (§10.4). Identifies the source message across all of its classified copies.
pub const ORIGINAL_MESSAGE_ID_KEY: &str = "OriginalMessageId";

/// Dead-letter reason for a message no rule matched (`§10.5`).
pub const UNSUPPORTED_MESSAGE_REASON: &str = "Unsupported message type";

/// Dead-letter reason for an unreadable message (`§10.5`): not UTF-8 or not
/// parseable as ER7. Operators filter the dead-letter queue on this text.
pub const INVALID_MESSAGE_REASON: &str = "Invalid HL7 message";

/// What to do with the inbound message once every path has been tried.
///
/// The three outcomes map to the three Service Bus settlements; choosing
/// `Complete` for a message with an unpublished copy would lose it silently,
/// so [`process_one`] only returns it when every send succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Every path was published. Complete it.
    Complete,
    /// At least one path failed. Abandon it, and everything behind it in
    /// the batch (`spec/09` §1.7), for redelivery (`§10.6`).
    Abandon,
    /// It can never succeed. Dead-letter it and carry on with the batch
    /// (`§10.5`).
    DeadLetter {
        /// The dead-letter reason.
        reason: String,
        /// The description: identifiers only, never payload.
        description: String,
    },
}

/// Take one inbound message through `§10`.
///
/// Never panics. A send failure is a [`Disposition::Abandon`], never a
/// completion: completing a message whose copy was not published would lose
/// a clinical message silently (`AGENTS.md`).
///
/// # Examples
///
/// ```
/// use hl7_lims_wrrs_transformer::{Disposition, MappingTypes, process_one};
/// use ihb_bus::fake::FakeBus;
/// use ihb_bus::message::Outgoing;
/// use ihb_telemetry::events::EventLogger;
///
/// # tokio_test::block_on(async {
/// let bus = FakeBus::new();
/// bus.send("in", Outgoing::new(
///     "MSH|^~\\&|328|L|W|D|2026||ADT^A31^ADT_A05|M1|P|2.5\r\
///      PID|1||1^^^NHS^NH~L1^^^328^PI",
/// ).with_message_id("orig-1"))?;
/// let message = bus.receive("in", 1).remove(0);
///
/// let sink = bus.sender("classified");
/// let events = EventLogger::new("lims-to-wrrs", "lims_wrrs_hl7_transformer");
/// let outcome = process_one(&message, &sink, Some("lims-to-wrrs"), &MappingTypes::default(), &events).await;
///
/// assert_eq!(outcome, Disposition::Complete);
/// assert_eq!(bus.all_sent("classified")[0].message_id.as_deref(), Some("orig-1-LIMS_NHS_ID_UPDATE"));
/// # Ok::<(), ihb_bus::BusError>(())
/// # }).unwrap();
/// ```
pub async fn process_one<E: MessageSink>(
    message: &Incoming,
    egress: &E,
    egress_session_id: Option<&str>,
    types: &MappingTypes,
    events: &EventLogger,
) -> Disposition {
    let values = LogValues::from_properties(&message.properties);
    let correlation = correlation_id_for_event(&values.correlation_id);
    let shown_id = message.message_id.clone().unwrap_or_default();

    // Undecodable bytes can never be parsed, so dead-letter rather than burn
    // delivery attempts (upstream `_decode_or_dead_letter`). The lossy copy
    // goes only to the event, which redacts.
    let Ok(body) = std::str::from_utf8(&message.body) else {
        let description = format!(
            "Invalid HL7 message - MessageId: {shown_id}, CorrelationId: {}: not valid UTF-8",
            values.correlation_id
        );
        tracing::error!(message_id = %shown_id, "{INVALID_MESSAGE_REASON}: not valid UTF-8");
        events.log_message_failed(
            &String::from_utf8_lossy(&message.body),
            &description,
            Some(INVALID_MESSAGE_REASON),
            correlation,
        );
        return dead_letter(INVALID_MESSAGE_REASON, description);
    };

    events.log_message_received(body, Some("LIMS WRRS message received"), correlation);

    let parsed = match ihb_validation::parse(body) {
        Ok(parsed) => parsed,
        Err(error) => {
            // A parse error names a structural position, not a value
            // (`spec/index.md` §0.8), so it is safe in a description.
            let description = format!(
                "Invalid HL7 message - MessageId: {shown_id}, CorrelationId: {}: {error}",
                values.correlation_id
            );
            tracing::error!(message_id = %shown_id, "{INVALID_MESSAGE_REASON}");
            events.log_message_failed(
                body,
                &description,
                Some(INVALID_MESSAGE_REASON),
                correlation,
            );
            return dead_letter(INVALID_MESSAGE_REASON, description);
        }
    };

    // The Service Bus id, else MSH-10 (`§10.4`).
    let original_message_id = message
        .message_id
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| ihb_fields::read(&parsed, "MSH-10"));
    let trigger = trigger_event(&parsed);
    let mapping_types = classify(&parsed, types);

    tracing::info!(
        message_id = %original_message_id,
        correlation_id = %values.correlation_id,
        trigger_event = %trigger,
        mapping_types = ?mapping_types,
        "classified message"
    );

    if mapping_types.is_empty() {
        let description = format!(
            "Unsupported message type (trigger event '{trigger}') - \
             MessageId: {original_message_id}, CorrelationId: {}",
            values.correlation_id
        );
        tracing::error!("{description}");
        events.log_message_failed(
            body,
            &description,
            Some(UNSUPPORTED_MESSAGE_REASON),
            correlation,
        );
        return dead_letter(UNSUPPORTED_MESSAGE_REASON, description);
    }

    // Every path is attempted even after one fails (`§10.6`): `all` would
    // short-circuit and leave the later copies unpublished until redelivery.
    let mut all_published = true;
    for mapping_type in &mapping_types {
        let published = publish_path(
            body,
            message,
            mapping_type,
            &original_message_id,
            (egress, egress_session_id),
            events,
            (correlation, &values.correlation_id),
        )
        .await;
        all_published &= published;
    }

    if all_published {
        Disposition::Complete
    } else {
        Disposition::Abandon
    }
}

/// Build a [`Disposition::DeadLetter`]. `description` must hold identifiers
/// only (§10.5), never message content.
fn dead_letter(reason: &str, description: String) -> Disposition {
    Disposition::DeadLetter {
        reason: reason.to_string(),
        description,
    }
}

/// Publish one classified copy (`§10.4`). `true` when it was sent.
async fn publish_path<E: MessageSink>(
    body: &str,
    message: &Incoming,
    mapping_type: &str,
    original_message_id: &str,
    (egress, session): (&E, Option<&str>),
    events: &EventLogger,
    (correlation, correlation_text): (Option<&str>, &str),
) -> bool {
    let processing_id = uuid::Uuid::new_v4().to_string();

    // The inbound properties first, so the three below win a clash, and the
    // correlation id and trace context survive the hop (`spec/09` §1.3.1).
    let mut properties: Properties = message.properties.clone();
    properties.insert(MAPPING_TYPE_KEY.into(), mapping_type.into());
    properties.insert(PROCESSING_ID_KEY.into(), processing_id.clone());
    properties.insert(ORIGINAL_MESSAGE_ID_KEY.into(), original_message_id.into());

    // The deterministic id is what lets duplicate detection drop an
    // already-published path when the inbound message is redelivered after a
    // sibling path failed (`§10.6`). It must differ per path or the broker
    // would drop the second copy of a first delivery.
    let outgoing = Outgoing::new(body)
        .with_message_id(format!("{original_message_id}-{mapping_type}"))
        .with_optional_session(session)
        .with_properties(properties);

    if let Err(error) = egress.send(outgoing).await {
        let description = format!(
            "Failed to publish {mapping_type} path - MessageId: {original_message_id}, \
             CorrelationId: {correlation_text}, ProcessingId: {processing_id}: {error}"
        );
        tracing::error!("{description}");
        events.log_message_failed(
            body,
            &description,
            Some(&format!("LIMS WRRS publish failed ({mapping_type})")),
            correlation,
        );
        return false;
    }

    tracing::info!(
        mapping_type,
        message_id = %original_message_id,
        processing_id = %processing_id,
        "published path"
    );
    events.log_message_processed(
        body,
        Some(&format!(
            "LIMS WRRS classified as {mapping_type} \
             (MessageId: {original_message_id}, ProcessingId: {processing_id})"
        )),
        correlation,
    );
    true
}

/// Process a received batch, in order.
///
/// Returns one [`Disposition`] per message handled. An
/// [`Disposition::Abandon`] is the **last** entry: everything behind it is
/// left unattempted and the caller abandons it too (`spec/09` §1.7). A dead
/// letter does not stop the batch (`§10.5`).
pub async fn process_batch<E: MessageSink>(
    batch: &[Incoming],
    egress: &E,
    egress_session_id: Option<&str>,
    types: &MappingTypes,
    events: &EventLogger,
) -> Vec<Disposition> {
    let mut outcomes = Vec::with_capacity(batch.len());
    for message in batch {
        let outcome = process_one(message, egress, egress_session_id, types, events).await;
        let stop = outcome == Disposition::Abandon;
        outcomes.push(outcome);
        if stop {
            break;
        }
    }
    outcomes
}

#[cfg(test)]
#[allow(clippy::assert_is_empty, reason = "plain asserts read best in tests")]
mod tests {
    use super::*;
    use ihb_bus::BusError;
    use ihb_bus::fake::FakeBus;

    const A31: &str = "MSH|^~\\&|328|LIMS|WRRS|DHCW|20260101120000||ADT^A31^ADT_A05|MSG0001|P|2.5\rEVN||20260101120000\rPID|1||1234567^^^NHS^NH~L123^^^328^PI||SMITH^JOHN||19800101|M\rPV1|1|N";
    const A40: &str = "MSH|^~\\&|328|LIMS|WRRS|DHCW|20260101120000||ADT^A40^ADT_A39|MSG0001|P|2.5\rEVN||20260101120000\rPID|1||1234567^^^NHS^NH~L123^^^328^PI||SMITH^JOHN||19800101|M\rMRG|OLD123^^^328^PI";
    const A04: &str = "MSH|^~\\&|328|LIMS|WRRS|DHCW|20260101120000||ADT^A04^ADT_A01|MSG0001|P|2.5\rPID|1||1234567^^^NHS^NH~L123^^^328^PI";

    fn props() -> Properties {
        [
            ("CorrelationId", "corr-1"),
            ("WorkflowID", "lims-to-mpi"),
            ("SourceSystem", "328"),
            ("MessageType", "A40"),
            ("traceparent", "00-abc-def-01"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    /// Put `body` on `in` as the broker would deliver it.
    fn inbound(bus: &FakeBus, body: impl Into<Vec<u8>>, id: Option<&str>) -> Incoming {
        let mut out = Outgoing::new(body).with_properties(props());
        if let Some(id) = id {
            out = out.with_message_id(id);
        }
        bus.send("in", out).unwrap();
        bus.receive("in", 1).remove(0)
    }

    fn events() -> EventLogger {
        EventLogger::new("lims-to-wrrs", "lims_wrrs_hl7_transformer")
    }

    async fn run(bus: &FakeBus, message: &Incoming) -> Disposition {
        process_one(
            message,
            &bus.sender("out"),
            Some("lims-to-wrrs"),
            &MappingTypes::default(),
            &events(),
        )
        .await
    }

    #[tokio::test]
    async fn a31_publishes_one_nhs_id_update() {
        let bus = FakeBus::new();
        let message = inbound(&bus, A31, Some("orig-1"));

        assert_eq!(run(&bus, &message).await, Disposition::Complete);

        let sent = bus.all_sent("out");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].properties["MappingType"], "LIMS_NHS_ID_UPDATE");
        assert_eq!(sent[0].session_id.as_deref(), Some("lims-to-wrrs"));
    }

    #[tokio::test]
    async fn a40_fans_out_two_independent_messages() {
        // §10.4.
        let bus = FakeBus::new();
        let message = inbound(&bus, A40, Some("orig-1"));

        assert_eq!(run(&bus, &message).await, Disposition::Complete);

        let sent = bus.all_sent("out");
        let kinds: Vec<_> = sent
            .iter()
            .map(|m| m.properties["MappingType"].as_str())
            .collect();
        assert_eq!(kinds, ["LIMS_NHS_ID_UPDATE", "ADT_A40"]);
        assert_ne!(
            sent[0].properties["ProcessingId"],
            sent[1].properties["ProcessingId"]
        );
        for m in &sent {
            assert_eq!(m.properties["OriginalMessageId"], "orig-1");
            assert_eq!(m.properties["CorrelationId"], "corr-1");
            assert_eq!(m.body, A40.as_bytes(), "the body is published unchanged");
        }
        let ids: Vec<_> = sent.iter().map(|m| m.message_id.clone().unwrap()).collect();
        assert_eq!(ids, ["orig-1-LIMS_NHS_ID_UPDATE", "orig-1-ADT_A40"]);
    }

    #[tokio::test]
    async fn inbound_properties_are_preserved() {
        // spec/09 §1.3.1 as applied by §10.4.
        let bus = FakeBus::new();
        let message = inbound(&bus, A31, Some("orig-1"));
        run(&bus, &message).await;

        let p = &bus.all_sent("out")[0].properties;
        assert_eq!(p["WorkflowID"], "lims-to-mpi");
        assert_eq!(p["SourceSystem"], "328");
        assert_eq!(p["MessageType"], "A40");
        assert_eq!(p["traceparent"], "00-abc-def-01");
    }

    #[tokio::test]
    async fn msh_10_is_the_fallback_original_id() {
        let bus = FakeBus::new();
        let message = inbound(&bus, A31, None);
        run(&bus, &message).await;
        assert_eq!(
            bus.all_sent("out")[0].properties["OriginalMessageId"],
            "MSG0001"
        );
    }

    #[tokio::test]
    async fn an_unsupported_message_is_dead_lettered_and_not_published() {
        // §10.5.
        let bus = FakeBus::new();
        let message = inbound(&bus, A04, Some("orig-1"));

        let Disposition::DeadLetter {
            reason,
            description,
        } = run(&bus, &message).await
        else {
            panic!("expected a dead letter");
        };
        assert_eq!(reason, "Unsupported message type");
        assert!(
            description.contains("orig-1") && description.contains("corr-1"),
            "{description}"
        );
        assert!(description.contains("'A04'"), "{description}");
        assert!(bus.all_sent("out").is_empty());
    }

    #[tokio::test]
    async fn an_unparseable_message_is_dead_lettered() {
        let bus = FakeBus::new();
        let message = inbound(&bus, "not an hl7 message", Some("orig-1"));

        let Disposition::DeadLetter { reason, .. } = run(&bus, &message).await else {
            panic!("expected a dead letter");
        };
        assert_eq!(reason, "Invalid HL7 message");
        assert!(bus.all_sent("out").is_empty());
    }

    #[tokio::test]
    async fn invalid_utf8_is_dead_lettered_without_the_bytes() {
        let bus = FakeBus::new();
        let message = inbound(&bus, b"MSH|^~\\&|\xff\xfe".to_vec(), Some("orig-1"));

        let Disposition::DeadLetter {
            reason,
            description,
        } = run(&bus, &message).await
        else {
            panic!("expected a dead letter");
        };
        assert_eq!(reason, "Invalid HL7 message");
        assert!(description.contains("orig-1") && description.contains("corr-1"));
        assert!(!description.contains("MSH"), "no payload in a description");
    }

    #[tokio::test]
    async fn a_failed_path_abandons_but_the_other_is_still_sent() {
        // §10.6: never complete a message whose copy was not published.
        let bus = FakeBus::new();
        let message = inbound(&bus, A40, Some("orig-1"));
        bus.fail_next_send(BusError::Disposition {
            action: "send",
            entity: "out".into(),
            reason: "boom".into(),
        });

        assert_eq!(run(&bus, &message).await, Disposition::Abandon);
        assert_eq!(
            bus.all_sent("out").len(),
            1,
            "the second path was still attempted"
        );
        assert_eq!(bus.all_sent("out")[0].properties["MappingType"], "ADT_A40");
    }

    #[tokio::test]
    async fn a_retry_reuses_the_same_message_ids() {
        // §10.6: what makes duplicate detection drop the repeat.
        let bus = FakeBus::new();
        let message = inbound(&bus, A40, Some("orig-1"));
        run(&bus, &message).await;
        run(&bus, &message).await;

        let sent = bus.all_sent("out");
        assert_eq!(sent[0].message_id, sent[2].message_id);
        assert_eq!(sent[1].message_id, sent[3].message_id);
        assert_ne!(sent[0].message_id, sent[1].message_id);
    }

    #[tokio::test]
    async fn a_dead_letter_does_not_stop_the_batch_but_an_abandon_does() {
        let bus = FakeBus::new();
        let batch = vec![inbound(&bus, A04, Some("a")), inbound(&bus, A31, Some("b"))];
        let out = process_batch(
            &batch,
            &bus.sender("out"),
            None,
            &MappingTypes::default(),
            &events(),
        )
        .await;
        assert!(matches!(out[0], Disposition::DeadLetter { .. }));
        assert_eq!(out[1], Disposition::Complete);

        // Built first: the fake bus's injected failure hits the next send of
        // any kind, including the one that stages the inbound message.
        let batch = vec![inbound(&bus, A31, Some("c")), inbound(&bus, A31, Some("d"))];
        bus.fail_next_send(BusError::Disposition {
            action: "send",
            entity: "out".into(),
            reason: "x".into(),
        });
        let out = process_batch(
            &batch,
            &bus.sender("out"),
            None,
            &MappingTypes::default(),
            &events(),
        )
        .await;
        assert_eq!(out, [Disposition::Abandon], "the tail is left unattempted");
    }
}
