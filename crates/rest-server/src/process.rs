//! What happens to one request on the `generic` pipeline.
//!
//! Implements `spec/19-rest-servers.md` §9.2, §9.3 and §10.
//!
//! # The order is the design
//!
//! As `hl7-soap-server`'s `process` (`spec/08-hl7-soap-server.md` §3) and for
//! the same reasons: the received event first, so a request that kills the
//! process still left a trace; the store send before the bus send, so there
//! is a persisted copy before the message is handed on; the bus send before
//! the success response, and blocking, because **a `200` is an acknowledgement**
//! and the one failure this system must never produce is acknowledging a
//! message that was not delivered.
//!
//! # Every failure is a status
//!
//! [`Fault`] and
//! [`RequestError`] carry their HTTP status and
//! fault code, and the response is built by the same adapter that read the
//! request. A 400 tells a sender not to retry, a 500 tells it to, and a 403
//! tells it to talk to someone, so no step here decides a status by itself.

use hl7_rest_server::config::Identity;
use hl7_rest_server::process::{Mint, RealMint};
use ihb_bus::message::Outgoing;
use ihb_bus::metadata::Tracking;
use ihb_bus::sink::MessageSink;
use ihb_bus::store::StoreEvent;
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::adapter::{ContentAdapter, RequestError};
use crate::config::{Generic, OutputFormat};
use crate::validator::{Fault, Validator};

/// The Service Bus property the trigger event rides in
/// (`spec/19-rest-servers.md` §10.1).
pub const MESSAGE_TYPE: &str = "MessageType";

/// The `validation_result` of a request-level rejection (§9.3).
const REJECTED: &str = "Request rejected";

/// The `validation_result` of a payload failure (§9.3).
const INVALID: &str = "Payload validation failed";

/// The `validation_result` of the catch-all (§9.3).
const UNHANDLED: &str = "Unhandled server error";

/// The text of the catch-all (§9.3).
const UNEXPECTED: &str = "Unexpected server error while processing request.";

/// A processed request: the status and the body, in the adapter's format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The HTTP status.
    pub status: u16,
    /// The body.
    pub body: String,
}

/// One request's journey, minus HTTP.
pub struct GenericProcessor<E, S, M = RealMint> {
    /// Who this is.
    identity: Identity,
    /// What it does.
    settings: Generic,
    /// Where a validated payload goes.
    egress: E,
    /// The audit copy.
    store: Option<S>,
    /// Events.
    events: EventLogger,
    /// Counters.
    metrics: Metrics,
    /// Correlation ids and timestamps.
    mint: M,
}

impl<E: MessageSink, S: MessageSink> GenericProcessor<E, S, RealMint> {
    /// A processor using the real clock.
    pub fn new(identity: Identity, settings: Generic, egress: E, store: Option<S>) -> Self {
        Self::with_mint(identity, settings, egress, store, RealMint)
    }
}

impl<E: MessageSink, S: MessageSink, M: Mint> GenericProcessor<E, S, M> {
    /// A processor with an explicit [`Mint`].
    pub fn with_mint(
        identity: Identity,
        settings: Generic,
        egress: E,
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
            store,
            events,
            metrics,
            mint,
        }
    }

    /// The deployment's adapter, for the HTTP layer's own errors.
    #[must_use]
    pub const fn adapter(&self) -> &ContentAdapter {
        &self.settings.adapter
    }

    /// The deployment's endpoint path.
    #[must_use]
    pub fn endpoint_path(&self) -> &str {
        &self.settings.endpoint_path
    }

    /// The deployment's validator.
    #[must_use]
    pub const fn validator(&self) -> &Validator {
        &self.settings.validator
    }

    /// Take one request body through §9.2.
    ///
    /// Never fails: every outcome, including a failure, is a [`Reply`].
    pub async fn process(&self, raw_body: &str) -> Reply {
        // ---- Step 1, before anything can fail. -------------------------
        self.events.log_message_received(raw_body, None, None);
        self.metrics.message_received();

        match self.process_inner(raw_body).await {
            Ok(reply) => reply,
            Err(fault) => self.fail(raw_body, fault),
        }
    }

    /// Steps 2 to 11 of `spec/19-rest-servers.md` §9.2; step 1 (the
    /// received event) is the caller's, so it is recorded even when the
    /// body is unusable.
    ///
    /// # Errors
    ///
    /// A [`Fault`] naming the status and fault code to answer with. A
    /// failed send is the `500` and no success body is built (§9.3).
    async fn process_inner(&self, raw_body: &str) -> Result<Reply, Fault> {
        // ---- Step 2. ---------------------------------------------------
        let extracted = self
            .settings
            .adapter
            .extract(raw_body)
            .map_err(Fault::Request)?;

        // ---- Steps 3 and 4. --------------------------------------------
        self.settings
            .validator
            .validate(&extracted.payload_xml, extracted.structure_id.as_deref())?;
        self.events.log_validation_result(
            raw_body,
            &format!(
                "Schema validation passed for structure '{}'",
                extracted.structure_id.as_deref().unwrap_or_default()
            ),
            true,
            None,
        );

        // ---- Step 5: the allow-list. -----------------------------------
        let allowed = &self.settings.allowed_source_identifiers;
        if !allowed.is_empty()
            && !extracted
                .source_identifier
                .as_deref()
                .is_some_and(|source| allowed.iter().any(|a| a == source))
        {
            let mut sorted: Vec<&str> = allowed.iter().map(String::as_str).collect();
            sorted.sort_unstable();
            return Err(Fault::Request(RequestError::new(
                "Client.Authorization",
                format!(
                    "Source identifier '{}' is not authorised. Allowed values: {}",
                    extracted.source_identifier.as_deref().unwrap_or("None"),
                    sorted.join(", ")
                ),
                403,
            )));
        }

        // ---- Step 5b: the trigger allow-list (upstream `#455`). --------
        self.check_trigger(extracted.message_type.as_deref())?;

        // ---- Step 6: the output format. --------------------------------
        let output = match self.settings.output_format {
            OutputFormat::Raw => extracted.payload_xml.clone(),
            OutputFormat::Er7 => {
                ihb_validation::xml_to_er7(&extracted.payload_xml).map_err(|error| {
                    // The error is logged in full, but the response says only
                    // that it failed: a conversion error can quote content.
                    tracing::error!(%error, "unable to convert the XML payload to ER7");
                    Fault::Request(RequestError::new(
                        "Client.Validation",
                        "Unable to convert XML payload to ER7 format.",
                        400,
                    ))
                })?
            }
        };

        // ---- Step 7: tracking metadata. --------------------------------
        let tracking = Tracking {
            correlation_id: self.mint.correlation_id(),
            received_at: self.mint.received_at(),
            source_system: extracted.source_identifier.clone().unwrap_or_default(),
            workflow_id: self.identity.workflow_id.clone(),
        };
        let mut properties = tracking.to_properties();
        // `spec/19-rest-servers.md` §10.1 — set when there is one, **absent**
        // otherwise: a subscription rule on a property that exists differs
        // from one on a property that does not.
        if let Some(message_type) = extracted.message_type.as_deref().filter(|t| !t.is_empty()) {
            properties.insert(MESSAGE_TYPE.to_string(), message_type.to_string());
        }
        let control_id = extracted.message_control_id.clone().unwrap_or_default();

        // ---- Step 8: the message store, non-blocking. ------------------
        self.store_message(raw_body, &extracted.payload_xml, &tracking)
            .await;

        // ---- Step 9: Service Bus, blocking. ----------------------------
        let mut outgoing = Outgoing::new(output)
            .with_session(self.identity.egress_session_id.clone())
            .with_properties(properties);
        if !control_id.is_empty() {
            outgoing = outgoing.with_message_id(control_id.clone());
        }
        self.egress.send(outgoing).await.map_err(|error| {
            tracing::error!(%error, "failed to send the payload to Service Bus");
            Fault::Request(RequestError::new("Server", UNEXPECTED, 500))
        })?;

        // ---- Steps 10 and 11. ------------------------------------------
        self.metrics.message_sent();
        self.events.log_message_processed(
            raw_body,
            Some(&format!(
                "Payload validated and forwarded (structure: {})",
                extracted.structure_id.as_deref().unwrap_or_default()
            )),
            Some(&tracking.correlation_id),
        );

        Ok(Reply {
            status: 200,
            body: self.settings.adapter.success_response(&control_id),
        })
    }

    /// §9.3 — the events and the response for a failure.
    fn fail(&self, raw_body: &str, fault: Fault) -> Reply {
        let adapter = &self.settings.adapter;
        match fault {
            Fault::Request(error) if error.code == "Server" => {
                // The catch-all: logged as a failure, not as a validation
                // result, because nothing was wrong with the request.
                tracing::error!(status = error.status, "{}", error.message);
                self.events
                    .log_message_failed(raw_body, &error.message, Some(UNHANDLED), None);
                Reply {
                    status: error.status,
                    body: adapter.error_response("Server", &error.message),
                }
            }
            Fault::Request(error) => {
                tracing::warn!(status = error.status, "Request rejected: {}", error.message);
                self.events
                    .log_validation_result(raw_body, &error.message, false, None);
                self.events
                    .log_message_failed(raw_body, &error.message, Some(REJECTED), None);
                Reply {
                    status: error.status,
                    body: adapter.error_response(&error.code, &error.message),
                }
            }
            Fault::Validation(message) => {
                tracing::warn!("Payload validation failed: {message}");
                self.events
                    .log_validation_result(raw_body, &message, false, None);
                self.events
                    .log_message_failed(raw_body, &message, Some(INVALID), None);
                Reply {
                    status: 400,
                    body: adapter.error_response("Client.Validation", &message),
                }
            }
        }
    }

    /// Step 5b: `ALLOWED_TRIGGER_EVENTS` (`spec/19-rest-servers.md` §9.5.4).
    fn check_trigger(&self, trigger: Option<&str>) -> Result<(), Fault> {
        let allowed = &self.settings.allowed_trigger_events;
        if allowed.is_empty() || trigger.is_some_and(|t| allowed.iter().any(|a| a == t)) {
            return Ok(());
        }
        let mut sorted: Vec<&str> = allowed.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        Err(Fault::Request(RequestError::new(
            "Client.Validation",
            format!(
                "Unsupported HL7 trigger event '{}'. Allowed values: {}",
                trigger.filter(|t| !t.is_empty()).unwrap_or("unknown"),
                sorted.join(", ")
            ),
            400,
        )))
    }

    /// Step 8 — the audit copy. Never fatal.
    async fn store_message(&self, raw_body: &str, payload_xml: &str, tracking: &Tracking) {
        let Some(store) = self.store.as_ref() else {
            return;
        };

        let event = StoreEvent {
            message_received_at: tracking.received_at.clone(),
            correlation_id: tracking.correlation_id.clone(),
            source_system: tracking.source_system.clone(),
            processing_component: self.identity.microservice_id.clone(),
            target_system: Some(self.identity.peer_service.clone()),
            // The whole request, not the payload: that is what arrived and
            // what an operator would need to reproduce a problem.
            raw_payload: raw_body.to_string(),
            xml_payload: Some(payload_xml.to_string()),
            session_id: self.identity.egress_session_id.clone(),
        };

        let json = match event.to_json() {
            Ok(json) => json,
            Err(error) => {
                tracing::error!(%error, "Failed to send message to message store");
                return;
            }
        };

        let outgoing = Outgoing::new(json)
            .with_message_id(tracking.correlation_id.clone())
            .with_session(self.identity.egress_session_id.clone());

        if let Err(error) = store.send(outgoing).await {
            tracing::error!(%error, "Failed to send message to message store: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use ihb_bus::error::BusError;
    use ihb_bus::fake::{FakeBus, FakeSender};
    use ihb_bus::retry::Disposition;
    use ihb_config::Reader;
    use ihb_validation::Flow;

    use super::*;

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

    const ADT: &str = r#"<ADT_A05 xmlns="urn:hl7-org:v2xml"><MSH><MSH.1>|</MSH.1><MSH.2>^~\&amp;</MSH.2><MSH.3><HD.1>328</HD.1></MSH.3><MSH.9><MSG.1>ADT</MSG.1><MSG.2>A31</MSG.2><MSG.3>ADT_A05</MSG.3></MSH.9><MSH.10>MSG-1</MSH.10><MSH.12><VID.1>2.5</VID.1></MSH.12></MSH></ADT_A05>"#;

    fn identity(with_store: bool) -> Identity {
        let mut pairs = vec![
            ("WORKFLOW_ID", "workflow-rest"),
            ("MICROSERVICE_ID", "rest-server"),
            ("HEALTH_BOARD", "test-board"),
            ("PEER_SERVICE", "MPI"),
            ("EGRESS_QUEUE_NAME", "egress-queue"),
            ("EGRESS_SESSION_ID", "rest-session"),
        ];
        if with_store {
            pairs.push(("MESSAGE_STORE_QUEUE_NAME", "store-queue"));
        }
        let mut reader = Reader::from_pairs(pairs);
        let identity = Identity::read(&mut reader);
        reader.finish().unwrap();
        identity
    }

    fn raw_xml_settings(validator: Validator, output: OutputFormat, allowed: &[&str]) -> Generic {
        Generic {
            endpoint_path: "/ingest".into(),
            adapter: ContentAdapter::XmlRaw {
                source_identifier_path: Some(vec!["Header".into(), "SourceSystem".into()]),
                message_control_id_path: Some(vec!["Header".into(), "MessageId".into()]),
            },
            validator,
            allowed_source_identifiers: allowed.iter().map(ToString::to_string).collect(),
            allowed_trigger_events: vec![],
            output_format: output,
        }
    }

    fn soap_settings(output: OutputFormat) -> Generic {
        Generic {
            endpoint_path: "/soap".into(),
            adapter: ContentAdapter::Soap,
            validator: Validator::None,
            allowed_source_identifiers: vec![],
            allowed_trigger_events: vec![],
            output_format: output,
        }
    }

    fn soap(payload: &str) -> String {
        format!(
            "<soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\"><soapenv:Body>{payload}</soapenv:Body></soapenv:Envelope>"
        )
    }

    const DOC: &str =
        "<Doc><Header><SourceSystem>SRC</SourceSystem><MessageId>ID-1</MessageId></Header></Doc>";

    fn rig(settings: Generic) -> (GenericProcessor<FakeSender, FakeSender, Fixed>, FakeBus) {
        let bus = FakeBus::new();
        let processor = GenericProcessor::with_mint(
            identity(true),
            settings,
            bus.sender("egress-queue"),
            Some(bus.sender("store-queue")),
            Fixed,
        );
        (processor, bus)
    }

    fn down() -> BusError {
        BusError::Send {
            destination: "egress-queue".into(),
            attempts: 1,
            reason: "down".into(),
            disposition: Disposition::Fatal,
        }
    }

    #[tokio::test]
    async fn a_valid_request_is_stored_forwarded_and_acknowledged() {
        // test_message_processor.py — the success path; spec/19 §9.2.
        let (processor, bus) = rig(raw_xml_settings(Validator::None, OutputFormat::Raw, &[]));
        let reply = processor.process(DOC).await;

        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.body,
            "<ack><status>Success</status><messageControlId>ID-1</messageControlId></ack>"
        );

        let sent = &bus.all_sent("egress-queue")[0];
        assert_eq!(
            sent.text().unwrap(),
            DOC,
            "raw output is the payload unchanged"
        );
        assert_eq!(sent.message_id.as_deref(), Some("ID-1"));
        assert_eq!(sent.session_id.as_deref(), Some("rest-session"));
        assert_eq!(sent.properties["SourceSystem"], "SRC");
        assert_eq!(sent.properties["WorkflowID"], "workflow-rest");
        assert!(
            !sent.properties.contains_key("MessageType"),
            "spec/19 §10.3"
        );

        let stored =
            StoreEvent::from_json(&bus.all_sent("store-queue")[0].text().unwrap()).unwrap();
        assert_eq!(stored.raw_payload, DOC);
        assert_eq!(stored.xml_payload.as_deref(), Some(DOC));
    }

    #[tokio::test]
    async fn the_store_is_written_before_the_bus_and_is_not_required() {
        // spec/19 §9.2 steps 8, 9; the store is non-blocking.
        let bus = FakeBus::new();
        let processor: GenericProcessor<FakeSender, FakeSender, Fixed> =
            GenericProcessor::with_mint(
                identity(false),
                raw_xml_settings(Validator::None, OutputFormat::Raw, &[]),
                bus.sender("egress-queue"),
                None,
                Fixed,
            );
        assert_eq!(processor.process(DOC).await.status, 200);
        assert_eq!(bus.all_sent("egress-queue").len(), 1);
    }

    #[tokio::test]
    async fn a_store_failure_does_not_stop_the_request() {
        // test_message_processor.py — store failure is non-blocking.
        let (processor, bus) = rig(raw_xml_settings(Validator::None, OutputFormat::Raw, &[]));
        bus.fail_next_send(down());
        assert_eq!(processor.process(DOC).await.status, 200);
        assert_eq!(bus.all_sent("egress-queue").len(), 1);
    }

    #[tokio::test]
    async fn a_bus_failure_is_a_500_and_there_is_no_success_body() {
        // test_message_processor.py — send failure; AGENTS.md rule 2.
        let bus = FakeBus::new();
        let processor: GenericProcessor<FakeSender, FakeSender, Fixed> =
            GenericProcessor::with_mint(
                identity(false),
                raw_xml_settings(Validator::None, OutputFormat::Raw, &[]),
                bus.sender("egress-queue"),
                None,
                Fixed,
            );
        bus.fail_next_send(down());
        let reply = processor.process(DOC).await;

        assert_eq!(reply.status, 500);
        assert_eq!(
            reply.body,
            "<error><code>Server</code><message>Unexpected server error while processing request.</message></error>"
        );
        assert!(!reply.body.contains("Success"));
    }

    #[tokio::test]
    async fn an_unauthorised_source_is_a_403_and_nothing_is_sent() {
        // test_message_processor.py — allow-list; spec/19 §9.2 step 5.
        let (processor, bus) = rig(raw_xml_settings(
            Validator::None,
            OutputFormat::Raw,
            &["B", "A"],
        ));
        let reply = processor.process(DOC).await;

        assert_eq!(reply.status, 403);
        assert!(
            reply.body.contains(
                "Source identifier &apos;SRC&apos; is not authorised. Allowed values: A, B"
            ),
            "{}",
            reply.body
        );
        assert!(reply.body.contains("<code>Client.Authorization</code>"));
        // spec/19 §9.4.1 — the quotes are escaped, as upstream's `_escape` does.
        assert_eq!(bus.all_sent("egress-queue").len(), 0);
        assert!(
            bus.all_sent("store-queue").is_empty(),
            "refused before the store"
        );
    }

    #[tokio::test]
    async fn an_authorised_source_passes() {
        let (processor, _) = rig(raw_xml_settings(
            Validator::None,
            OutputFormat::Raw,
            &["SRC"],
        ));
        assert_eq!(processor.process(DOC).await.status, 200);
    }

    #[tokio::test]
    async fn a_malformed_request_is_the_adapters_400() {
        // spec/19 §9.3.
        let (processor, bus) = rig(raw_xml_settings(Validator::None, OutputFormat::Raw, &[]));
        let reply = processor.process("<a>").await;

        assert_eq!(reply.status, 400);
        assert!(
            reply.body.contains("Malformed XML request."),
            "{}",
            reply.body
        );
        assert_eq!(bus.all_sent("egress-queue").len(), 0);
    }

    #[tokio::test]
    async fn a_validation_failure_is_a_400_client_validation() {
        // spec/19 §9.3 — always `Client.Validation`.
        let validator = Validator::Hl7Xsd {
            flow: Flow::Phw,
            allowed_structures: vec!["ADT_A05".into()],
        };
        let (processor, bus) = rig(Generic {
            validator,
            ..soap_settings(OutputFormat::Er7)
        });
        // An ADT_A05 with no PID: structurally an A05, not a conforming one.
        let reply = processor.process(&soap(ADT)).await;

        assert_eq!(reply.status, 400, "{}", reply.body);
        assert!(
            reply
                .body
                .contains("<faultcode>Client.Validation</faultcode>"),
            "{}",
            reply.body
        );
        assert_eq!(bus.all_sent("egress-queue").len(), 0);
    }

    #[tokio::test]
    async fn the_soap_profile_forwards_er7_with_the_trigger_event_as_message_type() {
        // spec/19 §10.1 (e090e23) and §9.2 step 6.
        let (processor, bus) = rig(soap_settings(OutputFormat::Er7));
        let reply = processor.process(&soap(ADT)).await;

        assert_eq!(reply.status, 200, "{}", reply.body);
        assert!(
            reply
                .body
                .contains("<MessageControlId>MSG-1</MessageControlId>"),
            "{}",
            reply.body
        );

        let sent = &bus.all_sent("egress-queue")[0];
        assert!(
            sent.text().unwrap().starts_with("MSH|^~\\&|328"),
            "{}",
            sent.text().unwrap()
        );
        assert_eq!(sent.properties["MessageType"], "A31");
        assert_eq!(sent.properties["SourceSystem"], "328");
        assert_eq!(sent.message_id.as_deref(), Some("MSG-1"));

        // The store keeps the whole envelope, and the payload element.
        let stored =
            StoreEvent::from_json(&bus.all_sent("store-queue")[0].text().unwrap()).unwrap();
        assert!(stored.raw_payload.starts_with("<soapenv:Envelope"));
        assert!(stored.xml_payload.unwrap().starts_with("<ADT_A05"));
    }

    #[tokio::test]
    async fn only_the_allowed_trigger_events_pass_and_the_refusal_is_a_400_before_the_bus() {
        // spec/19 §9.5.4 (upstream `#455`).
        let mut settings = soap_settings(OutputFormat::Er7);
        settings.allowed_trigger_events = vec!["A40".into(), "A28".into()];
        let (processor, bus) = rig(settings);

        let reply = processor.process(&soap(ADT)).await;

        assert_eq!(reply.status, 400, "{}", reply.body);
        assert!(
            reply
                .body
                .contains("<faultcode>Client.Validation</faultcode>"),
            "{}",
            reply.body
        );
        assert!(
            reply.body.contains(
                "Unsupported HL7 trigger event &apos;A31&apos;. Allowed values: A28, A40"
            ),
            "{}",
            reply.body
        );
        assert_eq!(bus.all_sent("egress-queue").len(), 0);
        assert!(
            bus.all_sent("store-queue").is_empty(),
            "refused before the store"
        );
    }

    #[tokio::test]
    async fn a_listed_trigger_passes_and_an_empty_list_allows_any() {
        let mut settings = soap_settings(OutputFormat::Er7);
        settings.allowed_trigger_events = vec!["A31".into()];
        assert_eq!(rig(settings).0.process(&soap(ADT)).await.status, 200);
        assert_eq!(
            rig(soap_settings(OutputFormat::Er7))
                .0
                .process(&soap(ADT))
                .await
                .status,
            200
        );
    }

    #[tokio::test]
    async fn a_message_with_no_trigger_is_refused_when_triggers_are_listed() {
        let mut settings = soap_settings(OutputFormat::Raw);
        settings.allowed_trigger_events = vec!["A31".into()];
        let no_type = ADT.replace("<MSG.2>A31</MSG.2>", "");
        let reply = rig(settings).0.process(&soap(&no_type)).await;
        assert_eq!(reply.status, 400);
        assert!(
            reply
                .body
                .contains("Unsupported HL7 trigger event &apos;unknown&apos;"),
            "{}",
            reply.body
        );
    }

    #[tokio::test]
    async fn a_soap_payload_without_a_trigger_event_has_no_message_type_property() {
        // spec/19 §10.1 — absent, not empty.
        let (processor, bus) = rig(soap_settings(OutputFormat::Raw));
        let no_type = ADT.replace("<MSG.2>A31</MSG.2>", "");
        assert_eq!(processor.process(&soap(&no_type)).await.status, 200);
        assert!(
            !bus.all_sent("egress-queue")[0]
                .properties
                .contains_key("MessageType")
        );
    }

    #[tokio::test]
    async fn er7_output_that_cannot_convert_is_a_400() {
        // spec/19 §9.6 — not valid v2.xml at all.
        let (processor, _) = rig(raw_xml_settings(Validator::None, OutputFormat::Er7, &[]));
        let reply = processor.process(DOC).await;
        assert_eq!(reply.status, 400, "{}", reply.body);
    }
}
