//! HL7 v2 validation, and conversion between ER7 and v2.xml.
//!
//! Implements `spec/06-validation.md`.
//!
//! This is where the rewrite's central technical decision lives. The Python
//! validated the v2.xml rendering of a message against per-flow XSDs with
//! `xmlschema`. Rust has no comparable XML Schema validator, so the XSDs are
//! compiled **offline** into `hl7-2` dictionaries — by
//! `cargo xtask build-dictionaries` — and validation becomes `hl7-2`'s own
//! schema-shape validation.
//!
//! That substitution is only acceptable because it was measured. The Phase 0
//! probe put twelve cases through both implementations and got twelve
//! matching verdicts; `spec/06-validation.md` §7.6 records the method, the
//! result, and the one adjustment it required.
//!
//! # The one adjustment
//!
//! **`ValueFormat` diagnostics are excluded.** `hl7-2` checks that a value
//! matches its declared data type; the flow XSDs declare every primitive as
//! an unconstrained `xsd:string`, so `xmlschema` never did. Including them
//! would reject messages flowing through production today — starting with
//! the PHW sample, whose `MSH-7` is `2025-05-05 23:23:32`, the very value the
//! PHW transformer exists to normalise. See §3.3.2a.
//!
//! # Modules
//!
//! - [`flow`] — the six schema groups and their embedded dictionaries.
//! - [`structure`] — working out what shape a message claims to be.
//! - [`validate`] — checking it against that shape, and against a release.
//! - [`convert`] — ER7 ↔ v2.xml.
//! - [`repair`] — the two segments the Python quietly supplies.
//! - [`xsd`] — real XSD validation, run through two independent engines and
//!   compared. A cross-check on the dictionary route above, and a possible
//!   replacement for it; see `spec/06-validation.md` §9.
//! - [`trigger_map`], [`grammar`], [`resolver`] — the standard-release path
//!   ported from upstream's `hl7_message_processor` (`spec/06-validation.md` §10).
//! - [`error`] — what can be wrong, said without saying what is in the
//!   message.
//!
//! # Examples
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use ihb_validation::{Flow, validate_and_convert};
//!
//! let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
//!            EVN||20250502092900\r\
//!            PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r\
//!            PV1||U";
//!
//! let result = validate_and_convert(er7, Flow::Phw, None)?;
//!
//! assert!(result.is_ok());
//! assert_eq!(result.structure_id, "ADT_A05");
//! // The XML goes to the message store alongside the raw payload.
//! assert!(result.xml.contains("urn:hl7-org:v2xml"));
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
// A test asserts; asserting is what `unwrap` and `panic` are for. The lints
// stay on for everything the container actually runs.
// See spec/17-testing-and-quality.md §2.3.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod convert;
pub mod error;
pub mod flow;
pub mod grammar;
pub mod repair;
pub mod resolver;
pub mod structure;
pub mod trigger_map;
pub mod validate;
pub mod xsd;

pub use convert::{er7_to_xml, er7_to_xml_for_flow, xml_to_er7};
pub use error::ValidationError;
pub use flow::Flow;
pub use structure::{Resolved, TriggerMapping, default_trigger_mapping};
pub use validate::{
    SUPPORTED_VERSIONS, ValidationResult, parse, parse_for_flow, validate_and_convert,
    validate_and_convert_parsed, validate_flow, validate_standard, validate_standard_parsed,
};
