//! Reading, copying and formatting HL7 v2 fields.
//!
//! Implements `spec/09-transformers.md` §2 and `spec/03-message-bus.md`
//! §4.1.
//!
//! The Python's `field_utils_lib` exists because `hl7apy` makes reading a
//! nested field a minefield of exceptions — `ChildNotFound`, `AttributeError`,
//! `IndexError` — and every mapper would otherwise be wrapped in `try`. Its
//! helpers all mean "read this if it is there, and do not blow up if it is
//! not".
//!
//! `hl7-2` already answers a missing path with `Ok(None)`, so most of that
//! disappears. What remains, and what this crate is, is the *vocabulary the
//! mapping tables are written in*:
//!
//! | Table says | Function |
//! |---|---|
//! | **read** | [`read`] — a value, or `""` |
//! | **copy** | [`copy`] — copy only if the source is non-empty |
//! | **copy range a..=b** | [`copy_range`] — fields `a` to `b`, repetitions and all |
//! | **set** | [`set`] — write unconditionally |
//!
//! Reading them as a vocabulary rather than as utilities is the point:
//! `spec/09-transformers.md` §3–§5 are tables of a few hundred rows, and a
//! row translates to one call.
//!
//! # The rule that catches people
//!
//! [`copy`] leaves the destination **untouched** when the source is empty —
//! it does not clear it. Half the PIMS and ChemoCare mapping rows depend on
//! that, because they set a default first and then copy over it. See
//! [`copy`] for the worked example.
//!
//! # Examples
//!
//! ```
//! # fn main() -> Result<(), hl7_2::Error> {
//! use ihb_fields::{copy, read};
//!
//! let source = hl7_2::parse("MSH|^~\\&|252|252|100|100|20260729||ADT^A28|MSG1|P|2.5\rPID|1||X^^^NHS")?;
//! let mut target = hl7_2::parse("MSH|^~\\&|||||||ADT^A28|MSG2|P|2.5\rPID|")?;
//!
//! assert_eq!(read(&source, "MSH-3"), "252");
//! assert_eq!(read(&source, "PID-99"), "", "a missing field reads as empty");
//!
//! copy(&source, &mut target, "PID-3.1")?;
//! assert_eq!(read(&target, "PID-3.1"), "X");
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
// A test asserts; asserting is what `unwrap` and `panic` are for. The lints
// stay on for everything the container actually runs.
// See spec/17-testing-and-quality.md §2.3.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use hl7_2::{Error, Message};

/// Read a field, component or subcomponent, or `""` when it is not there.
///
/// The Python's `get_hl7_field_value`. Returning `""` rather than an
/// `Option` is deliberate and matches it: a mapping table asks "is there a
/// value here?", and every caller would otherwise write the same
/// `unwrap_or_default`.
///
/// A malformed path also reads as `""`. That is the one place this differs
/// from being merely lenient, and it is right for the same reason: a path in
/// a mapping table is a constant, so a malformed one is a programming error
/// that shows up in the crate's own tests, not something a message can
/// trigger at three in the morning.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), hl7_2::Error> {
/// use ihb_fields::read;
///
/// let message = hl7_2::parse(
///     "MSH|^~\\&|252|252|100|100|20260729||ADT^A28|MSG1|P|2.5\r\
///      PID|1||8888888^^^252^PI||SURNAME^FORENAME",
/// )?;
///
/// assert_eq!(read(&message, "MSH-3"), "252");
/// assert_eq!(read(&message, "PID-5.1"), "SURNAME");
/// assert_eq!(read(&message, "PID-3.5"), "PI");
///
/// // Absent, out of range, or an unknown segment: all empty.
/// assert_eq!(read(&message, "PID-99"), "");
/// assert_eq!(read(&message, "ZZZ-1"), "");
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn read(message: &Message, path: &str) -> String {
    message.get(path).ok().flatten().unwrap_or_default()
}

/// Read a field, or `None` when it is absent or empty.
///
/// For the handful of places that genuinely need to tell "absent" from
/// "present but empty" — mostly guard conditions in the PIMS mapping, where
/// a repetition is mapped only if it carries a value.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), hl7_2::Error> {
/// use ihb_fields::read_some;
///
/// let message = hl7_2::parse("MSH|^~\\&|252\rPID|1||^^^NHS")?;
///
/// assert_eq!(read_some(&message, "MSH-3").as_deref(), Some("252"));
/// assert_eq!(read_some(&message, "PID-3.1"), None, "present but empty");
/// assert_eq!(read_some(&message, "PID-99"), None, "absent");
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn read_some(message: &Message, path: &str) -> Option<String> {
    message
        .get(path)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

/// Every repetition of a field.
///
/// The mapping tables say "every repetition" for `PID-13` in both the PIMS
/// and ChemoCare transformers, and for the assigning-authority extraction of
/// [`authority_codes`].
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), hl7_2::Error> {
/// use ihb_fields::repetitions;
///
/// let message = hl7_2::parse("MSH|^~\\&|252\rPID|1||||||||||||555-1111~555-2222")?;
///
/// assert_eq!(repetitions(&message, "PID-13"), ["555-1111", "555-2222"]);
/// assert!(repetitions(&message, "PID-99").is_empty());
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn repetitions(message: &Message, path: &str) -> Vec<String> {
    message.repetitions(path).unwrap_or_default()
}

/// Write a value, whatever was there before.
///
/// The mapping tables' **set**: a literal, or a value the mapper computed.
///
/// # Errors
///
/// [`Error`] when the path cannot be written — a malformed path, or a
/// structure the message cannot grow into.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), hl7_2::Error> {
/// use ihb_fields::{read, set};
///
/// let mut message = hl7_2::parse("MSH|^~\\&|252\rPID|1")?;
///
/// set(&mut message, "PID-3.4.1", "NHS")?;
/// assert_eq!(read(&message, "PID-3.4.1"), "NHS");
/// # Ok(())
/// # }
/// ```
pub fn set(message: &mut Message, path: &str, value: &str) -> Result<(), Error> {
    message.set(path, value)
}

/// Copy one path from a source message to the same path in a target — but
/// **only when the source has a value**.
///
/// The Python's `set_nested_field`, and the mapping tables' **copy**.
///
/// The conditional part is the whole point, and it is easy to get wrong.
/// Consider the ChemoCare `PID-3` rows, which set an authority code and then
/// copy an identifier over it:
///
/// ```
/// # fn main() -> Result<(), hl7_2::Error> {
/// use ihb_fields::{copy, read, set};
///
/// let empty_source = hl7_2::parse("MSH|^~\\&|252\rPID|1")?;
/// let mut target = hl7_2::parse("MSH|^~\\&|252\rPID|1")?;
///
/// set(&mut target, "PID-3.4.1", "NHS")?;
/// copy(&empty_source, &mut target, "PID-3.4.1")?;
///
/// // The default survives: an empty source does not clear the target.
/// assert_eq!(read(&target, "PID-3.4.1"), "NHS");
/// # Ok(())
/// # }
/// ```
///
/// Had this cleared the target, every ChemoCare message without an
/// identifier would lose its `NHS` authority code and route nowhere.
///
/// # Errors
///
/// [`Error`] when the target path cannot be written. A source that has
/// nothing is not an error; it is the no-op above.
pub fn copy(source: &Message, target: &mut Message, path: &str) -> Result<(), Error> {
    copy_to(source, target, path, path)
}

/// [`copy`], where the value lands somewhere other than where it came from.
///
/// The PIMS mapping needs this: `PD1-4[1].1` in the source becomes `PD1-3.3`
/// in the target (`spec/09-transformers.md` §5.4).
///
/// # Errors
///
/// As [`copy`].
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), hl7_2::Error> {
/// use ihb_fields::{copy_to, read};
///
/// let source = hl7_2::parse("MSH|^~\\&|252\rPID|1||ABC")?;
/// let mut target = hl7_2::parse("MSH|^~\\&|252\rPID|1")?;
///
/// copy_to(&source, &mut target, "PID-3", "PID-4")?;
/// assert_eq!(read(&target, "PID-4"), "ABC");
/// # Ok(())
/// # }
/// ```
pub fn copy_to(source: &Message, target: &mut Message, from: &str, to: &str) -> Result<(), Error> {
    // Repetition by repetition, never as one value.
    //
    // `Message::set` escapes what it is given, which is correct: a `~`
    // inside a value is data and must survive as an escape sequence. But it
    // means writing a whole field back as `"555-1111~555-2222"` produces
    // ONE repetition containing a literal tilde rather than two
    // repetitions, silently losing a phone number or an identifier. The
    // Python copied "rep-by-rep" for exactly this reason
    // (`copy_segment_fields_in_range`).
    //
    // A path that already pins down a repetition, or goes deeper than the
    // field, has nothing to iterate and is written directly.
    if is_field_path(from) {
        let values = repetitions(source, from);
        if values.is_empty() {
            return Ok(());
        }
        for (index, value) in values.iter().enumerate() {
            if value.is_empty() {
                continue;
            }
            target.set(&format!("{to}[{}]", index + 1), value)?;
        }
        return Ok(());
    }

    read_some(source, from).map_or(Ok(()), |value| target.set(to, &value))
}

/// Whether a path names a whole field, rather than a component, a
/// subcomponent, or one pinned repetition.
///
/// `PID-13` is a field; `PID-13.1`, `PID-13[2]` and `PID-5.1.1` are not.
/// Only a whole field can have repetitions to walk.
fn is_field_path(path: &str) -> bool {
    !path.contains('.') && !path.contains('[')
}

/// Copy a run of fields from one segment to another, inclusive of both ends.
///
/// The Python's `copy_segment_fields_in_range`, and the mapping tables'
/// **copy range a..=b**. Every repetition of every field is preserved,
/// because the field is copied whole — separators included — rather than
/// component by component.
///
/// An empty field in the range is skipped rather than written as empty, so
/// this composes with [`copy`]'s rule.
///
/// # Errors
///
/// [`Error`] when a target path cannot be written.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), hl7_2::Error> {
/// use ihb_fields::{copy_range, read};
///
/// let source = hl7_2::parse("MSH|^~\\&|252\rPID|1|A|B|C|D")?;
/// let mut target = hl7_2::parse("MSH|^~\\&|252\rPID|")?;
///
/// // PID-2 to PID-4; PID-5 is left behind.
/// copy_range(&source, &mut target, "PID", 2, 4)?;
///
/// assert_eq!(read(&target, "PID-2"), "A");
/// assert_eq!(read(&target, "PID-4"), "C");
/// assert_eq!(read(&target, "PID-5"), "");
/// # Ok(())
/// # }
/// ```
pub fn copy_range(
    source: &Message,
    target: &mut Message,
    segment: &str,
    start: usize,
    end: usize,
) -> Result<(), Error> {
    for index in start..=end {
        let path = format!("{segment}-{index}");
        copy(source, target, &path)?;
    }
    Ok(())
}

/// The distinct assigning-authority codes in a `PID` field, in first-seen
/// order.
///
/// The Python's `get_cx_4_hd_1_segment_codes_from_pid_field`. Each
/// repetition of the field contributes its `CX.4.1`; blanks are skipped and
/// duplicates are dropped, keeping the order they first appeared in.
///
/// Feeds the MPI routing properties of `spec/03-message-bus.md` §4 and the
/// `mpi` flow's validation in `spec/07-hl7-server.md` §3.3.2.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), hl7_2::Error> {
/// use ihb_fields::authority_codes;
///
/// let message = hl7_2::parse(
///     "MSH|^~\\&|252\r\
///      PID|1||8888888^^^252^PI~4444444444^^^NHS^NH~9999^^^252^PI",
/// )?;
///
/// // 252 appears twice and is reported once, in the order it first arrived.
/// assert_eq!(authority_codes(&message, 3), ["252", "NHS"]);
/// assert!(authority_codes(&message, 2).is_empty());
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn authority_codes(message: &Message, field: usize) -> Vec<String> {
    let mut codes: Vec<String> = Vec::new();

    for value in repetitions(message, &format!("PID-{field}")) {
        // Component 4 of a CX is the assigning authority; its first
        // subcomponent is the namespace id. Split rather than re-query,
        // because `repetitions` has already pinned down which repetition
        // each value came from and a path query would lose that.
        let Some(authority) = value.split('^').nth(3) else {
            continue;
        };
        let code = authority.split('&').next().unwrap_or(authority).trim();

        if !code.is_empty() && !codes.iter().any(|seen| seen == code) {
            codes.push(code.to_string());
        }
    }

    codes
}

/// Wrap a list of codes in pipes, for a Service Bus routing property.
///
/// Implements `spec/03-message-bus.md` §4.1.
///
/// The leading and trailing pipes are not decoration. The dashboard and the
/// message store query these properties with SQL `LIKE '%|108|%'`, and
/// without the delimiters `108` would match inside `1108` and `1084`. An
/// empty list renders as `""` rather than `"||"`, so an absent value is
/// absent rather than matching every query for the empty code.
///
/// # Examples
///
/// ```
/// use ihb_fields::pipe_wrap;
///
/// assert_eq!(pipe_wrap(&["252".to_string(), "109".to_string()]), "|252|109|");
/// assert_eq!(pipe_wrap(&["108".to_string()]), "|108|");
/// assert_eq!(pipe_wrap(&[]), "");
///
/// // The reason for the delimiters:
/// let wrapped = pipe_wrap(&["1084".to_string()]);
/// assert!(!wrapped.contains("|108|"));
/// ```
#[must_use]
pub fn pipe_wrap(values: &[String]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!("|{}|", values.join("|"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message with the shapes the mapping tables actually meet.
    const MESSAGE: &str = concat!(
        "MSH|^~\\&|252|252|100|100|20260729095037||ADT^A28^ADT_A05|MSG1|P|2.5\r",
        "PID|1||8888888^^^252^PI~4444444444^^^NHS^NH||SURNAME^FORENAME^M||19990101|M|||",
        "99 ROAD^PLACE^CITY^COUNTY^SA99 1XX||555-1111~555-2222\r",
        "PV1||N"
    );

    fn message() -> Message {
        hl7_2::parse(MESSAGE).expect("the fixture parses")
    }

    fn blank() -> Message {
        hl7_2::parse("MSH|^~\\&|||||||ADT^A28|MSG2|P|2.5\rPID|").expect("the fixture parses")
    }

    #[test]
    fn reading_a_present_field_gives_its_value() {
        // spec/09-transformers.md §2.3
        let message = message();
        assert_eq!(read(&message, "MSH-3"), "252");
        assert_eq!(read(&message, "PID-5.1"), "SURNAME");
        assert_eq!(read(&message, "PID-7"), "19990101");
    }

    #[test]
    fn reading_anything_absent_gives_empty_rather_than_failing() {
        // spec/09-transformers.md §2.3 — the whole reason this exists.
        let message = message();
        assert_eq!(read(&message, "PID-99"), "");
        assert_eq!(read(&message, "ZZZ-1"), "");
        assert_eq!(read(&message, "PID-5.99"), "");
    }

    #[test]
    fn a_malformed_path_reads_as_empty_rather_than_panicking() {
        // A mapping-table path is a constant; a bad one is caught by this
        // crate's tests, not by a message at three in the morning.
        let message = message();
        assert_eq!(read(&message, "PID-0"), "");
        assert_eq!(read(&message, "nonsense"), "");
    }

    #[test]
    fn read_some_tells_absent_from_present_but_empty() {
        let message = hl7_2::parse("MSH|^~\\&|252\rPID|1||^^^NHS").unwrap();
        assert_eq!(read_some(&message, "MSH-3").as_deref(), Some("252"));
        assert_eq!(read_some(&message, "PID-3.1"), None);
        assert_eq!(read_some(&message, "PID-99"), None);
    }

    #[test]
    fn every_repetition_is_reported() {
        // spec/09-transformers.md §4.3, §5.3 — PID-13, every repetition.
        let message = message();
        assert_eq!(repetitions(&message, "PID-13"), ["555-1111", "555-2222"]);
    }

    #[test]
    fn a_field_with_no_repetitions_reports_one_value() {
        let message = message();
        assert_eq!(repetitions(&message, "PID-7"), ["19990101"]);
    }

    #[test]
    fn copying_a_present_value_writes_it() {
        // spec/09-transformers.md §2.1
        let source = message();
        let mut target = blank();
        copy(&source, &mut target, "PID-5.1").unwrap();
        assert_eq!(read(&target, "PID-5.1"), "SURNAME");
    }

    #[test]
    fn copying_an_absent_value_leaves_the_target_alone() {
        // spec/09-transformers.md §2.1 — the rule the mapping tables lean on.
        let source = message();
        let mut target = blank();

        set(&mut target, "PID-3.4.1", "NHS").unwrap();
        copy(&source, &mut target, "PID-99").unwrap();

        assert_eq!(
            read(&target, "PID-3.4.1"),
            "NHS",
            "an absent source must not clear a default"
        );
    }

    #[test]
    fn copying_an_empty_value_leaves_the_target_alone() {
        // Present-but-empty counts as nothing to copy, as in the Python:
        // `if not field_content: return False`.
        let source = hl7_2::parse("MSH|^~\\&|252\rPID|1||").unwrap();
        let mut target = blank();

        set(&mut target, "PID-3", "KEEP").unwrap();
        copy(&source, &mut target, "PID-3").unwrap();

        assert_eq!(read(&target, "PID-3"), "KEEP");
    }

    #[test]
    fn copying_can_land_somewhere_else() {
        // spec/09-transformers.md §5.4 — PD1-4[1].1 becomes PD1-3.3.
        let source = message();
        let mut target = blank();
        copy_to(&source, &mut target, "PID-5.1", "PID-6.1").unwrap();
        assert_eq!(read(&target, "PID-6.1"), "SURNAME");
    }

    #[test]
    fn a_range_copies_both_ends_inclusively() {
        // spec/09-transformers.md §2.2
        let source = hl7_2::parse("MSH|^~\\&|252\rPID|1|A|B|C|D").unwrap();
        let mut target = blank();

        copy_range(&source, &mut target, "PID", 2, 4).unwrap();

        assert_eq!(read(&target, "PID-2"), "A");
        assert_eq!(read(&target, "PID-3"), "B");
        assert_eq!(read(&target, "PID-4"), "C");
        assert_eq!(read(&target, "PID-5"), "", "PID-5 is outside the range");
    }

    #[test]
    fn a_range_preserves_repetitions() {
        // spec/09-transformers.md §2.2 — "repetition by repetition".
        let source = message();
        let mut target = blank();

        copy_range(&source, &mut target, "PID", 13, 13).unwrap();

        assert_eq!(repetitions(&target, "PID-13"), ["555-1111", "555-2222"]);
    }

    #[test]
    fn a_copied_field_does_not_collapse_its_repetitions_on_the_wire() {
        // The defect this crate exists to avoid, and the reason `copy_to`
        // walks repetitions. Writing a whole field back through `set`
        // escapes the repetition separator, turning two phone numbers into
        // one nonsense value. The rendered message is what reaches the
        // wire, so that is what this asserts on.
        let source = message();
        let mut target = blank();

        copy(&source, &mut target, "PID-13").unwrap();

        let rendered = target.to_er7();
        assert!(
            rendered.contains("555-1111~555-2222"),
            "repetitions were collapsed: {rendered}"
        );
        assert!(
            !rendered.contains(r"\R\"),
            "the repetition separator was written as escaped data: {rendered}"
        );
    }

    #[test]
    fn a_copied_field_keeps_its_repetitions_addressable() {
        // Not just on the wire: a later mapper reading the target must see
        // two repetitions, not one value with a tilde in it.
        let source = message();
        let mut target = blank();

        copy(&source, &mut target, "PID-13").unwrap();

        assert_eq!(repetitions(&target, "PID-13"), ["555-1111", "555-2222"]);
    }

    #[test]
    fn only_a_whole_field_has_repetitions_to_walk() {
        assert!(is_field_path("PID-13"));
        assert!(!is_field_path("PID-13.1"));
        assert!(!is_field_path("PID-13[2]"));
        assert!(!is_field_path("PID-5.1.1"));
    }

    #[test]
    fn a_component_copy_still_goes_through_the_direct_path() {
        // A component cannot repeat, so it must not be routed through the
        // repetition walk — which would address `PID-5.1[1]` and mean
        // something different.
        let source = message();
        let mut target = blank();

        copy(&source, &mut target, "PID-5.1").unwrap();

        assert_eq!(read(&target, "PID-5.1"), "SURNAME");
    }

    #[test]
    fn a_range_skips_gaps_without_writing_empties() {
        let source = hl7_2::parse("MSH|^~\\&|252\rPID|1||B").unwrap();
        let mut target = blank();

        set(&mut target, "PID-2", "KEEP").unwrap();
        copy_range(&source, &mut target, "PID", 2, 3).unwrap();

        assert_eq!(read(&target, "PID-2"), "KEEP", "the gap did not clear it");
        assert_eq!(read(&target, "PID-3"), "B");
    }

    #[test]
    fn authority_codes_are_deduplicated_in_first_seen_order() {
        // spec/03-message-bus.md §4, spec/07-hl7-server.md §3.3.2.
        let message = hl7_2::parse(
            "MSH|^~\\&|252\rPID|1||8888888^^^252^PI~4444444444^^^NHS^NH~9999^^^252^PI",
        )
        .unwrap();
        assert_eq!(authority_codes(&message, 3), ["252", "NHS"]);
    }

    #[test]
    fn authority_codes_skip_repetitions_without_one() {
        let message =
            hl7_2::parse("MSH|^~\\&|252\rPID|1||8888888^^^^PI~4444444444^^^NHS^NH").unwrap();
        assert_eq!(authority_codes(&message, 3), ["NHS"]);
    }

    #[test]
    fn authority_codes_of_a_missing_field_are_empty() {
        // The MPI outbound validation depends on this being empty rather
        // than an error: spec/07-hl7-server.md §3.3.2.
        let message = message();
        assert!(authority_codes(&message, 2).is_empty());
        assert!(authority_codes(&message, 99).is_empty());
    }

    #[test]
    fn pipe_wrapping_delimits_both_ends() {
        // spec/03-message-bus.md §4.1
        assert_eq!(pipe_wrap(&["252".into(), "109".into()]), "|252|109|");
        assert_eq!(pipe_wrap(&["108".into()]), "|108|");
    }

    #[test]
    fn pipe_wrapping_an_empty_list_gives_an_empty_string() {
        // spec/03-message-bus.md §4.1 — not "||", which would match a
        // query for the empty code.
        assert_eq!(pipe_wrap(&[]), "");
    }

    #[test]
    fn pipe_wrapping_stops_a_substring_match() {
        // The entire reason the delimiters exist.
        assert!(!pipe_wrap(&["1084".into()]).contains("|108|"));
        assert!(!pipe_wrap(&["1108".into()]).contains("|108|"));
        assert!(pipe_wrap(&["108".into()]).contains("|108|"));
    }
}
