//! Working out what shape a message claims to be.
//!
//! Implements `spec/06-validation.md` §2.
//!
//! An HL7 v2 message says what it is in `MSH-9`, in up to three parts:
//! the message code (`ADT`), the trigger event (`A28`), and the message
//! structure (`ADT_A05`). The third is the one that names a schema — and it
//! is the one senders most often leave out.
//!
//! When it is missing, the structure has to be inferred from the first two,
//! and the inference is **not** simply joining them with an underscore. An
//! `ADT^A28` arrives carried by the `ADT_A05` structure; an `ADT^A40` by
//! `ADT_A39`. Those mappings are in the HL7 standard, not in the message, so
//! they are stated here.

use std::collections::BTreeMap;

use hl7_2::Message;

use crate::error::ValidationError;

/// The trigger events that arrive carried by another message's structure.
///
/// `validate.py :: _DEFAULT_TRIGGER_MAPPING`, and normative
/// (`spec/06-validation.md` §2.4).
const DEFAULT_TRIGGER_MAPPING: &[(&str, &str, &str)] = &[
    ("ADT", "A28", "ADT_A05"),
    ("ADT", "A31", "ADT_A05"),
    ("ADT", "A40", "ADT_A39"),
];

/// What a message says it is.
///
/// Produced by [`resolve`]. The `override_structure` field is the subtle
/// one: see [`Resolved::override_structure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The structure identifier: what names a schema, and what the v2.xml
    /// root element is called.
    pub structure_id: String,
    /// The structure to impose on the conversion, when the message did not
    /// carry one of its own.
    ///
    /// `None` when `MSH-9.3` was present — the message already knows what it
    /// is. `Some` when the structure was inferred, in which case the
    /// conversion must be told, because reading `MSH-9.3` again would find
    /// nothing (`spec/06-validation.md` §2.2).
    pub override_structure: Option<String>,
    /// `MSH-9.1`, the message code. Empty when absent.
    pub message_type: String,
    /// `MSH-9.2`, the trigger event. Empty when absent.
    pub trigger_event: String,
}

/// A caller's own mapping of `(code, trigger)` to structure.
///
/// Replaces the default **entirely** rather than extending it
/// (`spec/06-validation.md` §2.5), so a caller that supplies one is stating
/// the whole policy.
pub type TriggerMapping = BTreeMap<(String, String), String>;

/// The default mapping, as a map.
///
/// # Examples
///
/// ```
/// use ihb_validation::structure::default_trigger_mapping;
///
/// let mapping = default_trigger_mapping();
/// assert_eq!(
///     mapping.get(&("ADT".to_string(), "A28".to_string())).map(String::as_str),
///     Some("ADT_A05")
/// );
/// ```
#[must_use]
pub fn default_trigger_mapping() -> TriggerMapping {
    DEFAULT_TRIGGER_MAPPING
        .iter()
        .map(|(code, trigger, structure)| {
            (
                ((*code).to_string(), (*trigger).to_string()),
                (*structure).to_string(),
            )
        })
        .collect()
}

/// Work out what shape a message claims to be.
///
/// The order is normative (`spec/06-validation.md` §2):
///
/// 1. `MSH-9.3` present and non-blank — that **is** the structure, and
///    nothing is overridden.
/// 2. Otherwise `MSH-9.2` present — look up `(MSH-9.1, MSH-9.2)` in the
///    mapping, falling back to `"{code}_{trigger}"`. With no code, the
///    trigger alone. Either way the result is an override.
/// 3. Otherwise, fail.
///
/// # Errors
///
/// [`ValidationError::Structure`] when the message carries neither a
/// structure nor a trigger.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ihb_validation::structure::resolve;
///
/// // A message that says what it is.
/// let stated = hl7_2::parse("MSH|^~\\&|252|252|100|100|20260729||ADT^A28^ADT_A05|M1|P|2.5")?;
/// let resolved = resolve(&stated, None)?;
/// assert_eq!(resolved.structure_id, "ADT_A05");
/// assert_eq!(resolved.override_structure, None, "nothing to impose");
///
/// // A message that does not, whose trigger is carried by another structure.
/// let inferred = hl7_2::parse("MSH|^~\\&|224|224|100|100|20260729||ADT^A28|M1|P|2.4")?;
/// let resolved = resolve(&inferred, None)?;
/// assert_eq!(resolved.structure_id, "ADT_A05");
/// assert_eq!(resolved.override_structure.as_deref(), Some("ADT_A05"));
/// # Ok(())
/// # }
/// ```
pub fn resolve(
    message: &Message,
    trigger_mapping: Option<&TriggerMapping>,
) -> Result<Resolved, ValidationError> {
    let structure = read(message, "MSH-9.3");
    let trigger = read(message, "MSH-9.2");
    let code = read(message, "MSH-9.1");

    if !structure.is_empty() {
        return Ok(Resolved {
            structure_id: structure,
            override_structure: None,
            message_type: code,
            trigger_event: trigger,
        });
    }

    if trigger.is_empty() {
        return Err(ValidationError::Structure);
    }

    let structure_id = if code.is_empty() {
        // No code to pair the trigger with, so the trigger is all there is.
        trigger.clone()
    } else {
        let owned = default_trigger_mapping();
        let mapping = trigger_mapping.unwrap_or(&owned);
        mapping
            .get(&(code.clone(), trigger.clone()))
            .cloned()
            .unwrap_or_else(|| format!("{code}_{trigger}"))
    };

    Ok(Resolved {
        structure_id: structure_id.clone(),
        override_structure: Some(structure_id),
        message_type: code,
        trigger_event: trigger,
    })
}

/// Read a header value, trimmed, or `""`.
fn read(message: &Message, path: &str) -> String {
    message
        .get(path)
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message with the given `MSH-9`.
    fn with_type(msh9: &str) -> Message {
        hl7_2::parse(&format!(
            "MSH|^~\\&|252|252|100|100|20260729095037||{msh9}|MSG1|P|2.5"
        ))
        .expect("the fixture parses")
    }

    #[test]
    fn a_stated_structure_is_used_as_is() {
        // spec/06-validation.md §2.1
        let resolved = resolve(&with_type("ADT^A28^ADT_A05"), None).unwrap();
        assert_eq!(resolved.structure_id, "ADT_A05");
        assert_eq!(resolved.override_structure, None);
        assert_eq!(resolved.message_type, "ADT");
        assert_eq!(resolved.trigger_event, "A28");
    }

    #[test]
    fn a_stated_structure_wins_over_the_mapping() {
        // spec/06-validation.md §2.1 — the message knows best about itself.
        // ADT^A40 would map to ADT_A39, but this one says ADT_A40.
        let resolved = resolve(&with_type("ADT^A40^ADT_A40"), None).unwrap();
        assert_eq!(resolved.structure_id, "ADT_A40");
        assert_eq!(resolved.override_structure, None);
    }

    #[test]
    fn the_default_mapping_is_the_pythons() {
        // spec/06-validation.md §2.4 — all three rows.
        for (msh9, expected) in [
            ("ADT^A28", "ADT_A05"),
            ("ADT^A31", "ADT_A05"),
            ("ADT^A40", "ADT_A39"),
        ] {
            let resolved = resolve(&with_type(msh9), None).unwrap();
            assert_eq!(resolved.structure_id, expected, "{msh9}");
            assert_eq!(resolved.override_structure.as_deref(), Some(expected));
        }
    }

    #[test]
    fn an_unmapped_trigger_falls_back_to_joining_with_an_underscore() {
        // spec/06-validation.md §2.2
        let resolved = resolve(&with_type("ADT^A04"), None).unwrap();
        assert_eq!(resolved.structure_id, "ADT_A04");
        assert_eq!(resolved.override_structure.as_deref(), Some("ADT_A04"));
    }

    #[test]
    fn a_trigger_with_no_code_stands_alone() {
        // spec/06-validation.md §2.2 — the second bullet.
        let resolved = resolve(&with_type("^A28"), None).unwrap();
        assert_eq!(resolved.structure_id, "A28");
        assert_eq!(resolved.override_structure.as_deref(), Some("A28"));
        assert_eq!(resolved.message_type, "");
    }

    #[test]
    fn neither_a_structure_nor_a_trigger_is_an_error() {
        // spec/06-validation.md §2.3
        let message = with_type("ADT");
        assert_eq!(resolve(&message, None), Err(ValidationError::Structure));
    }

    #[test]
    fn an_empty_message_type_is_an_error() {
        // spec/06-validation.md §2.3
        let message =
            hl7_2::parse("MSH|^~\\&|252|252|100|100|20260729095037|||MSG1|P|2.5").unwrap();
        assert_eq!(resolve(&message, None), Err(ValidationError::Structure));
    }

    #[test]
    fn a_caller_mapping_replaces_the_default_entirely() {
        // spec/06-validation.md §2.5 — replaces, does not extend. A28 is in
        // the default but not in this one, so it falls back to joining.
        let mut mapping = TriggerMapping::new();
        mapping.insert(("ADT".into(), "A04".into()), "ADT_A01".into());

        let a04 = resolve(&with_type("ADT^A04"), Some(&mapping)).unwrap();
        assert_eq!(a04.structure_id, "ADT_A01");

        let a28 = resolve(&with_type("ADT^A28"), Some(&mapping)).unwrap();
        assert_eq!(
            a28.structure_id, "ADT_A28",
            "the default mapping must not still apply"
        );
    }

    #[test]
    fn header_values_are_trimmed() {
        let resolved = resolve(&with_type("ADT^A28^ ADT_A05 "), None).unwrap();
        assert_eq!(resolved.structure_id, "ADT_A05");
    }

    #[test]
    fn the_default_mapping_has_exactly_three_entries() {
        // spec/06-validation.md §2.4 — a fourth would be a silent policy
        // change, so the count is asserted rather than assumed.
        assert_eq!(default_trigger_mapping().len(), 3);
    }
}
