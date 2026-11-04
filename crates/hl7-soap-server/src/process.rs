//! What happens to one SOAP request.
//!
//! Implements `spec/08-hl7-soap-server.md` §3, §4 and §5.
//!
//! # Every failure is a fault, and every fault is a status
//!
//! §3.1 pairs each failure with an HTTP status and a SOAP fault code, and
//! the pairing is not decorative: a 400 tells the sender not to retry, a
//! 500 tells it to, and a 403 tells it to talk to someone. Getting one
//! wrong turns a configuration problem into an infinite retry loop, or a
//! transient outage into a discarded message.
//!
//! So [`Fault`] is carried out of every step, and the HTTP layer only ever
//! renders it. No step here decides a status by itself.
//!
//! # Fault strings never carry field values
//!
//! §5.4, which is `spec/index.md` §0.8 applied to a public endpoint. A SOAP
//! fault goes back over the network to the sender and, in practice, into
//! their logs. The schema-validation fault carries a reason and a path and
//! nothing else.

use hl7_2_soap::envelope::Envelope;
use hl7_2_soap::fault::Fault;
use ihb_bus::message::Outgoing;
use ihb_bus::metadata::Tracking;
use ihb_bus::sink::MessageSink;
use ihb_bus::store::StoreEvent;
use ihb_telemetry::events::EventLogger;
use ihb_telemetry::metrics::Metrics;

use crate::config::Config;

/// The `validation_result` on every rejection (§3.3).
const REJECTED: &str = "SOAP request rejected";

/// The `validation_result` on the catch-all (§3.3).
const UNHANDLED: &str = "Unhandled SOAP server error";

/// Mints the per-message values, so a test can fix them.
///
/// The two values are the only non-determinism in [`Processor::process`];
/// pulling them behind a trait lets the integration tests assert on whole
/// events and stored messages rather than patterns.
pub trait Mint: Send + Sync {
    /// A fresh correlation id, unique per request (a v4 UUID in production).
    fn correlation_id(&self) -> String;
    /// Now, as the ISO 8601 string `spec/index.md` §0.7 prescribes.
    fn received_at(&self) -> String;
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
}

/// What a successful request produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// `MSH-10`, for the success envelope and the Service Bus message id.
    pub control_id: String,
}

/// One request's journey, minus HTTP.
///
/// `E` is the egress sink, `S` the message-store sink and `M` the [`Mint`];
/// all generic so the tests substitute in-memory sinks and a fixed clock
/// without any dynamic dispatch (`MessageSink` is not dyn-compatible).
/// Shared across connections behind an `Arc`, so it holds no per-request
/// state.
pub struct Processor<E, S, M = RealMint> {
    /// This deployment.
    config: Config,
    /// Where a validated message goes.
    egress: E,
    /// Where the audit copy goes. `None` when `MESSAGE_STORE_ENABLED` is not
    /// true (`spec/02-configuration.md` §2.1.2).
    store: Option<S>,
    /// Structured events (`MESSAGE_RECEIVED` and so on), redacting message
    /// content when `HL7_LOG_REDACTION_ENABLED` is true.
    events: EventLogger,
    /// Counters, dimensioned by health board and peer service only; never by
    /// anything taken from the message.
    metrics: Metrics,
    /// Correlation ids and timestamps.
    mint: M,
}

impl<E: MessageSink, S: MessageSink> Processor<E, S, RealMint> {
    /// A processor using the real clock.
    pub fn new(config: Config, egress: E, store: Option<S>) -> Self {
        Self::with_mint(config, egress, store, RealMint)
    }
}

impl<E: MessageSink, S: MessageSink, M: Mint> Processor<E, S, M> {
    /// A processor with an explicit [`Mint`].
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

    /// Take one SOAP request through §3.
    ///
    /// `request` is the whole envelope as received: it is what the
    /// `MESSAGE_RECEIVED` event carries (step 1) and what the message store
    /// keeps as `RawPayload` (step 12).
    ///
    /// # Errors
    ///
    /// The [`Fault`] for whichever step failed, carrying the status §3.1
    /// gives it.
    pub async fn process(&self, request: &str) -> Result<Accepted, Fault> {
        // ---- Steps 1 and 2, before anything can fail. ------------------
        self.events.log_message_received(request, None, None);
        self.metrics.message_received();

        match self.process_inner(request).await {
            Ok(accepted) => Ok(accepted),
            Err(fault) => {
                // §3.3 — every fault emits both events, with the fault
                // string and the same validation result. The catch-all is
                // told apart by its code rather than by a separate path.
                let result = if fault.code == "Server" {
                    UNHANDLED
                } else {
                    REJECTED
                };
                self.events
                    .log_validation_result(request, &fault.reason, false, None);
                self.events
                    .log_message_failed(request, &fault.reason, Some(result), None);
                tracing::error!(code = %fault.code, status = fault.status, reason = %fault.reason,
                    "SOAP request rejected");
                Err(fault)
            }
        }
    }

    /// Steps 3 to 16, with every failure a [`Fault`].
    async fn process_inner(&self, request: &str) -> Result<Accepted, Fault> {
        // ---- Step 3: the envelope and its single business payload. -----
        //
        // `hl7-2-soap` matches on local name only (§3.2) and produces the
        // §3.1 faults for a malformed document, a missing Envelope, a
        // missing Body, and a Body that does not hold exactly one element.
        let envelope: Envelope = hl7_2_soap::envelope::parse(request)?;
        let payload = envelope.payload()?;

        // ---- Steps 4 and 5: the payload's own element name. -------------
        let structure = hl7_2_soap::message::structure_id(payload).to_string();
        if !self
            .config
            .allowed_structures
            .iter()
            .any(|a| a == &structure)
        {
            return Err(Fault::validation(format!(
                "Unsupported HL7 message structure '{structure}'. Allowed values: {}",
                sorted(&self.config.allowed_structures)
            )));
        }

        // ---- Steps 6 and 7: the schema, then validation. ---------------
        let flow = self.config.schema_group;
        if !flow.has_structure(&structure) {
            // §3.1 — a 500 `Server.Configuration`, not a 400: the sender
            // did nothing wrong, and retrying will work once someone fixes
            // the deployment.
            return Err(Fault::configuration(
                "SOAP schema mapping is not configured.",
            ));
        }

        // The payload as it arrived, sliced from the request rather than
        // re-rendered from the parsed tree; see `crate::payload`.
        let payload_xml = crate::payload::extract(request, &structure)
            .ok_or_else(|| {
                // `hl7-2-soap` parsed it, so this can only mean the two
                // disagree about where the element is — which is a defect
                // here, not a malformed request, hence a 500.
                tracing::error!(
                    structure,
                    "the parsed payload could not be located in the request"
                );
                Fault::server("Unexpected server error while processing SOAP request.")
            })?
            .to_string();

        // ---- Step 9: the assigning authority. --------------------------
        //
        // Read before conversion, so an unauthorised sender is refused
        // before this process does any more work on its behalf.
        let authority = hl7_2_soap::message::assigning_authority(payload).ok_or_else(|| {
            // §4.1.
            Fault::validation("Unable to determine assigning authority from payload.")
        })?;
        if !self
            .config
            .allowed_authorities
            .iter()
            .any(|a| a == authority)
        {
            return Err(Fault::authorization(format!(
                "Assigning authority '{authority}' is not authorised. Allowed values: {}",
                sorted(&self.config.allowed_authorities)
            )));
        }

        // ---- Step 10: XML to ER7. --------------------------------------
        let er7 = ihb_validation::xml_to_er7(&payload_xml).map_err(|error| {
            // §5.4 — the error is logged in full, but the fault string that
            // goes back over the network says only that it failed. A
            // conversion error can quote message content.
            tracing::error!(%error, "unable to convert XML payload to ER7");
            Fault::validation("Unable to convert XML payload to ER7 format.")
        })?;

        // Validation of the converted message against the flow schema
        // (step 7). Done on the ER7 because that is what the dictionaries
        // of `spec/06-validation.md` §3 describe, and it is the form that
        // will actually be forwarded.
        let validated =
            ihb_validation::validate_and_convert(&er7, flow, None).map_err(|error| {
                tracing::error!(%error, "payload schema validation failed");
                Fault::validation(format!("Payload schema validation failed: {error}"))
            })?;
        if !validated.is_valid {
            let reason = validated.error.as_ref().map_or_else(
                || "the payload does not conform to its schema".to_string(),
                ToString::to_string,
            );
            return Err(Fault::validation(format!(
                "Payload schema validation failed: {reason}"
            )));
        }

        // ---- Step 8. ---------------------------------------------------
        self.events.log_validation_result(
            request,
            &format!("Payload validation passed for structure '{structure}'"),
            true,
            None,
        );

        // ---- Step 11: tracking metadata. -------------------------------
        let control_id = validated.message_control_id.clone();
        let tracking = Tracking {
            correlation_id: self.mint.correlation_id(),
            received_at: self.mint.received_at(),
            source_system: ihb_validation::parse(&er7)
                .map(|message| ihb_fields::read(&message, "MSH-3"))
                // The message validated a moment ago, so this cannot fail;
                // an empty `SourceSystem` is what `spec/03-message-bus.md`
                // §4 specifies for "not present" in any case.
                .unwrap_or_default(),
            workflow_id: self.config.workflow_id.clone(),
        };

        // ---- Step 12: the message store, non-blocking (§3.4). ----------
        //
        // Note which payload is which: `RawPayload` is the **whole SOAP
        // request**, not the ER7, because that is what arrived and what an
        // operator would need to reproduce the problem.
        self.store_message(request, &payload_xml, &tracking).await;

        // ---- Step 13: Service Bus. -------------------------------------
        let outgoing = Outgoing::new(er7)
            .with_message_id(control_id.clone())
            .with_session(self.config.egress_session_id.clone())
            .with_properties(tracking.to_properties());

        self.egress.send(outgoing).await.map_err(|error| {
            tracing::error!(%error, "failed to send the SOAP message to Service Bus");
            // A 500 `Server`: the sender's message was fine and retrying is
            // the right response, which is exactly what a 500 asks for.
            Fault::server("Unexpected server error while processing SOAP request.")
        })?;

        // ---- Steps 14 and 15. ------------------------------------------
        self.metrics.message_sent();
        self.events
            .log_message_processed(request, None, Some(&tracking.correlation_id));

        Ok(Accepted { control_id })
    }

    /// Step 12 — the audit copy. Never fatal (§3.4).
    async fn store_message(&self, request: &str, payload_xml: &str, tracking: &Tracking) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let event = StoreEvent {
            message_received_at: tracking.received_at.clone(),
            correlation_id: tracking.correlation_id.clone(),
            source_system: tracking.source_system.clone(),
            processing_component: self.config.microservice_id.clone(),
            target_system: Some(self.config.peer_service.clone()),
            raw_payload: request.to_string(),
            xml_payload: Some(payload_xml.to_string()),
            session_id: self.config.egress_session_id.clone(),
        };

        let json = match event.to_json() {
            Ok(json) => json,
            Err(error) => {
                tracing::error!(%error, "Failed to send SOAP message to message store");
                return;
            }
        };

        let outgoing = Outgoing::new(json)
            .with_message_id(tracking.correlation_id.clone())
            .with_session(self.config.egress_session_id.clone());

        if let Err(error) = store.send(outgoing).await {
            tracing::error!(%error, "Failed to send SOAP message to message store: {error}");
        }
    }
}

/// An allow-list as §3.1 renders it: sorted, comma-space separated.
///
/// Sorted so the fault string does not change when the environment variable
/// is reordered — a sender diffing responses should not see a difference
/// that means nothing.
fn sorted(values: &[String]) -> String {
    let mut sorted: Vec<&str> = values.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_allow_list_renders_sorted_and_comma_spaced() {
        // spec/08-hl7-soap-server.md §3.1 — "sorted, comma-space".
        assert_eq!(
            sorted(&["ADT_A39".to_string(), "ADT_A05".to_string()]),
            "ADT_A05, ADT_A39"
        );
        assert_eq!(sorted(&["328".to_string()]), "328");
        assert_eq!(sorted(&[]), "");
    }
}
