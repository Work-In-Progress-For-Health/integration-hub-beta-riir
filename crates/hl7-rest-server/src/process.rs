//! What happens to one HL7 message.
//!
//! Implements `spec/19-rest-servers.md` §3.
//!
//! # The ordering is the design
//!
//! As `hl7-server`'s `pipeline` (`spec/07-hl7-server.md` §5), and for the same
//! reasons. The received event comes first, so a message that kills the
//! process still left a trace. The store send comes before the bus send, so
//! there is a persisted copy before the message is handed on. The bus send
//! comes before the acknowledgement and is blocking: **acknowledging a
//! message that was not forwarded is the one failure this system must never
//! produce**, and here the acknowledgement is an HTTP `201`.
//!
//! So which errors in this module use `?` and which are caught and logged is
//! not stylistic. Each is a clause.
//!
//! # Every failure carries its own NACK
//!
//! [`Failure`] holds the status, the NACK and the reason. The HTTP layer
//! renders it and decides nothing; a status chosen there could disagree with
//! the NACK chosen here.

use hl7_2::Message;
use ihb_bus::message::Outgoing;
use ihb_bus::metadata::{Properties, Tracking, WORKFLOW_ID};
use ihb_bus::sink::MessageSink;
use ihb_bus::store::StoreEvent;
use ihb_fields::read;
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::ack;
use crate::config::{FlowRule, Hl7Settings, Identity};
use crate::risp::{Destination, RoutingTarget, resolve_targets};
use crate::routing::flow_properties;
use crate::rules;

/// The `validation_result` on a parse failure (`spec/19-rest-servers.md` §3.1).
const UNPARSEABLE: &str = "Message could not be parsed";

/// The `validation_result` on a validation failure (§3.2).
const REJECTED: &str = "Message rejected - failed validation";

/// The `validation_result` on a failed forward (§3.4).
const NOT_FORWARDED: &str = "Failed to forward message";

/// Mints the per-message values, so a test can fix them.
///
/// Everything non-deterministic in the pipeline (ids and clocks) comes
/// through here, which is what lets the ACK text and the forwarded
/// properties be asserted byte for byte.
pub trait Mint: Send + Sync {
    /// A fresh correlation id, unique per message and carried to every hop.
    fn correlation_id(&self) -> String;
    /// The receive time for `MessageReceivedAt`: UTC, Python `isoformat()`
    /// shaped (`spec/index.md` §0.7), because the next hop parses it.
    fn received_at(&self) -> String;
    /// The `MSH-7` of an ACK: HL7 `TS` (`YYYYMMDDHHMMSS`) in the process's
    /// local offset, not UTC; see [`ihb_fields::timestamp::timestamp`].
    fn ack_timestamp(&self) -> String;
    /// A generated `MSH-10` for an ACK whose request carried none
    /// (`spec/19-rest-servers.md` §4.3).
    fn control_id(&self) -> String;
}

/// A UUID and the real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealMint;

impl Mint for RealMint {
    fn correlation_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
    fn received_at(&self) -> String {
        ihb_telemetry::clock::Clock::now_iso8601(&ihb_telemetry::clock::SystemClock)
    }
    fn ack_timestamp(&self) -> String {
        ihb_fields::timestamp::timestamp()
    }
    fn control_id(&self) -> String {
        ack::generate_control_id()
    }
}

/// A message that was not forwarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// 422 for a validation failure, 500 for anything else.
    pub status: u16,
    /// The NACK to return as `ErrorMessage`.
    pub nack: String,
    /// Why, for the log. The NACK carries it too (sanitised).
    pub reason: String,
}

/// One message's journey, minus HTTP.
pub struct Processor<E, S, M = RealMint> {
    /// Who this is.
    identity: Identity,
    /// How messages are judged and routed.
    settings: Hl7Settings,
    /// `EGRESS_*`. Blocking.
    egress: E,
    /// `WRRS_*`, for `risp`.
    wrrs: Option<E>,
    /// The audit copy. Non-blocking.
    store: Option<S>,
    /// Events.
    events: EventLogger,
    /// Counters.
    metrics: Metrics,
    /// Correlation ids and timestamps.
    mint: M,
}

impl<E: MessageSink, S: MessageSink> Processor<E, S, RealMint> {
    /// A processor using the real clock.
    pub fn new(
        identity: Identity,
        settings: Hl7Settings,
        egress: E,
        wrrs: Option<E>,
        store: Option<S>,
    ) -> Self {
        Self::with_mint(identity, settings, egress, wrrs, store, RealMint)
    }
}

impl<E: MessageSink, S: MessageSink, M: Mint> Processor<E, S, M> {
    /// A processor with an explicit [`Mint`].
    pub fn with_mint(
        identity: Identity,
        settings: Hl7Settings,
        egress: E,
        wrrs: Option<E>,
        store: Option<S>,
        mint: M,
    ) -> Self {
        let events = EventLogger::new(&identity.workflow_id, &identity.microservice_id)
            .with_redaction(identity.redaction_enabled);
        let metrics = Metrics::new(
            &identity.workflow_id,
            &identity.microservice_id,
            &identity.health_board,
            &identity.peer_service,
        );
        Self {
            identity,
            settings,
            egress,
            wrrs,
            store,
            events,
            metrics,
            mint,
        }
    }

    /// The generic `AE` for something that is not a parsed message.
    ///
    /// Used by the HTTP layer for a v2.xml that did not convert
    /// (`spec/19-rest-servers.md` §2.2).
    #[must_use]
    pub fn generic_nack(&self, reason: &str) -> String {
        ack::generic_nack(reason, &self.mint.control_id(), &self.mint.ack_timestamp())
    }

    /// Take one ER7 message through every step of §3.
    ///
    /// Returns the ACK to send as the `201` body. An `Err` means the message
    /// was **not** forwarded and no ACK exists.
    ///
    /// # Errors
    ///
    /// [`Failure`], with the status and NACK §2 gives the kind of failure.
    pub async fn process(&self, raw: &str) -> Result<String, Failure> {
        // ---- Steps 1 and 2, before anything can fail. ------------------
        self.events.log_message_received(raw, None, None);
        self.metrics.message_received();

        // ---- Step 3: parse. --------------------------------------------
        let message = match ihb_validation::parse(raw) {
            Ok(message) => message,
            Err(error) => {
                let reason = error.to_string();
                tracing::error!(%error, "HL7 parsing error");
                self.events.log_validation_result(
                    raw,
                    &format!("HL7 parsing error: {reason}"),
                    false,
                    None,
                );
                self.events
                    .log_message_failed(raw, &reason, Some(UNPARSEABLE), None);
                return Err(Failure {
                    status: 500,
                    nack: self.generic_nack(&reason),
                    reason,
                });
            }
        };

        let control_id = read(&message, "MSH-10");
        let message_type = message_type_of(&message);
        tracing::info!(message_type, "Received message");

        // ---- Step 4: common validation. --------------------------------
        if let Err(invalid) = rules::validate(&message, &self.settings) {
            return Err(self.reject(raw, &message, &invalid.0, None));
        }
        self.events.log_validation_result(
            raw,
            &format!("Valid HL7 message - Type: {message_type}"),
            true,
            None,
        );

        // ---- Step 6: tracking metadata. --------------------------------
        let tracking = Tracking {
            correlation_id: self.mint.correlation_id(),
            received_at: self.mint.received_at(),
            source_system: read(&message, "MSH-3"),
            workflow_id: self.identity.workflow_id.clone(),
        };
        let correlation_id = tracking.correlation_id.clone();

        // ---- Step 7: RISP routing, or the flow schema. -----------------
        let (targets, mut xml) = if self.settings.flow == Some(FlowRule::Risp) {
            let targets = match resolve_targets(&message, raw) {
                Ok(targets) => targets,
                Err(invalid) => {
                    return Err(self.reject(raw, &message, &invalid.0, Some(&correlation_id)));
                }
            };
            // §6.2 — an XML target's payload doubles as the store's copy.
            let xml = targets
                .iter()
                .find(|target| target.is_xml)
                .map(|target| target.payload.clone());
            (Some(targets), xml)
        } else {
            (None, self.flow_schema(raw, &message, &tracking).await?)
        };

        // ---- Step 8: a schema-unaware rendering for the store. ---------
        if xml.is_none() {
            xml = self.render_xml(raw, &correlation_id);
        }

        // ---- Step 9: the standard release. -----------------------------
        if let Some(standard) = self.settings.standard.as_deref() {
            if let Err(error) = ihb_validation::validate_standard_parsed(&message, standard) {
                let reason = format!("Standard validation error: {error}");
                return Err(self.reject(raw, &message, &reason, Some(&correlation_id)));
            }
            self.events.log_validation_result(
                raw,
                &format!("Standard HL7 v{standard} validation passed"),
                true,
                Some(&correlation_id),
            );
        }

        // ---- Step 10: routing properties; cannot fail. -----------------
        let mut properties = tracking.to_properties();
        properties.extend(flow_properties(self.settings.flow, &message));

        // ---- Step 11: the message store, non-blocking. -----------------
        self.store_message(raw, xml.as_deref(), &tracking).await;

        // ---- Step 12: Service Bus, blocking. ---------------------------
        if let Some(targets) = targets {
            self.forward_targets(raw, &control_id, &properties, &targets, &correlation_id)
                .await?;
        } else {
            let outgoing = outgoing(
                raw,
                &control_id,
                &self.identity.egress_session_id,
                properties,
            );
            self.send(&self.egress, outgoing, raw, &control_id, &correlation_id)
                .await?;
        }

        // ---- Step 13. --------------------------------------------------
        let acknowledgement = ack::success(&message, &self.mint.ack_timestamp());
        self.events.log_message_processed(
            raw,
            Some("ACK generated successfully"),
            Some(&correlation_id),
        );
        Ok(acknowledgement)
    }

    /// Step 7, outside `risp`: validate against the flow's schema and
    /// produce the store's XML (`spec/19-rest-servers.md` §3.3).
    async fn flow_schema(
        &self,
        raw: &str,
        message: &Message,
        tracking: &Tracking,
    ) -> Result<Option<String>, Failure> {
        let Some(FlowRule::Schema(flow)) = self.settings.flow else {
            // `mpi` has no schema (`01-architecture.md` §2.2.1), and no flow
            // has none to apply.
            return Ok(None);
        };

        let outcome = match ihb_validation::validate_and_convert_parsed(message, flow, None) {
            Ok(result) if result.is_valid => {
                self.events.log_validation_result(
                    raw,
                    &format!("XML validation passed for flow '{}'", flow.as_str()),
                    true,
                    Some(&tracking.correlation_id),
                );
                return Ok(Some(result.xml));
            }
            Ok(result) => result.error.as_ref().map_or_else(
                || "the message does not conform to its flow schema".to_string(),
                ToString::to_string,
            ),
            Err(error) => error.to_string(),
        };

        // §3.3 — **store it, then abort**: the message the Hub refused is
        // the one an operator will want to read. No XML, because there is
        // no valid rendering of it.
        self.store_message(raw, None, tracking).await;
        let reason = format!(
            "XML validation failed for flow '{}': {outcome}",
            flow.as_str()
        );
        Err(self.reject(raw, message, &reason, Some(&tracking.correlation_id)))
    }

    /// Step 8 — never fatal; the audit copy loses its XML and that is all.
    fn render_xml(&self, raw: &str, correlation_id: &str) -> Option<String> {
        match ihb_validation::er7_to_xml(raw) {
            Ok(xml) => Some(xml),
            Err(error) => {
                let detail = format!(
                    "Failed to generate XML payload for message store: {error} \
                     (CorrelationId: {correlation_id})"
                );
                tracing::error!(%error, "failed to generate XML payload for message store");
                self.events
                    .log_validation_result(raw, &detail, false, Some(correlation_id));
                None
            }
        }
    }

    /// The events, metric and 422 every validation failure shares
    /// (`spec/19-rest-servers.md` §3.2).
    fn reject(
        &self,
        raw: &str,
        message: &Message,
        reason: &str,
        correlation_id: Option<&str>,
    ) -> Failure {
        tracing::error!(reason, "HL7 validation error");
        self.events
            .log_validation_result(raw, reason, false, correlation_id);
        self.events
            .log_message_failed(raw, reason, Some(REJECTED), correlation_id);
        Failure {
            status: 422,
            nack: ack::validation_nack(
                message,
                reason,
                &self.mint.control_id(),
                &self.mint.ack_timestamp(),
            ),
            reason: reason.to_string(),
        }
    }

    /// Step 12 for `risp`: every target, in order.
    ///
    /// `spec/19-rest-servers.md` §3.4 and §6.3 — a failure part-way stops
    /// everything and returns no ACK; a target already sent to is not rolled
    /// back, because a duplicate is the safe failure.
    async fn forward_targets(
        &self,
        raw: &str,
        control_id: &str,
        properties: &Properties,
        targets: &[RoutingTarget],
        correlation_id: &str,
    ) -> Result<(), Failure> {
        for target in targets {
            let (sender, session, workflow) = match target.destination {
                Destination::MpiTransformer => (
                    Some(&self.egress),
                    self.identity.egress_session_id.clone(),
                    self.identity.workflow_id.clone(),
                ),
                Destination::Wrrs => {
                    let wrrs = self.settings.wrrs.as_ref();
                    (
                        self.wrrs.as_ref(),
                        wrrs.map_or_default(|w| w.session_id.clone()),
                        wrrs.map_or_else(
                            || self.identity.workflow_id.clone(),
                            |w| w.workflow_id.clone(),
                        ),
                    )
                }
            };
            let Some(sender) = sender else {
                // Unreachable from a validated configuration (`risp` without
                // `WRRS_*` does not start), but a panic on a clinical path
                // is worse than a 500 that says what is wrong.
                let reason = "No sender client configured for the destination".to_string();
                tracing::error!(destination = ?target.destination, "{reason}");
                return Err(self.not_forwarded(raw, &reason, correlation_id));
            };

            let mut properties = properties.clone();
            properties.insert(WORKFLOW_ID.to_string(), workflow);
            let outgoing = outgoing(&target.payload, control_id, &session, properties);
            self.send(sender, outgoing, raw, control_id, correlation_id)
                .await?;
        }
        Ok(())
    }

    /// One blocking send, awaited to the broker's settlement.
    ///
    /// The caller replies to the sender only after this returns, so a
    /// success here is the only thing that may lead to an ACK: a failure
    /// is a `500` NACK and the source system retries, never a lost message.
    ///
    /// # Errors
    ///
    /// A [`Failure`] (500) when the bus refuses the send; the reason names
    /// the control id and the bus error, not the message body.
    async fn send(
        &self,
        sender: &E,
        outgoing: Outgoing,
        raw: &str,
        control_id: &str,
        correlation_id: &str,
    ) -> Result<(), Failure> {
        match sender.send(outgoing).await {
            Ok(()) => {
                self.metrics.message_sent();
                tracing::info!(
                    control_id,
                    entity = sender.entity(),
                    "message sent to Service Bus"
                );
                Ok(())
            }
            Err(error) => {
                tracing::error!(%error, control_id, entity = sender.entity(),
                    "Failed to send message to Service Bus");
                let reason = format!("Failed to send message {control_id} to Service Bus: {error}");
                Err(self.not_forwarded(raw, &reason, correlation_id))
            }
        }
    }

    /// A 500 with the generic NACK (`spec/19-rest-servers.md` §3.4).
    fn not_forwarded(&self, raw: &str, reason: &str, correlation_id: &str) -> Failure {
        self.events
            .log_message_failed(raw, reason, Some(NOT_FORWARDED), Some(correlation_id));
        Failure {
            status: 500,
            nack: self.generic_nack(reason),
            reason: reason.to_string(),
        }
    }

    /// Step 11 — the audit copy. Never fatal (`spec/19-rest-servers.md`
    /// §3.5).
    async fn store_message(&self, raw: &str, xml: Option<&str>, tracking: &Tracking) {
        let Some(store) = self.store.as_ref() else {
            return;
        };

        let event = StoreEvent {
            message_received_at: tracking.received_at.clone(),
            correlation_id: tracking.correlation_id.clone(),
            source_system: tracking.source_system.clone(),
            processing_component: self.identity.microservice_id.clone(),
            target_system: Some(self.identity.peer_service.clone()),
            raw_payload: raw.to_string(),
            xml_payload: xml.map(ToString::to_string),
            session_id: self.identity.egress_session_id.clone(),
        };

        let json = match event.to_json() {
            Ok(json) => json,
            Err(error) => {
                tracing::error!(%error, "Failed to send to message store");
                return;
            }
        };

        let outgoing = Outgoing::new(json)
            .with_message_id(tracking.correlation_id.clone())
            .with_session(self.identity.egress_session_id.clone());

        if let Err(error) = store.send(outgoing).await {
            tracing::error!(%error, "Failed to send to message store: {error}");
        }
    }
}

/// A message for one destination: the body, the session that keeps the
/// destination's messages in order, and the routing `properties`.
fn outgoing(body: &str, control_id: &str, session: &str, properties: Properties) -> Outgoing {
    let outgoing = Outgoing::new(body)
        .with_session(session.to_string())
        .with_properties(properties);
    if control_id.is_empty() {
        outgoing
    } else {
        outgoing.with_message_id(control_id.to_string())
    }
}

/// `MSH-9` as `ADT^A31^ADT_A05`, trailing empty components dropped.
///
/// A message type is a header code, not a value about a patient, so it may
/// appear in a log line and an event (`spec/19-rest-servers.md` §10.4).
fn message_type_of(message: &Message) -> String {
    let mut parts = [
        read(message, "MSH-9.1"),
        read(message, "MSH-9.2"),
        read(message, "MSH-9.3"),
    ]
    .to_vec();
    while parts.last().is_some_and(String::is_empty) {
        parts.pop();
    }
    parts.join("^")
}

#[cfg(test)]
mod tests {
    use ihb_bus::error::BusError;
    use ihb_bus::fake::{FakeBus, FakeSender};
    use ihb_bus::retry::Disposition;
    use ihb_config::Reader;

    use super::*;

    /// The Python helpers' `VALID_ER7_MESSAGE`.
    const VALID: &str = "MSH|^~\\&|252|252|MPI|MPI|20240101120000||ADT^A28^ADT_A05|MSGID12345|P|2.5\r\
                                    EVN|A28|20240101120000\r\
                                    PID|1||123456^^^NHS||SMITH^JOHN||19800101|M";
    const A28: &str = "MSH|^~\\&|349|349|MPI|MPI|20240101120000||ADT^A28^ADT_A05|RISPMSG001|P|2.5.1\r\
                       EVN|A28|20240101120000\r\
                       PID|1||654321^^^NHS||DOE^JANE||19900101|F";
    const A40: &str = "MSH|^~\\&|349|349|MPI|MPI|20240101120000||ADT^A40^ADT_A39|RISPMSG002|P|2.5.1\r\
                       EVN|A40|20240101120000\r\
                       PID|1||654321^^^NHS||DOE^JANE||19900101|F\r\
                       MRG|123456^^^NHS";
    const ORU: &str = "MSH|^~\\&|350|350|MPI|MPI|20240101120000||ORU^R01^ORU_R01|RISPMSG003|P|2.5.1\r\
                       PID|1||654321^^^NHS||DOE^JANE||19900101|F\r\
                       PV1|1|I\r\
                       OBR|1||ORDER1||\r\
                       OBX|1|ST|TEST||RESULT";

    struct Fixed;
    impl Mint for Fixed {
        fn correlation_id(&self) -> String {
            "fixed-correlation-id".into()
        }
        fn received_at(&self) -> String {
            "2026-08-20T09:15:00+00:00".into()
        }
        fn ack_timestamp(&self) -> String {
            "20260820091500".into()
        }
        fn control_id(&self) -> String {
            "GENERATED".into()
        }
    }

    fn down(destination: &str) -> BusError {
        BusError::Send {
            destination: destination.into(),
            attempts: 1,
            reason: "down".into(),
            disposition: Disposition::Fatal,
        }
    }

    struct Rig {
        processor: Processor<FakeSender, FakeSender, Fixed>,
        bus: FakeBus,
    }

    fn rig(extra: &[(&str, &str)]) -> Rig {
        let mut pairs: Vec<(String, String)> = [
            ("WORKFLOW_ID", "test-workflow"),
            ("MICROSERVICE_ID", "hl7_rest_server"),
            ("HEALTH_BOARD", "TEST"),
            ("PEER_SERVICE", "MPI"),
            ("EGRESS_QUEUE_NAME", "egress-queue"),
            ("EGRESS_SESSION_ID", "session"),
            ("MESSAGE_STORE_QUEUE_NAME", "store-queue"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (key, value) in extra {
            pairs.retain(|(name, _)| name != key);
            pairs.push(((*key).to_string(), (*value).to_string()));
        }

        let mut reader = Reader::from_pairs(pairs);
        let identity = Identity::read(&mut reader);
        let settings = Hl7Settings::read(&mut reader);
        reader.finish().unwrap();

        let bus = FakeBus::new();
        let wrrs = settings
            .wrrs
            .as_ref()
            .map(|w| bus.sender(w.egress.entity()));
        let processor = Processor::with_mint(
            identity,
            settings,
            bus.sender("egress-queue"),
            wrrs,
            Some(bus.sender("store-queue")),
            Fixed,
        );
        Rig { processor, bus }
    }

    fn risp() -> Rig {
        rig(&[
            ("HL7_VALIDATION_FLOW", "risp"),
            ("WRRS_QUEUE_NAME", "wrrs-queue"),
            ("WRRS_EGRESS_SESSION_ID", "risp-to-wrrs"),
            ("WRRS_WORKFLOW_ID", "risp-to-wrrs"),
        ])
    }

    #[tokio::test]
    async fn a_valid_message_is_stored_then_sent_then_acknowledged() {
        // test_valid_message_sends_to_store_then_service_bus;
        // spec/19 §3 steps 11 to 13.
        let rig = rig(&[]);
        let ack = rig.processor.process(VALID).await.unwrap();

        assert!(ack.contains("MSA|AA|MSGID12345"), "{ack}");
        assert_eq!(rig.bus.all_sent("egress-queue").len(), 1);
        assert_eq!(rig.bus.all_sent("store-queue").len(), 1);

        let sent = &rig.bus.all_sent("egress-queue")[0];
        assert_eq!(sent.text().unwrap(), VALID, "forwarded as received");
        assert_eq!(sent.message_id.as_deref(), Some("MSGID12345"));
        assert_eq!(sent.session_id.as_deref(), Some("session"));
        assert_eq!(sent.properties["CorrelationId"], "fixed-correlation-id");
        assert_eq!(sent.properties["SourceSystem"], "252");
        assert_eq!(sent.properties["WorkflowID"], "test-workflow");
    }

    #[tokio::test]
    async fn the_store_copy_carries_the_xml_and_the_raw_message() {
        // spec/19 §3 step 8 and 11.
        let rig = rig(&[]);
        rig.processor.process(VALID).await.unwrap();

        let stored = rig.bus.all_sent("store-queue")[0].text().unwrap();
        let event = StoreEvent::from_json(&stored).unwrap();
        assert_eq!(event.raw_payload, VALID);
        assert!(event.xml_payload.unwrap().contains("urn:hl7-org:v2xml"));
        assert_eq!(event.session_id, "session");
    }

    #[tokio::test]
    async fn an_unparsable_message_is_a_500_with_a_generic_ae_and_no_send() {
        // test_unparsable_message_raises_parse_error; spec/19 §3.1.
        let rig = rig(&[]);
        let failure = rig
            .processor
            .process("not a valid hl7 message")
            .await
            .unwrap_err();

        assert_eq!(failure.status, 500);
        assert!(
            failure.nack.contains("MSA|AE|GENERATED|"),
            "{}",
            failure.nack
        );
        assert_eq!(rig.bus.all_sent("egress-queue").len(), 0);
    }

    #[tokio::test]
    async fn a_version_mismatch_is_a_422_with_an_ar_and_no_send() {
        // test_version_mismatch_raises_validation_error; spec/19 §3.2.
        let rig = rig(&[("HL7_VERSION", "2.9")]);
        let failure = rig.processor.process(VALID).await.unwrap_err();

        assert_eq!(failure.status, 422);
        assert!(
            failure
                .nack
                .contains("MSA|AR|MSGID12345|Message has wrong version")
        );
        assert_eq!(rig.bus.all_sent("egress-queue").len(), 0);
    }

    #[tokio::test]
    async fn a_store_failure_does_not_stop_the_message() {
        // test_store_failure_is_non_blocking; spec/19 §3.5.
        let rig = rig(&[]);
        rig.bus.fail_next_send(down("store-queue"));
        let ack = rig.processor.process(VALID).await.unwrap();

        assert!(ack.contains("MSA|AA|"));
        assert_eq!(rig.bus.all_sent("egress-queue").len(), 1);
    }

    /// A processor with no message store, so a fake failure lands on the
    /// egress send rather than the store's.
    fn without_store(
        extra: &[(&str, &str)],
    ) -> (Processor<FakeSender, FakeSender, Fixed>, FakeBus) {
        let mut pairs: Vec<(String, String)> = [
            ("WORKFLOW_ID", "w"),
            ("MICROSERVICE_ID", "m"),
            ("HEALTH_BOARD", "h"),
            ("PEER_SERVICE", "p"),
            ("EGRESS_QUEUE_NAME", "egress-queue"),
            ("EGRESS_SESSION_ID", "s"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        pairs.extend(
            extra
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
        );
        let mut reader = Reader::from_pairs(pairs);
        let identity = Identity::read(&mut reader);
        let settings = Hl7Settings::read(&mut reader);
        reader.finish().unwrap();
        let bus = FakeBus::new();
        // Deliberately no WRRS sender even when `risp` is configured: the
        // "not configured" path below.
        let processor = Processor::with_mint(
            identity,
            settings,
            bus.sender("egress-queue"),
            None,
            None,
            Fixed,
        );
        (processor, bus)
    }

    #[tokio::test]
    async fn without_a_store_queue_nothing_is_stored() {
        // spec/19 §3.5.
        let (processor, bus) = without_store(&[]);
        processor.process(VALID).await.unwrap();
        assert_eq!(bus.all_sent("egress-queue").len(), 1);
        assert_eq!(bus.all_sent("store-queue").len(), 0);
    }

    #[tokio::test]
    async fn a_service_bus_failure_is_a_500_and_there_is_no_ack() {
        // test_service_bus_failure_propagates; spec/19 §2.3 and §3.4 —
        // the rule that must never be broken.
        let (processor, bus) = without_store(&[]);
        bus.fail_next_send(down("egress-queue"));
        let failure = processor.process(VALID).await.unwrap_err();

        assert_eq!(failure.status, 500);
        assert!(failure.nack.contains("MSA|AE|"), "{}", failure.nack);
        assert!(!failure.nack.contains("MSA|AA|"));
        assert_eq!(bus.all_sent("egress-queue").len(), 0);
    }

    #[tokio::test]
    async fn a_failed_second_target_returns_no_ack_and_does_not_unsend_the_first() {
        // spec/19 §3.4 — a duplicate is the safe failure; a lost message is not.
        let (processor, bus) = without_store(&[
            ("HL7_VALIDATION_FLOW", "risp"),
            ("WRRS_QUEUE_NAME", "wrrs-queue"),
            ("WRRS_EGRESS_SESSION_ID", "x"),
            ("WRRS_WORKFLOW_ID", "x"),
        ]);
        let failure = processor.process(A40).await.unwrap_err();

        assert_eq!(failure.status, 500);
        assert!(failure.nack.contains("MSA|AE|"), "{}", failure.nack);
        assert_eq!(bus.all_sent("egress-queue").len(), 1, "not rolled back");
    }

    #[tokio::test]
    async fn the_flow_schema_rejects_and_stores_a_copy_without_xml() {
        // spec/19 §3.3.
        let rig = rig(&[("HL7_VALIDATION_FLOW", "phw")]);
        let missing_pid = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
                           EVN||20250502092900\rPV1||U";
        let failure = rig.processor.process(missing_pid).await.unwrap_err();

        assert_eq!(failure.status, 422);
        assert!(
            failure
                .reason
                .starts_with("XML validation failed for flow 'phw': "),
            "{}",
            failure.reason
        );
        assert_eq!(rig.bus.all_sent("egress-queue").len(), 0);

        let stored =
            StoreEvent::from_json(&rig.bus.all_sent("store-queue")[0].text().unwrap()).unwrap();
        assert!(stored.xml_payload.is_none(), "no valid rendering of it");
        assert_eq!(stored.raw_payload, missing_pid);
    }

    #[tokio::test]
    async fn the_standard_release_is_checked_after_the_store_has_its_xml() {
        // spec/19 §3.6 — a wrong release is a validation failure.
        let rig = rig(&[("HL7_VALIDATION_STANDARD", "2.6")]);
        let failure = rig.processor.process(VALID).await.unwrap_err();
        assert_eq!(failure.status, 422);
        assert!(
            failure.reason.starts_with("Standard validation error: "),
            "{}",
            failure.reason
        );
    }

    #[tokio::test]
    async fn mpi_adds_its_routing_properties() {
        // spec/19 §7.1.
        let rig = rig(&[("HL7_VALIDATION_FLOW", "mpi")]);
        let message = "MSH|^~\\&|100|100|252|252|2024||ADT^A31^ADT_A05|M1|P|2.5\r\
                       PID|1|8888888^^^252^PI|4444444444^^^NHS^NH";
        rig.processor.process(message).await.unwrap();

        let properties = &rig.bus.all_sent("egress-queue")[0].properties;
        assert_eq!(properties["MessageType"], "A31");
        assert_eq!(properties["UpdateSources"], "|252|");
    }

    // ---- RISP ---------------------------------------------------------

    #[tokio::test]
    async fn a28_goes_to_the_mpi_transformer_only() {
        // test_a28_sends_only_to_mpi_transformer
        let rig = risp();
        let ack = rig.processor.process(A28).await.unwrap();

        assert!(ack.contains("MSA|AA|RISPMSG001"), "{ack}");
        assert_eq!(rig.bus.all_sent("egress-queue").len(), 1);
        assert_eq!(rig.bus.all_sent("wrrs-queue").len(), 0);
    }

    #[tokio::test]
    async fn a40_goes_to_both_with_each_destinations_workflow_and_session() {
        // test_a40_sends_to_both_mpi_transformer_and_wrrs; spec/19 §6.3.
        let rig = risp();
        let ack = rig.processor.process(A40).await.unwrap();
        assert!(ack.contains("MSA|AA|RISPMSG002"), "{ack}");

        let mpi = &rig.bus.all_sent("egress-queue")[0];
        assert_eq!(mpi.text().unwrap(), A40, "the raw ER7");
        assert_eq!(mpi.properties["WorkflowID"], "test-workflow");
        assert_eq!(mpi.session_id.as_deref(), Some("session"));

        let wrrs = &rig.bus.all_sent("wrrs-queue")[0];
        assert!(wrrs.text().unwrap().contains("<ADT_A39"));
        assert_eq!(wrrs.properties["WorkflowID"], "risp-to-wrrs");
        assert_eq!(wrrs.session_id.as_deref(), Some("risp-to-wrrs"));
        // The same tracking, so the two copies are one message downstream.
        assert_eq!(
            wrrs.properties["CorrelationId"],
            mpi.properties["CorrelationId"]
        );

        // spec/19 §6.2 — the XML target's payload is the store's copy.
        let stored =
            StoreEvent::from_json(&rig.bus.all_sent("store-queue")[0].text().unwrap()).unwrap();
        assert_eq!(
            stored.xml_payload.as_deref(),
            Some(wrrs.text().unwrap().as_str())
        );
    }

    #[tokio::test]
    async fn risp_messages_carry_the_trigger_and_structure_on_every_target() {
        // spec/19 §7.2.
        let rig = risp();
        rig.processor.process(A40).await.unwrap();

        for entity in ["egress-queue", "wrrs-queue"] {
            let properties = &rig.bus.all_sent(entity)[0].properties;
            assert_eq!(properties["MessageType"], "A40", "{entity}");
            assert_eq!(properties["MessageStructure"], "ADT_A39", "{entity}");
        }
    }

    #[tokio::test]
    async fn oru_r01_goes_to_wrrs_only() {
        // test_oru_r01_sends_only_to_wrrs_after_schema_validation
        let rig = risp();
        let ack = rig.processor.process(ORU).await.unwrap();

        assert!(ack.contains("MSA|AA|RISPMSG003"), "{ack}");
        assert_eq!(rig.bus.all_sent("egress-queue").len(), 0);
        assert_eq!(rig.bus.all_sent("wrrs-queue").len(), 1);
    }

    #[tokio::test]
    async fn a_wrong_facility_is_a_422_and_nothing_is_sent() {
        // test_wrong_sending_facility_rejects_with_no_sends
        let rig = risp();
        let failure = rig
            .processor
            .process(&A28.replace("|349|349|", "|999|999|"))
            .await
            .unwrap_err();

        assert_eq!(failure.status, 422);
        assert_eq!(rig.bus.all_sent("egress-queue").len(), 0);
        assert_eq!(rig.bus.all_sent("wrrs-queue").len(), 0);
    }
}
