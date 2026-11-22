//! What happens to one message.
//!
//! Implements `spec/07-hl7-server.md` §5 (ordering), §6 (tracking metadata)
//! and the failure paths of §3.4, §3.6 and §7.
//!
//! # The ordering is the design
//!
//! §5 lists thirteen steps and says the order is normative "because each
//! step's failure mode depends on it". That is worth spelling out, because
//! the sequence looks arbitrary until you ask what happens when each step
//! throws:
//!
//! - The `MESSAGE_RECEIVED` event and the metric come **first**, before
//!   anything that can fail, so a message that kills the process still left
//!   a trace that it arrived.
//! - The store send (step 10) comes **before** the bus send (step 11), so
//!   there is a persisted copy before the message is handed on. It is
//!   non-blocking: losing the audit copy must not stop a clinical message.
//! - The bus send (step 11) comes **before** the acknowledgement (step 12),
//!   and is blocking. This is the one that matters:
//!   **acknowledging a message that was not forwarded is the single failure
//!   this system must never produce** (§5.3). A source system that receives
//!   an `AA` will not send that message again, ever.
//!
//! So the shape of this module — which errors use `?` and which are caught
//! and logged — is not stylistic. Each choice is a clause.
//!
//! # Every failure is a NACK
//!
//! Every failure here returns `Err`, and the caller answers with the NACK
//! from [`Pipeline::nack`] and then closes the connection (§4.3). `AR` tells
//! the sender the message is invalid; `AE` tells it to try again. A missing
//! reply is no longer how a retry is signalled.

use hl7_2::Message;
use ihb_bus::message::Outgoing;
use ihb_bus::metadata::{Properties, Tracking};
use ihb_bus::sink::MessageSink;
use ihb_bus::store::StoreEvent;
use ihb_fields::{read, source_system};
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::ack;
use crate::config::Config;
use crate::routing::flow_properties;
use crate::validate::{Rejection, check_fields, is_accepted_message_type, message_type_of};

/// Mints the values §6 requires per message.
///
/// A trait so a test can pin the correlation id and the arrival time and
/// then assert on a whole message rather than on all of it except two
/// fields (`spec/17-testing-and-quality.md` §5.1).
pub trait Mint: Send + Sync {
    /// A fresh correlation id. Production returns a UUID v4.
    fn correlation_id(&self) -> String;

    /// Now, formatted per `spec/index.md` §0.7.
    fn received_at(&self) -> String;

    /// Now, formatted for `MSH-7` of an ACK (`spec/07-hl7-server.md` §4).
    fn ack_timestamp(&self) -> String;
}

/// The real clock and a real UUID.
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
        ack::timestamp()
    }
}

/// One message's journey, minus the socket.
///
/// Generic over its two sinks so the same code runs against Azure and
/// against [`ihb_bus::fake::FakeBus`]; see [`ihb_bus::sink::MessageSink`]
/// for why that is a generic rather than a `dyn`.
pub struct Pipeline<E, S, M = RealMint> {
    /// This deployment.
    config: Config,
    /// Where a validated message goes. Step 11, blocking.
    egress: E,
    /// Where the audit copy goes. Step 10, non-blocking. `None` when
    /// `MESSAGE_STORE_QUEUE_NAME` is unset.
    store: Option<S>,
    /// Events.
    events: EventLogger,
    /// Counters.
    metrics: Metrics,
    /// Correlation ids and timestamps.
    mint: M,
}

impl<E: MessageSink, S: MessageSink> Pipeline<E, S, RealMint> {
    /// A pipeline using the real clock.
    pub fn new(config: Config, egress: E, store: Option<S>) -> Self {
        Self::with_mint(config, egress, store, RealMint)
    }
}

impl<E: MessageSink, S: MessageSink, M: Mint> Pipeline<E, S, M> {
    /// A pipeline with an explicit [`Mint`].
    pub fn with_mint(config: Config, egress: E, store: Option<S>, mint: M) -> Self {
        let events = EventLogger::new(&config.workflow_id, &config.microservice_id)
            .with_redaction(config.redaction_enabled);
        let metrics = Metrics::new(
            &config.workflow_id,
            &config.microservice_id,
            &config.health_board,
            &config.peer_service,
        );

        Self {
            config,
            egress,
            store,
            events,
            metrics,
            mint,
        }
    }

    /// This deployment's configuration.
    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// Take one message through every step of §5.
    ///
    /// Returns the ER7 acknowledgement to write back. An `Err` means the
    /// message was not forwarded: the caller sends [`Pipeline::nack`] and
    /// closes the connection (§4.3).
    ///
    /// # Errors
    ///
    /// [`Rejection`] for every path that stops a message — an unparseable
    /// message (§7.2), an unregistered type (§7.1), a failed field or schema
    /// or standard check (§3), or a Service Bus send that did not succeed
    /// (§5.3).
    pub async fn handle(&self, raw: &str) -> Result<String, Rejection> {
        // ---- Step 1 and 2, before anything can fail. -------------------
        //
        // §5: "before anything can fail, so a message that kills the
        // process is still recorded". The correlation id does not exist
        // yet — it is minted at step 6 — so this event carries none, which
        // is why `spec/04-observability.md` allows an absent one.
        self.events.log_message_received(raw, None, None);
        self.metrics.message_received();

        // ---- Step 3: parse. --------------------------------------------
        let message = match ihb_validation::parse(raw) {
            Ok(message) => message,
            Err(error) => {
                // §7.2.
                let detail = format!("Invalid HL7 Message: {error}");
                tracing::error!(%error, "Invalid HL7 Message");
                let rejection = Rejection::unparseable(detail);
                self.fail(raw, &rejection, None);
                return Err(rejection);
            }
        };

        // §7.1 — the allow-list. In the Python this was handler dispatch,
        // which happens after parsing and before `reply`, so it sits here
        // rather than among the §3 layers.
        let (code, trigger, structure) = message_type_of(&message);
        if !is_accepted_message_type(&code, &trigger, &structure) {
            let named = [code.as_str(), trigger.as_str(), structure.as_str()]
                .iter()
                .filter(|part| !part.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("^");
            let detail = format!("Unsupported Message Type: {named}");
            tracing::error!(message_type = %named, "Unsupported Message Type");
            let rejection = Rejection::unsupported_message_type(detail);
            self.fail(raw, &rejection, None);
            return Err(rejection);
        }

        // ---- Step 4: the field-level layers of §3.1 to §3.3. ------------
        if let Err(rejection) = check_fields(
            &message,
            self.config.hl7_version.as_deref(),
            &self.config.sending_apps,
            self.config.validation_flow_name.as_deref(),
        ) {
            tracing::error!(reason = %rejection.reason, "validation failed");
            self.events
                .log_validation_result(raw, &rejection.reason, false, None);
            self.fail(raw, &rejection, None);
            return Err(rejection);
        }

        // ---- Step 5. ---------------------------------------------------
        self.events
            .log_validation_result(raw, "Message validation passed", true, None);

        // ---- Step 6: tracking metadata (§6). ---------------------------
        let tracking = Tracking {
            correlation_id: self.mint.correlation_id(),
            received_at: self.mint.received_at(),
            source_system: source_system(&message),
            workflow_id: self.config.workflow_id.clone(),
        };
        let correlation_id = tracking.correlation_id.clone();

        // ---- Step 7: the XML payload (§3.4 or §3.5). -------------------
        let xml = match self.render_xml(raw, &message, &correlation_id) {
            Ok(xml) => xml,
            Err(rejection) => {
                // §3.4.4 step 4 — **store it, then abort**. A message the
                // hub rejected is exactly the one an operator will want to
                // read, so the audit copy is written even though the
                // message is going no further. The `XmlPayload` is null
                // because there is no valid rendering of it.
                self.store_message(raw, None, &tracking).await;
                return Err(rejection);
            }
        };

        // ---- Step 8: the standard release check (§3.6). ----------------
        if let Some(standard) = self.config.validation_standard.as_deref() {
            if let Err(error) = ihb_validation::validate_standard_parsed(&message, standard) {
                let detail = format!("Standard HL7 v{standard} validation failed: {error}");
                tracing::error!(%error, standard, "standard HL7 validation failed");
                self.events
                    .log_validation_result(raw, &detail, false, Some(&correlation_id));
                let rejection = Rejection {
                    reason: detail,
                    validation_result: "Standard HL7 validation failed",
                    ack_code: ack::REJECT,
                };
                self.fail(raw, &rejection, Some(&correlation_id));
                return Err(rejection);
            }
            self.events.log_validation_result(
                raw,
                &format!("Standard HL7 v{standard} validation passed"),
                true,
                Some(&correlation_id),
            );
        }

        // ---- Step 9: routing properties (§6.2), non-blocking (§5.4). ---
        let mut properties = tracking.to_properties();
        properties.extend(flow_properties(
            self.config.validation_flow_name.as_deref(),
            &message,
        ));

        // ---- Step 10: the message store, non-blocking (§5.2). ----------
        self.store_message(raw, xml.as_deref(), &tracking).await;

        // ---- Step 11: Service Bus, blocking (§5.3). --------------------
        let control_id = read(&message, "MSH-10");
        self.forward(raw, &control_id, properties, &correlation_id)
            .await?;

        // ---- Step 12 and 13. -------------------------------------------
        let acknowledgement = ack::build(&message, &self.mint.ack_timestamp());
        self.events
            .log_message_processed(raw, None, Some(&correlation_id));

        Ok(acknowledgement)
    }

    /// The NACK for a rejected message (`spec/07-hl7-server.md` §4.3).
    ///
    /// Re-parses `raw` to echo its header, as upstream does: a message that
    /// parsed during [`handle`](Self::handle) parses again, and one that
    /// did not gets the generic NACK of §4.3.2.
    #[must_use]
    pub fn nack(&self, raw: &str, rejection: &Rejection) -> String {
        nack_for(raw, rejection, &self.mint.ack_timestamp())
    }

    /// Step 7 — produce the v2.xml rendering for the message store.
    ///
    /// Two very different paths behind one return type:
    ///
    /// - **§3.4**, when a flow schema is configured: validation and
    ///   conversion in one. A failure here **stops the message**, and stores
    ///   it first with a null payload so an operator can inspect exactly the
    ///   message that was refused.
    /// - **§3.5**, otherwise: a schema-unaware conversion whose failure is
    ///   **not fatal**. The message goes on with no XML.
    ///
    /// The asymmetry is the point. A message that fails its flow schema is
    /// invalid; a message whose XML rendering failed is merely missing an
    /// audit convenience.
    fn render_xml(
        &self,
        raw: &str,
        message: &Message,
        correlation_id: &str,
    ) -> Result<Option<String>, Rejection> {
        // §3.4.1 — flow-schema validation runs when a flow is configured and
        // it is not `mpi`. `validation_flow` is already `None` for `mpi`
        // (see `config::read_flow`), so this covers both conditions.
        if let Some(flow) = self.config.validation_flow {
            return match ihb_validation::validate_and_convert_parsed(message, flow, None) {
                Ok(result) if result.is_valid => {
                    // §3.4.3.
                    self.events.log_validation_result(
                        raw,
                        &format!("XML validation passed for flow '{}'", flow.as_str()),
                        true,
                        Some(correlation_id),
                    );
                    Ok(Some(result.xml))
                }
                Ok(result) => {
                    // `is_valid` false always carries an `error`; the
                    // fallback is here so a future change to that invariant
                    // degrades to a vague message rather than a panic on
                    // the rejection path.
                    let reason = result.error.as_ref().map_or_else(
                        || "the message does not conform to its flow schema".to_string(),
                        ToString::to_string,
                    );
                    Err(self.flow_schema_failed(raw, flow.as_str(), &reason, correlation_id))
                }
                Err(error) => Err(self.flow_schema_failed(
                    raw,
                    flow.as_str(),
                    &error.to_string(),
                    correlation_id,
                )),
            };
        }

        // §3.5 — schema-unaware, and a failure is not fatal.
        match ihb_validation::er7_to_xml(raw) {
            Ok(xml) => Ok(Some(xml)),
            Err(error) => {
                let detail = format!(
                    "Failed to generate XML payload for message store: {error} \
                     (CorrelationId: {correlation_id})"
                );
                tracing::error!(%error, "failed to generate XML payload for message store");
                self.events
                    .log_validation_result(raw, &detail, false, Some(correlation_id));
                Ok(None)
            }
        }
    }

    /// §3.4.4 — the four things that happen, in order, before aborting.
    fn flow_schema_failed(
        &self,
        raw: &str,
        flow: &str,
        error: &str,
        correlation_id: &str,
    ) -> Rejection {
        let detail = format!(
            "XML validation failed for flow '{flow}': {error} (CorrelationId: {correlation_id})"
        );

        // 1.
        tracing::error!(flow, %error, correlation_id, "XML validation failed");
        // 2.
        self.events
            .log_validation_result(raw, &detail, false, Some(correlation_id));

        let rejection = Rejection {
            reason: detail,
            validation_result: "XML schema validation failed",
            ack_code: ack::REJECT,
        };
        // 3.
        self.fail(raw, &rejection, Some(correlation_id));

        // Step 4 of §3.4.4 — the store send — belongs to the caller,
        // because it needs the tracking metadata this function does not
        // have. `handle` does it on the way out; see the `Err` arm of
        // step 7.
        rejection
    }

    /// Step 10 — the audit copy. Never fatal (§5.2).
    async fn store_message(&self, raw: &str, xml: Option<&str>, tracking: &Tracking) {
        let Some(store) = self.store.as_ref() else {
            return;
        };

        let event = StoreEvent {
            message_received_at: tracking.received_at.clone(),
            correlation_id: tracking.correlation_id.clone(),
            source_system: tracking.source_system.clone(),
            processing_component: self.config.microservice_id.clone(),
            target_system: Some(self.config.peer_service.clone()),
            raw_payload: raw.to_string(),
            xml_payload: xml.map(ToString::to_string),
            session_id: self.config.egress_session_id.clone(),
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
            .with_session(self.config.egress_session_id.clone());

        if let Err(error) = store.send(outgoing).await {
            // §5.2 — logged, and that is all. The message keeps going.
            tracing::error!(%error, "Failed to send to message store: {error}");
        }
    }

    /// Step 11 — the forward that decides whether there is an ACK (§5.3).
    async fn forward(
        &self,
        raw: &str,
        control_id: &str,
        properties: Properties,
        correlation_id: &str,
    ) -> Result<(), Rejection> {
        let outgoing = Outgoing::new(raw)
            .with_message_id(control_id.to_string())
            .with_session(self.config.egress_session_id.clone())
            .with_properties(properties);

        match self.egress.send(outgoing).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let detail = format!("Failed to send message {control_id} to Service Bus: {error}");
                tracing::error!(%error, control_id, "Failed to send message to Service Bus");
                let rejection = Rejection {
                    reason: detail,
                    validation_result: "Service Bus send failed",
                    // AE, not AR: the message is fine and the hub is not, so
                    // the sender must retry (§4.3).
                    ack_code: ack::ERROR,
                };
                self.fail(raw, &rejection, Some(correlation_id));
                Err(rejection)
            }
        }
    }

    /// Emit the `MESSAGE_FAILED` event that accompanies every rejection.
    fn fail(&self, raw: &str, rejection: &Rejection, correlation_id: Option<&str>) {
        self.events.log_message_failed(
            raw,
            &rejection.reason,
            Some(rejection.validation_result),
            correlation_id,
        );
    }
}

/// [`Pipeline::nack`] without a pipeline, for the listener's own failures.
fn nack_for(raw: &str, rejection: &Rejection, now: &str) -> String {
    match ihb_validation::parse(raw) {
        Ok(message) if !read(&message, "MSH-10").is_empty() => {
            ack::build_nack(&message, rejection.ack_code, &rejection.reason, now)
        }
        // §2.6.2 — echo the sender's own MSH-10 when it can still be read.
        _ => ack::build_generic_nack(
            &rejection.reason,
            ack::sniff_control_id(raw).as_deref(),
            now,
        ),
    }
}
