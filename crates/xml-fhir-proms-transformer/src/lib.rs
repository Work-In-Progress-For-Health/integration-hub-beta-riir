//! WPAS XML into a FHIR R4B message Bundle for Promptly.
//!
//! Implements [`spec/16-proms-transformer.md`].
//!
//! [`spec/16-proms-transformer.md`]: ../../../spec/16-proms-transformer.md
//!
//! # Independent of the HL7 v2 pipeline
//!
//! §1.1 makes this a transformer in the runtime sense — same receive loop,
//! same configuration — but the payload is a bespoke XML schema, **not**
//! HL7 v2.xml, so none of the HL7 tooling applies. Nothing else in this
//! workspace depends on it.

#![forbid(unsafe_code)]

pub mod bundle;
pub mod fhir;
pub mod identity;
pub mod mapping;
pub mod message_type;
pub mod parse;
pub mod reference_data;
pub mod resources;
pub mod source_system;
pub mod transform;

/// The transformer, on the shared runtime.
///
/// `spec/16-proms-transformer.md` §1.1: the same receive loop as the ER7
/// transformers, differing only in `transform`. It implements
/// [`ihb_transformer::PayloadTransformer`] rather than
/// [`ihb_transformer::Transformer`] because its payload is XML in and JSON
/// out, and that trait exists for exactly this.
#[derive(Debug, Default)]
pub struct Proms {
    /// Where §5.2's per-board endpoint overrides come from.
    ///
    /// Read once at construction rather than per message: the environment
    /// does not change while the process runs, and a `getenv` per bundle
    /// would be a syscall on the hot path.
    endpoints: std::collections::BTreeMap<String, String>,
}

impl Proms {
    /// Read the `WPAS_SOURCE_ENDPOINT_{id}` overrides of §5.2.
    #[must_use]
    pub fn from_env() -> Self {
        let endpoints = source_system::SOURCE_SYSTEMS
            .iter()
            .filter_map(|board| {
                let value = std::env::var(format!("WPAS_SOURCE_ENDPOINT_{}", board.system_id))
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())?;
                Some((board.system_id.to_string(), value))
            })
            .collect();

        Self { endpoints }
    }
}

impl ihb_transformer::PayloadTransformer for Proms {
    fn name(&self) -> &'static str {
        "PROMs"
    }

    fn transform_payload(&mut self, body: &str) -> Result<String, ihb_transformer::TransformError> {
        let bundle = transform::transform(
            body,
            &identity::RandomUuids,
            &reference_data::StaticResolver,
            &|system_id| self.endpoints.get(system_id).cloned(),
        )
        // §2.3 and §2.5.6 are both §1.6's **first** row: an ordinary
        // transform failure, logged and abandoned. Not `Unexpected` —
        // that is for a payload an ingress server already accepted, and
        // WPAS XML reaches this service without one.
        .map_err(|error| ihb_transformer::TransformError::Parse(error.to_string()))?;

        serde_json::to_string(&bundle).map_err(|error| {
            ihb_transformer::TransformError::Parse(format!(
                "the bundle could not be serialised: {error}"
            ))
        })
    }
}
