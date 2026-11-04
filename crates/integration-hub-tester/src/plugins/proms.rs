//! The PROMs transformer: WPAS XML to a FHIR R4B bundle.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §4.5.

use ihb_transformer::PayloadTransformer;
use xml_fhir_proms_transformer::Proms;

use crate::plugin::{Outcome, Plugin, Sample};

#[path = "proms_samples.rs"]
mod samples;

/// `proms` (§4.5): runs the real PROMs transformer on a WPAS XML payload and
/// pretty-prints the resulting FHIR Bundle. Entry UUIDs are random, so two
/// runs differ in those values only.
#[derive(Debug, Default, Clone, Copy)]
pub struct PromsPlugin;

impl Plugin for PromsPlugin {
    fn id(&self) -> &'static str {
        "proms"
    }
    fn tab_label(&self) -> &'static str {
        "PROMS Transformer"
    }
    fn description(&self) -> &'static str {
        "WPAS XML (PromsEventRequest) -> Promptly FHIR R4B message Bundle. Supported eventCodes: \
         REFERRAL, SURGERY, PREOP, INPATIENT, CANCELLED, PREREAD"
    }
    fn input_label(&self) -> &'static str {
        "WPAS PromsEventRequest XML"
    }
    fn output_label(&self) -> &'static str {
        "FHIR R4B JSON Bundle"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("REFERRAL (Referral)", samples::REFERRAL),
            ("SURGERY (Procedure)", samples::SURGERY),
            ("PREOP (Appointment)", samples::PREOP),
            ("INPATIENT (Encounter)", samples::INPATIENT),
            ("CANCELLED (Appointment Cancelled)", samples::CANCELLED),
            ("PREREAD (Pre-admission)", samples::PREREAD),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        // `from_env` is not used: it reads `WPAS_SOURCE_ENDPOINT_*`, and the
        // tester reads no environment (§1.2).
        let json = Proms::default()
            .transform_payload(input.trim())
            .map_err(|error| error.to_string())?;
        let value: serde_json::Value =
            serde_json::from_str(&json).map_err(|error| format!("bad bundle JSON: {error}"))?;
        let pretty = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;

        let kinds: Vec<&str> = value
            .get("entry")
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.pointer("/resource/resourceType")?.as_str())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Outcome::accepted(
            pretty,
            format!("{} entries: {}", kinds.len(), kinds.join(", ")),
        ))
    }
}
