#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! ChemoCare → MPI.
//!
//! Implements `spec/09-transformers.md` §4.
//!
//! Unlike PHW, this one **rebuilds** the message: it maps named fields into
//! a fresh message and copies nothing else except the non-specific segments
//! of §4.6. A field not in §4's tables does not reach MPI.
//!
//! # The health-board prefix
//!
//! The one piece of real business logic here is §4.3.2, and it is worth
//! reading before changing anything: `PID-3[2].1` becomes
//! `{health_board}{pid2}`, and an unrecognised sending application leaves
//! it **unset** rather than falling back. That is deliberate — a fabricated
//! patient identifier routed into MPI is worse than a missing one.

use hl7_2::Message;
use ihb_fields::{copy, copy_range, read, set, trim_trailing_separators};
use ihb_transformer::{TransformError, Transformer};

/// `msh3` to the prefix `PID-3[2].1` carries
/// (`spec/09-transformers.md` §4.3.2, `pid_mapper.py ::
/// HEALTH_BOARD_MAPPING`).
pub const HEALTH_BOARDS: &[(&str, &str)] = &[
    ("224", "VCC"),
    ("212", "BCUCC"),
    ("192", "SWWCC"),
    ("245", "SEWCC"),
];

/// Segments §4.6 copies verbatim, in this order.
pub const NON_SPECIFIC: &[&str] = &[
    "PV1", "PV2", "OBX", "AL1", "DG1", "PR1", "GT1", "IN1", "IN2", "IN3",
];

/// The prefix for a sending application, or `None` when it is not one of
/// the four (`spec/09-transformers.md` §4.3.2).
///
/// # Examples
///
/// ```
/// use hl7_chemo_transformer::health_board;
///
/// assert_eq!(health_board("224"), Some("VCC"));
/// assert_eq!(health_board("245"), Some("SEWCC"));
///
/// // Not one of the four. The identifier is left unset rather than
/// // invented — see the module documentation.
/// assert_eq!(health_board("999"), None);
/// assert_eq!(health_board(""), None);
/// ```
#[must_use]
pub fn health_board(msh3: &str) -> Option<&'static str> {
    HEALTH_BOARDS
        .iter()
        .find(|(code, _)| *code == msh3)
        .map(|(_, prefix)| *prefix)
}

/// The ChemoCare mapping.
#[derive(Debug, Default)]
pub struct Chemo;

impl Chemo {
    /// Map one message.
    ///
    /// # Errors
    ///
    /// [`TransformError`] when a field cannot be written.
    pub fn map(&self, message: &Message) -> Result<Message, TransformError> {
        let mut out = ihb_validation::parse(&skeleton(message))
            .map_err(|error| TransformError::Parse(error.to_string()))?;

        map_msh(message, &mut out)?;

        if has(message, "EVN") {
            copy_segment(message, &mut out, "EVN")?;
        }
        if has(message, "PID") {
            map_pid(message, &mut out)?;
        }
        if has(message, "PD1") {
            map_pd1(message, &mut out)?;
        }
        if has(message, "NK1") {
            map_nk1(message, &mut out)?;
        }

        // §4.6 — verbatim, and a failure here must not fail the transform.
        for name in NON_SPECIFIC {
            if !has(message, name) {
                continue;
            }
            if let Err(error) = copy_segment(message, &mut out, name) {
                // §4.6.1's exact wording; the required mapping has already
                // completed and the message is still worth forwarding.
                tracing::warn!("Error copying segment {name}: {error}");
            }
        }

        trim_trailing_separators(&mut out);
        Ok(out)
    }
}

impl Transformer for Chemo {
    fn name(&self) -> &'static str {
        // The Python's class is `ChemocareTransformer` and its audit text
        // says "Chemocare", not "ChemoCare". Operators match on it.
        "Chemocare"
    }

    fn transform(&mut self, message: &Message) -> Result<Message, TransformError> {
        self.map(message)
    }
}

/// §4.1 — `MSH`.
fn map_msh(source: &Message, target: &mut Message) -> Result<(), TransformError> {
    for path in [
        "MSH-3.1", "MSH-4.1", "MSH-5.1", "MSH-6.1", "MSH-7.1", "MSH-8",
    ] {
        copy(source, target, path)?;
    }
    copy(source, target, "MSH-9.1")?;
    copy(source, target, "MSH-9.2")?;
    // §4.1 — a literal, because ChemoCare sends `ADT^A28` with no structure
    // and MPI validates against `ADT_A05`.
    set(target, "MSH-9.3", "ADT_A05")?;

    copy(source, target, "MSH-10")?;
    copy(source, target, "MSH-11")?;
    // §2.5 and §4.1 — the message is re-versioned. ChemoCare speaks 2.4;
    // MPI is addressed at 2.5.
    set(target, "MSH-12.1", "2.5")?;

    copy_range(source, target, "MSH", 13, 21)?;
    Ok(())
}

/// §4.3 — `PID`, including the health-board prefix.
fn map_pid(source: &Message, target: &mut Message) -> Result<(), TransformError> {
    copy(source, target, "PID-1")?;

    // §4.3.1 — both derived values fall back to the whole field, because
    // ChemoCare sends `PID-2` as a bare value in some messages and as a
    // `CX` in others.
    let pid2 = first_non_empty(source, "PID-2.1", "PID-2");
    let msh3 = first_non_empty(source, "MSH-3.1", "MSH-3");

    // §4.3.3 — both repetitions exist unconditionally, with their authority
    // and type codes, even when no identifier value is available. MPI reads
    // the type codes to know which identifier is which.
    set(target, "PID-3[1].4.1", "NHS")?;
    set(target, "PID-3[1].5", "NH")?;
    set(target, "PID-3[2].4.1", &msh3)?;
    set(target, "PID-3[2].5", "PI")?;

    if !pid2.is_empty() {
        set(target, "PID-3[1].1", &pid2)?;

        // §4.3.2 — and only when the sending application is one of the
        // four. An unrecognised one leaves this unset.
        if let Some(prefix) = health_board(&msh3) {
            set(target, "PID-3[2].1", &format!("{prefix}{pid2}"))?;
        }
    }

    copy(source, target, "PID-5.1.1")?;
    for component in 2..=8 {
        copy(source, target, &format!("PID-5.{component}"))?;
    }
    copy(source, target, "PID-5.9.1")?;
    copy(source, target, "PID-5.10")?;
    copy(source, target, "PID-5.11")?;

    copy(source, target, "PID-6.1.1")?;
    copy(source, target, "PID-7")?;
    copy(source, target, "PID-8")?;
    copy(source, target, "PID-9.1.1")?;
    copy(source, target, "PID-10.1")?;

    copy(source, target, "PID-11.1.1")?;
    for component in [2, 3, 4, 5, 7, 8] {
        copy(source, target, &format!("PID-11.{component}"))?;
    }

    // §4.3 — every repetition of `PID-13`, three components each. Written
    // per repetition because §2.2.1 forbids the whole-field route, and per
    // component because only .1, .2 and .4 are mapped.
    let phones = repetition_count(source, "PID-13");
    for index in 1..=phones {
        for component in [1, 2, 4] {
            copy(source, target, &format!("PID-13[{index}].{component}"))?;
        }
    }
    // The Python adds a destination repetition per source repetition
    // whether or not it carries anything, so a field ending in `~` keeps
    // its empty last repetition. `copy` alone would drop it.
    ihb_fields::ensure_repetitions(target, "PID-13", phones);

    copy(source, target, "PID-14.1")?;
    copy(source, target, "PID-14.2")?;
    copy(source, target, "PID-17.1")?;
    copy(source, target, "PID-22.1")?;
    copy(source, target, "PID-29.1")?;
    copy(source, target, "PID-32")?;

    Ok(())
}

/// §4.4 — `PD1`.
fn map_pd1(source: &Message, target: &mut Message) -> Result<(), TransformError> {
    for component in [1, 3, 4, 5, 7, 9] {
        copy(source, target, &format!("PD1-3.{component}"))?;
    }
    copy(source, target, "PD1-3.6.1")?;
    copy(source, target, "PD1-3.8.1")?;

    for component in [1, 3, 4, 6] {
        copy(source, target, &format!("PD1-4.{component}"))?;
    }
    copy(source, target, "PD1-4.2.1")?;
    Ok(())
}

/// §4.5 — `NK1`.
fn map_nk1(source: &Message, target: &mut Message) -> Result<(), TransformError> {
    copy(source, target, "NK1-2.2")?;
    copy(source, target, "NK1-2.7")?;
    copy(source, target, "NK1-2.1.1")?;
    copy(source, target, "NK1-3.1")?;
    copy(source, target, "NK1-4.2")?;
    copy(source, target, "NK1-4.7")?;
    copy(source, target, "NK1-4.1.1")?;
    copy(source, target, "NK1-5.1")?;
    Ok(())
}

/// The first of two paths that reads non-empty (§4.3.1).
fn first_non_empty(message: &Message, primary: &str, fallback: &str) -> String {
    let value = read(message, primary);
    if value.is_empty() {
        read(message, fallback)
    } else {
        value
    }
}

/// How many repetitions a field has.
fn repetition_count(message: &Message, path: &str) -> usize {
    ihb_fields::repetitions(message, path).len()
}

/// Whether the message carries this segment.
fn has(message: &Message, name: &str) -> bool {
    message.segments().any(|segment| segment.name == name)
}

/// How many fields a segment carries.
fn field_count(message: &Message, name: &str) -> usize {
    message
        .segments()
        .find(|segment| segment.name == name)
        .map_or(0, |segment| segment.fields.len())
}

/// Copy a whole segment, field by field.
fn copy_segment(source: &Message, target: &mut Message, name: &str) -> Result<(), TransformError> {
    for index in 1..=field_count(source, name) {
        copy(source, target, &format!("{name}-{index}"))?;
    }
    Ok(())
}

/// The output's segments, in §4's order.
fn skeleton(source: &Message) -> String {
    let mut names: Vec<&str> = vec!["MSH"];
    for name in ["EVN", "PID", "PD1", "NK1"] {
        if has(source, name) {
            names.push(name);
        }
    }
    for name in NON_SPECIFIC {
        if has(source, name) {
            names.push(name);
        }
    }

    names
        .into_iter()
        .map(|name| {
            if name == "MSH" {
                "MSH|^~\\&|".to_string()
            } else {
                format!("{name}|")
            }
        })
        .collect::<Vec<_>>()
        .join("\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_of_the_health_board_table() {
        // spec/09-transformers.md §4.3.2 and §6.3.
        assert_eq!(health_board("224"), Some("VCC"));
        assert_eq!(health_board("212"), Some("BCUCC"));
        assert_eq!(health_board("192"), Some("SWWCC"));
        assert_eq!(health_board("245"), Some("SEWCC"));
    }

    #[test]
    fn an_unmapped_health_board_leaves_the_identifier_unset() {
        // §4.3.2 and §6.3 — the case worth having a test for. A fabricated
        // identifier routed into MPI is worse than a missing one.
        let source = ihb_validation::parse(
            "MSH|^~\\&|999|999|100|100|20250624161510||ADT^A28|M1|P|2.4\r\
             PID|1|1000000001^^^^NH",
        )
        .unwrap();

        let out = Chemo.map(&source).unwrap();

        assert_eq!(
            read(&out, "PID-3[1].1"),
            "1000000001",
            "the NHS number is kept"
        );
        assert_eq!(read(&out, "PID-3[2].1"), "", "but nothing is invented");
        assert_eq!(
            read(&out, "PID-3[2].4.1"),
            "999",
            "the authority is still set"
        );
        assert_eq!(read(&out, "PID-3[2].5"), "PI");
    }

    #[test]
    fn the_health_board_prefix_is_applied_to_the_identifier() {
        let source = ihb_validation::parse(
            "MSH|^~\\&|224|224|100|100|20250624161510||ADT^A28|M1|P|2.4\r\
             PID|1|1000000001^^^^NH",
        )
        .unwrap();

        let out = Chemo.map(&source).unwrap();

        assert_eq!(read(&out, "PID-3[2].1"), "VCC1000000001");
    }

    #[test]
    fn both_pid3_repetitions_exist_even_with_no_identifier() {
        // §4.3.3 — MPI reads the type codes to know which identifier is
        // which, so they are set whether or not a value is available.
        let source = ihb_validation::parse(
            "MSH|^~\\&|224|224|100|100|20250624161510||ADT^A28|M1|P|2.4\rPID|1",
        )
        .unwrap();

        let out = Chemo.map(&source).unwrap();

        assert_eq!(read(&out, "PID-3[1].4.1"), "NHS");
        assert_eq!(read(&out, "PID-3[1].5"), "NH");
        assert_eq!(read(&out, "PID-3[2].4.1"), "224");
        assert_eq!(read(&out, "PID-3[2].5"), "PI");
        assert_eq!(read(&out, "PID-3[1].1"), "");
        assert_eq!(read(&out, "PID-3[2].1"), "");
    }

    #[test]
    fn pid2_and_msh3_fall_back_to_the_whole_field() {
        // §4.3.1 — ChemoCare sends `PID-2` as a bare value in some messages
        // and as a `CX` in others.
        let source = ihb_validation::parse(
            "MSH|^~\\&|224|224|100|100|20250624161510||ADT^A28|M1|P|2.4\r\
             PID|1|9999999999",
        )
        .unwrap();

        let out = Chemo.map(&source).unwrap();
        assert_eq!(read(&out, "PID-3[1].1"), "9999999999");
        assert_eq!(read(&out, "PID-3[2].1"), "VCC9999999999");
    }

    #[test]
    fn the_message_is_reversioned_and_given_a_structure() {
        // §4.1 — ChemoCare speaks 2.4 and sends `ADT^A28` with no
        // structure; MPI is addressed at 2.5 and validates `ADT_A05`.
        let source = ihb_validation::parse(
            "MSH|^~\\&|224|224|100|100|20250624161510||ADT^A28|M1|P|2.4\rPID|1",
        )
        .unwrap();

        let out = Chemo.map(&source).unwrap();

        assert_eq!(read(&out, "MSH-9.1"), "ADT");
        assert_eq!(read(&out, "MSH-9.2"), "A28");
        assert_eq!(read(&out, "MSH-9.3"), "ADT_A05");
        assert_eq!(read(&out, "MSH-12.1"), "2.5");
    }

    #[test]
    fn a_message_with_no_pid_is_still_mapped() {
        // §4.3 — "skipped entirely when the source has no PID".
        let source = ihb_validation::parse(
            "MSH|^~\\&|224|224|100|100|20250624161510||ADT^A28|M1|P|2.4\rPV1||U",
        )
        .unwrap();

        let out = Chemo.map(&source).unwrap();

        assert!(!out.to_er7().contains("\rPID"), "{}", out.to_er7());
        assert!(out.to_er7().contains("PV1||U"), "{}", out.to_er7());
    }

    #[test]
    fn the_audit_name_is_the_pythons_spelling() {
        // The Python's class is `ChemocareTransformer` and its audit text
        // reads "Chemocare". An operator's saved search matches on it.
        assert_eq!(Chemo.name(), "Chemocare");
    }
}
