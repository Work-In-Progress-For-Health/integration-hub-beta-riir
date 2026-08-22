#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! PHW → MPI.
//!
//! Implements `spec/09-transformers.md` §3.
//!
//! The lightest of the three: it copies the message and normalises two
//! timestamps. Both normalisations are reported in the audit text, which is
//! the only way an operator can tell that a message reaching MPI is not
//! byte-identical to the one PHW sent.

pub mod datetime;

use hl7_2::Message;
use ihb_fields::{copy, copy_range, read, set};
use ihb_transformer::{TransformError, Transformer};

use crate::datetime::{date_of_death, normalise};

/// Segments this transformer maps by name; everything else is copied in
/// source order (`spec/09-transformers.md` §3).
const HANDLED: [&str; 3] = ["MSH", "EVN", "PID"];

/// A timestamp that was rewritten, for the audit text of §3.5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewritten {
    /// As it arrived.
    pub from: String,
    /// As it was forwarded.
    pub to: String,
}

/// What §3.4 and §3.6 changed, if anything.
///
/// `spec/09-transformers.md` §3.5.1: the Python stores these on the
/// transformer instance between `transform_message` and
/// `get_processed_audit_text`. Returning them instead keeps the runtime
/// from carrying per-message state on a shared object — which matters the
/// moment two messages are ever in flight together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// `MSH-7.1`, per §3.4.
    pub datetime: Option<Rewritten>,
    /// `PID-29.1`, per §3.6.
    pub date_of_death: Option<Rewritten>,
}

impl Applied {
    /// Whether either transformation ran.
    #[must_use]
    pub const fn any(&self) -> bool {
        self.datetime.is_some() || self.date_of_death.is_some()
    }

    /// The audit text of §3.5, or `None` when neither ran.
    #[must_use]
    pub fn audit_text(&self) -> Option<String> {
        if !self.any() {
            return None;
        }

        let mut details = Vec::new();
        if let Some(Rewritten { from, to }) = &self.datetime {
            details.push(format!("DateTime transformed from {from} to {to}"));
        }
        if let Some(Rewritten { from, to }) = &self.date_of_death {
            details.push(format!("Date of death transformed from {from} to {to}"));
        }

        Some(format!(
            "HL7 transformations applied: {}",
            details.join("; ")
        ))
    }
}

/// The PHW mapping.
///
/// # Examples
///
/// ```
/// use hl7_phw_transformer::Phw;
/// use ihb_transformer::Transformer;
///
/// let message = ihb_validation::parse(
///     "MSH|^~\\&|252|252|100|100|2025-05-05 23:23:32||ADT^A31^ADT_A05|M1|P|2.5\r\
///      EVN||20250502092900\r\
///      PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M",
/// )?;
///
/// let mut phw = Phw::default();
/// let out = phw.transform(&message)?;
///
/// // §3.4 — the space-separated timestamp is normalised.
/// assert_eq!(ihb_fields::read(&out, "MSH-7.1"), "20250505232332");
///
/// // §3.5 — and the change is reported.
/// assert_eq!(
///     phw.processed_audit_text(&message),
///     "HL7 transformations applied: \
///      DateTime transformed from 2025-05-05 23:23:32 to 20250505232332"
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Default)]
pub struct Phw {
    /// What the last [`Transformer::transform`] changed.
    ///
    /// Held only so [`Transformer::processed_audit_text`] can report it,
    /// because the trait's signature does not carry a result through. §3.5.1
    /// is about not keeping this on a *shared* object; a transformer is
    /// owned by one runtime and sees one message at a time, so this is safe
    /// — and [`Phw::transform_reporting`] is the version that returns it
    /// directly, which is what the tests use.
    applied: Applied,
}

impl Phw {
    /// Transform, returning what was rewritten.
    ///
    /// The honest signature. [`Transformer::transform`] wraps it.
    ///
    /// # Errors
    ///
    /// [`TransformError`] when `MSH-7.1` does not parse (§3.4 step 3), or a
    /// field cannot be written.
    pub fn transform_reporting(
        &self,
        message: &Message,
    ) -> Result<(Message, Applied), TransformError> {
        // A skeleton to build into, carrying the segments §3 says the
        // output has, in the order it says. `hl7apy` builds a message by
        // adding segments as each mapper runs; `hl7-2` writes into a
        // message that already has them, so the shape is decided here and
        // filled in below.
        //
        // `MSH-1`/`MSH-2` come from parsing the header, which is why §3.1's
        // table starts at `MSH-3`.
        let mut out = ihb_validation::parse(&skeleton(message))
            .map_err(|error| TransformError::Parse(error.to_string()))?;

        let mut applied = Applied::default();

        // ---- §3.1 MSH ------------------------------------------------
        copy_range(message, &mut out, "MSH", 3, 6)?;

        // §3.1.1 — `MSH-7` is handled explicitly rather than inside a
        // range, so that an absent source timestamp leaves the field
        // absent. In the Python that mattered because a new message
        // auto-populated it; here nothing is auto-populated (§2.6), and the
        // exclusion is kept so the two produce the same message.
        let source_datetime = read(message, "MSH-7.1");
        if !source_datetime.is_empty() {
            let normalised = normalise(&source_datetime)?;
            set(&mut out, "MSH-7.1", &normalised)?;
            applied.datetime = Some(Rewritten {
                from: source_datetime,
                to: normalised,
            });
        }

        copy_range(message, &mut out, "MSH", 8, 21)?;

        // §3.1 — `MSH-12[0]`, first repetition, failures ignored. The
        // Python wraps this in a bare `except`, and the range above has
        // already copied `MSH-12`; this is a belt-and-braces line that
        // must not be allowed to fail the transform.
        let _ = copy(message, &mut out, "MSH-12[1]");

        // ---- §3.2 EVN ------------------------------------------------
        if has_segment(message, "EVN") {
            copy_segment(message, &mut out, "EVN")?;
        }

        // ---- §3.3 PID ------------------------------------------------
        if has_segment(message, "PID") {
            copy_range(message, &mut out, "PID", 1, 28)?;

            let source_death = read(message, "PID-29.1");
            if !source_death.trim().is_empty() {
                let mapped = date_of_death(&source_death);
                set(&mut out, "PID-29.1", &mapped)?;
                applied.date_of_death = Some(Rewritten {
                    from: source_death,
                    to: mapped,
                });
            }

            copy_range(message, &mut out, "PID", 30, 39)?;
        }

        // ---- Everything else, in source order ------------------------
        copy_other_segments(message, &mut out);

        // The message was built field by field, so it carries trailing
        // separators from wherever a source field had them. The Python's
        // `hl7apy` drops those on the way out; `spec/09-transformers.md`
        // §6.6 compares byte for byte, so this does too.
        ihb_fields::trim_trailing_separators(&mut out);

        Ok((out, applied))
    }
}

impl Transformer for Phw {
    fn name(&self) -> &'static str {
        "PHW"
    }

    fn transform(&mut self, message: &Message) -> Result<Message, TransformError> {
        let (out, applied) = self.transform_reporting(message)?;
        self.applied = applied;
        Ok(out)
    }

    /// §3.5 — the transformation detail, or §1.5's default.
    fn processed_audit_text(&self, message: &Message) -> String {
        self.applied.audit_text().unwrap_or_else(|| {
            format!(
                "{} transformation applied for SENDING_APP: {}",
                self.name(),
                match read(message, "MSH-3.1").as_str() {
                    "" => "UNKNOWN".to_string(),
                    value => value.to_string(),
                }
            )
        })
    }
}

/// The output's segments, in the order §3 gives: `MSH`, `EVN`, `PID`, then
/// every other source segment in source order.
///
/// Only segments the source actually has are included. A transformer that
/// emitted an empty `EVN` for a message that never had one would be adding
/// information, and MPI reads `EVN` for the event it is being told about.
fn skeleton(source: &Message) -> String {
    let mut names: Vec<&str> = Vec::new();

    for handled in HANDLED {
        if handled == "MSH" || source.segments().any(|segment| segment.name == handled) {
            names.push(handled);
        }
    }
    for segment in source.segments() {
        if !HANDLED.contains(&segment.name.as_str()) {
            names.push(&segment.name);
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

/// Whether the message carries a segment with this name.
fn has_segment(message: &Message, name: &str) -> bool {
    message.segments().any(|segment| segment.name == name)
}

/// How many fields a segment carries, or `0` when it is absent.
fn field_count(message: &Message, name: &str) -> usize {
    message
        .segments()
        .find(|segment| segment.name == name)
        .map_or(0, |segment| segment.fields.len())
}

/// Copy a whole segment, field by field.
///
/// `ihb_fields::copy` addresses a field; a whole segment is every field it
/// has. The count comes from the source, so a segment with three fields
/// copies three and does not invent trailing empties — which is what
/// `spec/09-transformers.md` §2.1's "only when the source value is
/// non-empty" amounts to at segment scale.
fn copy_segment(source: &Message, target: &mut Message, name: &str) -> Result<(), TransformError> {
    for index in 1..=field_count(source, name) {
        copy(source, target, &format!("{name}-{index}"))?;
    }
    Ok(())
}

/// Copy every segment this transformer does not map by name, in source
/// order.
fn copy_other_segments(source: &Message, target: &mut Message) {
    for segment in source.segments() {
        let name = segment.name.clone();
        if HANDLED.contains(&name.as_str()) {
            continue;
        }
        // A whole-segment copy, which `ihb_fields::copy` addresses
        // repetition by repetition per §2.2.1.
        if let Err(error) = copy_segment(source, target, &name) {
            // §4.6.1's rule, which PHW inherits: the required mapping has
            // already completed, so a non-specific segment must not fail
            // the transform.
            tracing::warn!(segment = %name, %error, "Error copying segment {name}: {error}");
        }
    }
}
