#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable
    )
)]
//! WDS → WIS.
//!
//! Implements `spec/09-transformers.md` §7.
//!
//! Not a mapping but a filter: only `A28` and `A31` go on, and they go on
//! unchanged. The output stays ER7 because the `soap-sender` downstream
//! converts it to v2 XML and wraps it for WIS; converting here as well would
//! do that work twice (§7.2).
//!
//! Where it sits: the transformer consumes from the WDS ingress queue and
//! publishes to the WIS egress queue; [`ihb_transformer::main_loop::run`]
//! owns the receive/lock/forward/acknowledge cycle (`spec/09-transformers.md`
//! §1), so this crate supplies only the accept-or-refuse decision. A refused
//! message is abandoned and the bus's delivery count dead-letters it (§7.3).
//!
//! Upstream: `transformers/hl7_wds_transformer/hl7_wds_transformer/wds_transformer.py`
//! in the Python repository.

use hl7_2::Message;
use ihb_transformer::{TransformError, Transformer};

/// The trigger events (`MSH-9.2`) forwarded to WIS (`spec/09-transformers.md` §7.1).
///
/// Compared exactly, after trimming; see [`is_allowed`].
pub const ALLOWED_TRIGGER_EVENTS: [&str; 2] = ["A28", "A31"];

/// The WDS filter: a stateless [`Transformer`] that forwards `A28`/`A31`
/// and refuses everything else (`spec/09-transformers.md` §7).
///
/// Unit struct because there is no configuration to hold; queue names and
/// credentials are the runtime's concern (§7.4).
#[derive(Debug, Default)]
pub struct Wds;

/// Whether `trigger` is forwarded.
///
/// **Case-sensitive** (`spec/09-transformers.md` §7.1): the Python compared
/// the trimmed value with a set of upper-case literals, so `a28` is refused.
///
/// # Examples
///
/// ```
/// use hl7_wds_transformer::is_allowed;
///
/// assert!(is_allowed("A28"));
/// assert!(is_allowed(" A31 "), "trimmed first");
/// assert!(!is_allowed("a28"));
/// assert!(!is_allowed("A08"));
/// assert!(!is_allowed(""));
/// ```
#[must_use]
pub fn is_allowed(trigger: &str) -> bool {
    ALLOWED_TRIGGER_EVENTS.contains(&trigger.trim())
}

impl Transformer for Wds {
    /// `WDS` (`spec/09-transformers.md` §7.4); it feeds the audit text and
    /// the logging target, so it is frozen.
    fn name(&self) -> &'static str {
        "WDS"
    }

    /// Returns the message re-parsed flat and otherwise untouched when
    /// `MSH-9.2` is allowed.
    ///
    /// # Errors
    ///
    /// [`TransformError::Parse`] if the re-parse fails, and
    /// [`TransformError::Rejected`] (the normative §7.3 text) for any other
    /// trigger, including an empty one. The runtime abandons the message in
    /// both cases rather than acknowledging it.
    fn transform(&mut self, message: &Message) -> Result<Message, TransformError> {
        // §7.1 — re-parse flat so `MSH-9.2` is read the same way for every
        // structure: group detection for some structures
        // hides the `MSH-9` components from a structured read.
        let flat = ihb_validation::parse(&message.to_er7())
            .map_err(|error| TransformError::Parse(error.to_string()))?;
        let trigger = ihb_fields::read(&flat, "MSH-9.2");
        let trigger = trigger.trim();

        if !is_allowed(trigger) {
            // §7.3 — normative text. The trigger is a code, not patient data.
            return Err(TransformError::Rejected(format!(
                "WDS transformer: unsupported trigger event '{trigger}'. \
                 Allowed: A28, A31. Message will be dead-lettered."
            )));
        }

        tracing::info!(
            trigger,
            "WDS transformer: passing the message through unchanged"
        );
        Ok(flat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Python's own fixtures (`tests/wds_messages.py`).
    const A28: &str = "MSH|^~\\&|328||100||20260729095037||ADT^A28^ADT_A05|6778031837018553261|P|2.5|||AL|NE||UTF-8\rEVN|A28|20260729095037\rPID|1||B0000010612^^^328^PI|||Testpatient^Test^^^||20010909|M|||Welsh Parliament^Cardiff Bay^CARDIFF^^CF99 1SN\rPD1|||^UNK|UNK\rPV1||N";
    const A31: &str = "MSH|^~\\&|328||100||20260729095037||ADT^A31^ADT_A05|6778031837018553262|P|2.5|||AL|NE||UTF-8\rEVN|A31|20260729095037\rPID|1||B0000010613^^^328^PI|||Testpatient^Test^^^||19900101|F|||1 Test Street^Test Town^CARDIFF^^CF1 1AA\rPD1|||^UNK|UNK\rPV1||N";
    const A08: &str = "MSH|^~\\&|328||100||20260729095037||ADT^A08^ADT_A01|6778031837018553263|P|2.5|||AL|NE||UTF-8\rEVN|A08|20260729095037\rPID|1||B0000010614^^^328^PI|||Testpatient^Test^^^||19800601|M\rPV1||N";

    fn run(input: &str) -> Result<Message, TransformError> {
        let parsed = ihb_validation::parse(input).unwrap();
        Wds.transform(&parsed)
    }

    #[test]
    fn an_a28_is_passed_through() {
        // §7.2.
        let out = run(A28).unwrap();
        assert_eq!(ihb_fields::read(&out, "MSH-9.2"), "A28");
    }

    #[test]
    fn an_a31_is_passed_through() {
        let out = run(A31).unwrap();
        assert_eq!(ihb_fields::read(&out, "MSH-9.2"), "A31");
    }

    #[test]
    fn the_output_is_the_reparsed_input_and_nothing_else() {
        // §7.2 and §7.5 — "unchanged" means unchanged.
        let parsed = ihb_validation::parse(A28).unwrap();
        let expected = ihb_validation::parse(&parsed.to_er7()).unwrap().to_er7();
        assert_eq!(run(A28).unwrap().to_er7(), expected);
    }

    #[test]
    fn a_disallowed_trigger_is_refused_with_the_normative_text() {
        // §7.3, byte for byte.
        let error = run(A08).unwrap_err();
        assert_eq!(
            error.to_string(),
            "WDS transformer: unsupported trigger event 'A08'. \
             Allowed: A28, A31. Message will be dead-lettered."
        );
        assert!(matches!(error, TransformError::Rejected(_)));
    }

    #[test]
    fn an_empty_trigger_is_refused() {
        // §7.5.
        let message = "MSH|^~\\&|328||100||20260729095037||ADT|1|P|2.5\rPID|1\r";
        let error = run(message).unwrap_err();
        assert!(
            error.to_string().contains("unsupported trigger event ''"),
            "{error}"
        );
    }

    #[test]
    fn a_lower_case_trigger_is_refused() {
        // §7.1 — case-sensitive.
        let message = A28.replace("ADT^A28", "ADT^a28");
        assert!(run(&message).is_err());
    }

    #[test]
    fn the_name_is_wds() {
        // §7.4.
        assert_eq!(Transformer::name(&Wds), "WDS");
        assert_eq!(
            Wds.received_audit_text(),
            "Message received for WDS transformation"
        );
    }
}
