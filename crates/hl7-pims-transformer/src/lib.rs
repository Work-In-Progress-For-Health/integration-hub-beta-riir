#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! PIMS → MPI.
//!
//! Implements `spec/09-transformers.md` §5.
//!
//! The most opinionated of the three. It rewrites the routing identifiers
//! to constants, normalises the trigger event, strips timezones from every
//! timestamp, and drops anything it was not told to keep.
//!
//! Where it fits: one hop between the `hl7-server` ingress queue and the MPI
//! egress topic, run by [`ihb_transformer::main_loop::run`]. [`Pims`] is the
//! pure mapping (§5.1 to §5.9); [`PimsTransformer`] is the running service,
//! which adds the reference-data lookup of §5.10 (a network call per coded
//! field) and rewrites the `MessageType` property of §5.11 so the egress topic
//! subscriptions still match after the trigger has been normalised.
//!
//! # Two comparisons that look the same and are not
//!
//! §5.1.1 upper-cases the trigger before matching, so `a04` normalises like
//! `A04`. §5.9 — the predicate deciding whether `PD1` and `PV1` are mapped
//! at all — is **case-sensitive** on the raw value, so `a04` is not `A04`
//! and those segments are dropped.
//!
//! That is inconsistent, and it is the Python's behaviour, and it is
//! preserved. A message whose trigger is lower-cased would be re-triggered
//! as `A28` while losing its `PD1` — which is worth knowing about, and is
//! why both sites say so.

pub mod datetime;
pub mod reference_data;

use std::sync::Arc;

use hl7_2::Message;
use ihb_bus::metadata::Properties;
use ihb_fields::{copy, copy_to, ensure_repetitions, read, set, trim_trailing_separators};
use ihb_transformer::{TransformError, Transformer};

use crate::datetime::strip_timezone;
use crate::reference_data::{Dataset, ReferenceData};

/// Segments §5.8 copies verbatim. `PV1` is **absent**, unlike ChemoCare's
/// list, because §5.5 owns it.
pub const NON_SPECIFIC: &[&str] = &[
    "PV2", "OBX", "AL1", "DG1", "PR1", "GT1", "IN1", "IN2", "IN3",
];

/// The trigger a source trigger normalises to, with its structure
/// (`spec/09-transformers.md` §5.1.1).
///
/// # Examples
///
/// ```
/// use hl7_pims_transformer::normalise_trigger;
///
/// assert_eq!(normalise_trigger("A04"), ("A28", "ADT_A05"));
/// assert_eq!(normalise_trigger("a28"), ("A28", "ADT_A05"), "upper-cased first");
/// assert_eq!(normalise_trigger("A08"), ("A31", "ADT_A05"));
/// assert_eq!(normalise_trigger("A31"), ("A31", "ADT_A05"));
///
/// // The catch-all: PIMS sends A40 for merges and nothing else reaches
/// // this transformer.
/// assert_eq!(normalise_trigger("A40"), ("A40", "ADT_A39"));
/// assert_eq!(normalise_trigger("Z99"), ("A40", "ADT_A39"));
/// assert_eq!(normalise_trigger(""), ("A40", "ADT_A39"));
/// ```
#[must_use]
pub fn normalise_trigger(source: &str) -> (&'static str, &'static str) {
    match source.trim().to_ascii_uppercase().as_str() {
        "A04" | "A28" => ("A28", "ADT_A05"),
        "A08" | "A31" => ("A31", "ADT_A05"),
        _ => ("A40", "ADT_A39"),
    }
}

/// Whether `PD1` and `PV1` are mapped (`spec/09-transformers.md` §5.9).
///
/// **Case-sensitive**, on the raw value, unlike [`normalise_trigger`]. See
/// the module documentation for why the difference is kept.
///
/// # Examples
///
/// ```
/// use hl7_pims_transformer::is_a04_or_a08;
///
/// assert!(is_a04_or_a08("A04"));
/// assert!(is_a04_or_a08("A08"));
///
/// assert!(!is_a04_or_a08("a04"), "case-sensitive, unlike §5.1.1");
/// assert!(!is_a04_or_a08("A40"));
/// assert!(!is_a04_or_a08(""));
/// ```
#[must_use]
pub fn is_a04_or_a08(trigger: &str) -> bool {
    trigger == "A04" || trigger == "A08"
}

/// The PIMS mapping.
///
/// This is the mapping **without** §5.10's reference-data enrichment: it
/// needs no network, which is what the golden corpus (captured before
/// upstream's `c1d2a74` added enrichment) and the developer tester want.
/// The running service is [`PimsTransformer`], which adds §5.10 and §5.11.
#[derive(Debug, Default)]
pub struct Pims;

impl Pims {
    /// Map one message, without enrichment (§5.1 to §5.9).
    ///
    /// # Errors
    ///
    /// [`TransformError`] when a timestamp cannot be stripped (§5.6) or a
    /// field cannot be written.
    pub fn map(&self, message: &Message) -> Result<Message, TransformError> {
        self.map_with(message, None)
    }

    /// Map one message and enrich it from `lookup` (§5.10).
    ///
    /// # Errors
    ///
    /// As [`Pims::map`], and [`TransformError::Field`] naming the `PID`
    /// field when a lookup fails (§5.10.3). A failed lookup fails the whole
    /// transform: forwarding the untranslated code would put a PIMS code in a
    /// field the eMPI reads as its own.
    pub fn map_enriched(
        &self,
        message: &Message,
        lookup: &dyn ReferenceData,
    ) -> Result<Message, TransformError> {
        self.map_with(message, Some(lookup))
    }

    /// The mapping, with or without a lookup (§5.0 to §5.9, and §5.10 when
    /// `lookup` is `Some`).
    ///
    /// Order matters: the flat re-parse comes first, the trigger is read from
    /// the *source* (the predicates of §5.7 and §5.9 are on the raw value, not
    /// the normalised one `map_msh` writes), and `PID` is enriched only after
    /// it is mapped, because enrichment reads the destination fields.
    #[allow(clippy::unused_self)] // kept a method so `Pims.map(..)` call sites read the same
    fn map_with(
        &self,
        message: &Message,
        lookup: Option<&dyn ReferenceData>,
    ) -> Result<Message, TransformError> {
        // §5.0 — re-parse flat. `ADT_A39`, which carries the A40 merge,
        // nests `PID`/`PD1`/`MRG`/`PV1` inside a repeating `PATIENT` group,
        // and every mapper below addresses segments directly.
        let flat = ihb_validation::parse(&message.to_er7())
            .map_err(|error| TransformError::Parse(error.to_string()))?;
        let source = &flat;

        let trigger = read(source, "MSH-9.2");
        let a04_or_a08 = is_a04_or_a08(&trigger);

        let mut out = ihb_validation::parse(&skeleton(source, &trigger, a04_or_a08))
            .map_err(|error| TransformError::Parse(error.to_string()))?;

        map_msh(source, &mut out, &trigger)?;
        map_evn(source, &mut out)?;

        if has(source, "PID") {
            map_pid(source, &mut out)?;
            if let Some(lookup) = lookup {
                enrich_pid(source, &mut out, lookup)?;
            }
        }
        if a04_or_a08 {
            if has(source, "PD1") {
                map_pd1(source, &mut out)?;
            }
            // §5.5 — a literal, and only for these two triggers.
            set(&mut out, "PV1-2", "N")?;
        }
        // §5.7 — only for an exactly-`A40` source trigger.
        if trigger == "A40" && has(source, "MRG") {
            map_mrg(source, &mut out)?;
        }

        for name in NON_SPECIFIC {
            if !has(source, name) {
                continue;
            }
            if let Err(error) = copy_segment(source, &mut out, name) {
                tracing::warn!("Error copying segment {name}: {error}");
            }
        }

        trim_trailing_separators(&mut out);
        Ok(out)
    }
}

impl Transformer for Pims {
    /// `PIMS`; the same name as [`PimsTransformer`] so audit text is
    /// identical whichever is running.
    fn name(&self) -> &'static str {
        "PIMS"
    }

    fn transform(&mut self, message: &Message) -> Result<Message, TransformError> {
        self.map(message)
    }
}

/// The service: the mapping, plus §5.10 and §5.11.
///
/// # Examples
///
/// ```
/// use hl7_pims_transformer::PimsTransformer;
/// use hl7_pims_transformer::reference_data::{Dataset, LookupError, ReferenceData};
/// use ihb_transformer::Transformer;
///
/// /// Translates nothing: every code is its own translation.
/// struct Identity;
/// impl ReferenceData for Identity {
///     fn lookup(&self, _: Dataset, code: &str) -> Result<String, LookupError> {
///         Ok(code.to_owned())
///     }
/// }
///
/// let mut transformer = PimsTransformer::new(Arc::new(Identity));
/// let message = ihb_validation::parse(
///     "MSH|^~\\&|PIMS|X|EMPI|EMPI|20250702085450+0000||ADT^A04^ADT_A01|1|P|2.3.1\r\
///      EVN||20250702085440+0000\rPID|||1^01^^^NI||T^T||20000101|F",
/// )?;
/// let out = transformer.transform(&message)?;
/// assert_eq!(ihb_fields::read(&out, "PID-32"), "01");
/// # use std::sync::Arc;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct PimsTransformer {
    /// Where codes are resolved (§5.10); a [`reference_data::LookupClient`]
    /// in the service, a fake in tests. Shared so the binary can hold one
    /// TLS configuration for the process lifetime.
    lookup: Arc<dyn ReferenceData>,
}

impl std::fmt::Debug for PimsTransformer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PimsTransformer").finish_non_exhaustive()
    }
}

impl PimsTransformer {
    /// A transformer resolving codes through `lookup`.
    #[must_use]
    pub fn new(lookup: Arc<dyn ReferenceData>) -> Self {
        Self { lookup }
    }
}

impl Transformer for PimsTransformer {
    /// `PIMS`; operators match audit text on it (§1.5).
    fn name(&self) -> &'static str {
        "PIMS"
    }

    fn transform(&mut self, message: &Message) -> Result<Message, TransformError> {
        Pims.map_enriched(message, &*self.lookup)
    }

    /// §5.11: the incoming properties with `MessageType` replaced by the
    /// transformed `MSH-9.2`. Never fails; see `spec/09-transformers.md`
    /// §1.3.3.
    fn egress_properties(&self, incoming: &Properties, transformed: &Message) -> Properties {
        // §5.11.1. The incoming `MessageType` is the source trigger
        // (A04/A08); the transformed topic's subscriptions filter on the
        // normalised one (A28/A31/A40). Upstream: `PimsTransformer
        // .get_egress_properties`, 04b30e7.
        let mut properties = incoming.clone();
        properties.insert("MessageType".to_owned(), read(transformed, "MSH-9.2"));
        properties
    }
}

/// Whether a source value is absent, blank or the HL7 null (`""`), i.e.
/// has nothing to translate (§5.10.1; upstream `_is_empty_or_hl7_null`).
fn is_blank_or_null(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value == datetime::EXPLICIT_NULL
}

/// §5.10 — replace the four coded `PID` fields with their eMPI codes.
///
/// Upstream: `pid_mapper._apply_reference_lookups`.
fn enrich_pid(
    source: &Message,
    target: &mut Message,
    lookup: &dyn ReferenceData,
) -> Result<(), TransformError> {
    // (dataset, source path, target path, whether a blank target is written)
    //
    // `PID-8` alone writes a blank: §5.3 copied the source code into it
    // already, and skipping the write would leave the untranslated PIMS code
    // in the outgoing message (§5.10.2, upstream 04b30e7).
    let jobs = [
        (Dataset::Gender, read(source, "PID-8"), "PID-8", true),
        (
            Dataset::MaritalStatus,
            read(source, "PID-16.1"),
            "PID-16.1",
            false,
        ),
        (
            Dataset::EthnicGroup,
            read(source, "PID-22.1"),
            "PID-22.1",
            false,
        ),
        (
            Dataset::NhsStatus,
            nhs_status_source(source),
            "PID-32",
            false,
        ),
    ];

    for (dataset, value, path, write_blank) in jobs {
        if is_blank_or_null(&value) {
            continue;
        }
        let code = lookup
            .lookup(dataset, value.trim())
            .map_err(|error| TransformError::field(path, error.0))?;
        if code.is_empty() && !write_blank {
            continue;
        }
        set(target, path, &code)?;
    }
    Ok(())
}

/// §5.10 — the `NI` repetition's `CX.2`, the source of `PID-32`.
///
/// `PID-32` does not exist in HL7 2.3.1, so PIMS carries the NHS number
/// status in the check-digit component of the NHS number. The first
/// repetition whose `CX.5` is `NI` (trimmed, case-insensitive) wins;
/// upstream `_get_nhs_number_status_source`.
fn nhs_status_source(source: &Message) -> String {
    let count = ihb_fields::repetitions(source, "PID-3").len();
    for index in 1..=count {
        if read(source, &format!("PID-3[{index}].5"))
            .trim()
            .eq_ignore_ascii_case("NI")
        {
            return read(source, &format!("PID-3[{index}].2")).trim().to_owned();
        }
    }
    String::new()
}

/// §5.1 — `MSH`, with its constants and its trigger normalisation.
fn map_msh(source: &Message, target: &mut Message, trigger: &str) -> Result<(), TransformError> {
    // §5.1 — the routing identifiers are rewritten to constants. PIMS
    // addresses `PIMS`/`EMPI`; MPI expects `103`/`200`.
    set(target, "MSH-3.1", "103")?;
    set(target, "MSH-4.1", "103")?;
    set(target, "MSH-5.1", "200")?;
    set(target, "MSH-6.1", "200")?;

    let stamped = strip_timezone(&read(source, "MSH-7.1"))?;
    set(target, "MSH-7.1", &stamped)?;

    copy(source, target, "MSH-8")?;

    set(target, "MSH-9.1", "ADT")?;
    let (code, structure) = normalise_trigger(trigger);
    set(target, "MSH-9.2", code)?;
    set(target, "MSH-9.3", structure)?;

    copy(source, target, "MSH-10")?;
    copy(source, target, "MSH-11")?;
    set(target, "MSH-12.1", "2.5")?;
    copy(source, target, "MSH-13")?;
    set(target, "MSH-17", "GBR")?;
    set(target, "MSH-19.1", "EN")?;

    Ok(())
}

/// §5.2 — `EVN`: copies `EVN-1`, and strips the timezone from `EVN-2.1` and
/// `EVN-6.1` when present (§5.6). A malformed timestamp fails the message
/// rather than being forwarded unstripped.
fn map_evn(source: &Message, target: &mut Message) -> Result<(), TransformError> {
    copy(source, target, "EVN-1")?;
    for path in ["EVN-2.1", "EVN-6.1"] {
        let value = read(source, path);
        if !value.is_empty() {
            set(target, path, &strip_timezone(&value)?)?;
        }
    }
    Ok(())
}

/// §5.3 — `PID`, including the two guarded identifier repetitions.
fn map_pid(source: &Message, target: &mut Message) -> Result<(), TransformError> {
    // §5.3.1 and §5.3.2 — the two identifier repetitions, each behind its
    // own guard.
    //
    // The mapped ones are written **in order, packed from the first
    // position**, not into the position they came from. The Python appends
    // with `add_field`, so a message whose first repetition fails its guard
    // has its second identifier land in repetition 1. That matters: MPI
    // reads `PID-3[1]` and would otherwise find an empty repetition where
    // the Python put the patient's identifier.
    let mut written = 0_usize;
    for (source_index, type_code, authority, mapped_type) in
        [(1_usize, "NI", "NHS", "NH"), (2, "PI", "103", "PI")]
    {
        let code = read(source, &format!("PID-3[{source_index}].5"));
        if !code.trim().eq_ignore_ascii_case(type_code) {
            continue;
        }
        let value = read(source, &format!("PID-3[{source_index}].1"));
        let value = value.trim();
        if value.is_empty() {
            continue;
        }

        written += 1;
        set(target, &format!("PID-3[{written}].1"), value)?;
        set(target, &format!("PID-3[{written}].4.1"), authority)?;
        set(target, &format!("PID-3[{written}].5"), mapped_type)?;
    }

    copy(source, target, "PID-5.1.1")?;
    for component in 2..=5 {
        copy(source, target, &format!("PID-5.{component}"))?;
    }

    let birth = read(source, "PID-7.1");
    if !birth.is_empty() {
        set(target, "PID-7.1", &strip_timezone(&birth)?)?;
    }
    copy(source, target, "PID-8")?;

    // §5.3 — the whole of `PID-11.1` into `PID-11.1.1`, because `SAD` does
    // not exist in HL7 2.3.1 and PIMS sends the street as one value.
    copy_to(source, target, "PID-11.1", "PID-11.1.1")?;
    for component in 2..=5 {
        copy(source, target, &format!("PID-11.{component}"))?;
    }

    let phones = ihb_fields::repetitions(source, "PID-13").len();
    for index in 1..=phones {
        copy(source, target, &format!("PID-13[{index}].1"))?;
    }
    ensure_repetitions(target, "PID-13", phones);

    copy(source, target, "PID-14.1")?;

    // §5.3.3 — length, not validity, and unconditionally. A message with
    // no `PID-29` still gets an explicit null, which tells MPI to clear any
    // death date it holds.
    let death = read(source, "PID-29.1");
    if death.chars().count() > 6 {
        set(target, "PID-29.1", &strip_timezone(&death)?)?;
    } else {
        set(target, "PID-29.1", datetime::EXPLICIT_NULL)?;
    }

    Ok(())
}

/// §5.4 — `PD1`: `PD1-4.1` is copied, and a second `PD1-4` repetition (the
/// practice) is moved to `PD1-3.3`. Called only for `A04`/`A08` (§5.9).
fn map_pd1(source: &Message, target: &mut Message) -> Result<(), TransformError> {
    copy(source, target, "PD1-4.1")?;
    // Only when the source `PD1-4` repeats: the second repetition is the
    // practice, which MPI reads from `PD1-3.3`. `[2]` is 1-based here; the
    // spec's `PD1-4[1]` and the Python's `[1]` are the same 0-based repetition.
    if ihb_fields::repetitions(source, "PD1-4").len() > 1 {
        copy_to(source, target, "PD1-4[2].1", "PD1-3.3")?;
    }
    Ok(())
}

/// §5.7 — `MRG`: copies the prior identifier (`MRG-1.1`) and stamps the
/// assigning-authority (`103`) and type (`PI`) literals the spec gives.
/// Called only when the source trigger is exactly `A40`.
fn map_mrg(source: &Message, target: &mut Message) -> Result<(), TransformError> {
    copy(source, target, "MRG-1.1")?;
    set(target, "MRG-1.4.1", "103")?;
    set(target, "MRG-1.5", "PI")?;
    Ok(())
}

/// Whether the message carries a segment with this name (any position).
fn has(message: &Message, name: &str) -> bool {
    message.segments().any(|segment| segment.name == name)
}

/// How many fields the first segment of this name carries; `0` if absent.
fn field_count(message: &Message, name: &str) -> usize {
    message
        .segments()
        .find(|segment| segment.name == name)
        .map_or(0, |segment| segment.fields.len())
}

/// Copy a whole segment, field by field (§5.8).
///
/// The count comes from the source, so no trailing empty fields are invented.
fn copy_segment(source: &Message, target: &mut Message, name: &str) -> Result<(), TransformError> {
    for index in 1..=field_count(source, name) {
        copy(source, target, &format!("{name}-{index}"))?;
    }
    Ok(())
}

/// The output's segments as bare ER7, in §5's order: `MSH`, `EVN`, `PID`,
/// then (for `A04`/`A08`) `PD1` and `PV1`, `MRG` for an exact `A40`, then
/// [`NON_SPECIFIC`].
///
/// `PV1` is emitted for A04/A08 even when the source has none, because §5.5
/// sets `PV1-2` unconditionally; `EVN` is always emitted because `map_evn`
/// always writes `EVN-1`.
fn skeleton(source: &Message, trigger: &str, a04_or_a08: bool) -> String {
    let mut names: Vec<&str> = vec!["MSH", "EVN"];

    if has(source, "PID") {
        names.push("PID");
    }
    if a04_or_a08 {
        if has(source, "PD1") {
            names.push("PD1");
        }
        // §5.5 sets `PV1-2` unconditionally for these triggers, so the
        // segment exists whether or not the source had one.
        names.push("PV1");
    }
    if trigger == "A40" && has(source, "MRG") {
        names.push("MRG");
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

    /// A `PID` segment built by field number.
    ///
    /// `PID-29` is far enough along the segment that writing it as a run of
    /// twenty-odd pipes is unreadable and easy to get wrong by one.
    fn pid(fields: &[(usize, &str)]) -> String {
        let highest = fields.iter().map(|(index, _)| *index).max().unwrap_or(0);
        let mut segment = vec![String::new(); highest + 1];
        segment[0] = "PID".to_string();
        for (index, value) in fields {
            segment[*index] = (*value).to_string();
        }
        segment.join("|")
    }

    /// A minimal A04, which is the shape most of §5 is written against.
    fn a04(pid: &str) -> Message {
        ihb_validation::parse(&format!(
            "MSH|^~\\&|PIMS|BroMor HL7Sender|EMPI|EMPI|20250702085450+0000||\
             ADT^A04^ADT_A01|73726643|P|2.3.1\rEVN||20250702085440+0000\r{pid}"
        ))
        .unwrap()
    }

    #[test]
    fn every_row_of_the_trigger_table() {
        // spec/09-transformers.md §5.1.1 and §6.4.
        for source in ["A04", "A28", "a04", " A28 "] {
            assert_eq!(normalise_trigger(source), ("A28", "ADT_A05"), "{source}");
        }
        for source in ["A08", "A31", "a31"] {
            assert_eq!(normalise_trigger(source), ("A31", "ADT_A05"), "{source}");
        }
        for source in ["A40", "A01", "", "ZZZ"] {
            assert_eq!(normalise_trigger(source), ("A40", "ADT_A39"), "{source}");
        }
    }

    #[test]
    fn the_trigger_predicate_is_case_sensitive_unlike_the_table() {
        // §5.9, and the module documentation. The inconsistency is the
        // Python's and is preserved deliberately.
        assert!(is_a04_or_a08("A04"));
        assert!(!is_a04_or_a08("a04"));
        assert_eq!(
            normalise_trigger("a04"),
            ("A28", "ADT_A05"),
            "yet the same value normalises"
        );
    }

    #[test]
    fn the_first_identifier_is_mapped_only_when_it_is_an_ni() {
        // §5.3.1 and §6.5.
        let mapped = Pims
            .map(&a04("PID|||N4000000001^03^^^NI~N1000001^^^^PI"))
            .unwrap();
        assert_eq!(read(&mapped, "PID-3[1].1"), "N4000000001");
        assert_eq!(read(&mapped, "PID-3[1].4.1"), "NHS");
        assert_eq!(read(&mapped, "PID-3[1].5"), "NH");

        // Wrong type code: not mapped as an NHS number. The `PI` behind it
        // still is, and packs into the first position (§5.3.2a), so what
        // proves the guard fired is the *authority*, not the position.
        let wrong = Pims
            .map(&a04("PID|||N4000000001^03^^^XX~N1000001^^^^PI"))
            .unwrap();
        assert!(!wrong.to_er7().contains("NHS"), "{}", wrong.to_er7());
        assert_eq!(read(&wrong, "PID-3[1].4.1"), "103", "the PI, moved up");

        // Right type code, empty value: not mapped, and the *second*
        // identifier packs into the first position — §5.3.2a, and the shape
        // of the `a08` fixture in the Python's own corpus.
        let empty = Pims.map(&a04("PID|||^03^^^NI~N1000001^^^^PI")).unwrap();
        assert!(!empty.to_er7().contains("NHS"), "{}", empty.to_er7());
        assert_eq!(
            read(&empty, "PID-3[1].1"),
            "N1000001",
            "the surviving identifier moves to PID-3[1], where MPI reads it"
        );
        assert_eq!(read(&empty, "PID-3[1].4.1"), "103");
        assert_eq!(
            read(&empty, "PID-3[2].1"),
            "",
            "and nothing is left behind it"
        );
    }

    #[test]
    fn the_second_identifier_is_mapped_only_when_it_is_a_pi() {
        // §5.3.2 and §6.5.
        let mapped = Pims
            .map(&a04("PID|||N4000000001^03^^^NI~N1000001^^^^PI"))
            .unwrap();
        assert_eq!(read(&mapped, "PID-3[2].1"), "N1000001");
        assert_eq!(read(&mapped, "PID-3[2].4.1"), "103");
        assert_eq!(read(&mapped, "PID-3[2].5"), "PI");

        let wrong = Pims
            .map(&a04("PID|||N4000000001^03^^^NI~N1000001^^^^XX"))
            .unwrap();
        assert_eq!(read(&wrong, "PID-3[2].1"), "");

        // Missing repetition entirely.
        let single = Pims.map(&a04("PID|||N4000000001^03^^^NI")).unwrap();
        assert_eq!(read(&single, "PID-3[2].1"), "");
    }

    #[test]
    fn the_death_date_rule_is_on_length_not_validity() {
        // §5.3.3 — "more than 6 characters", applied unconditionally.
        let long = Pims
            .map(&a04(&pid(&[(3, "1^01^^^NI"), (29, "20250101120000+0000")])))
            .unwrap();
        assert_eq!(read(&long, "PID-29.1"), "20250101120000");

        // Six characters or fewer: the explicit null.
        let short = Pims
            .map(&a04(&pid(&[(3, "1^01^^^NI"), (29, "123456")])))
            .unwrap();
        // The explicit null decodes to the empty string, so `read` cannot
        // see it — that is §2.2.2d, and why `is_explicit_null` exists.
        assert!(ihb_fields::is_explicit_null(&short, "PID-29.1"));

        // And at the rendered level too, because the in-memory tree is not
        // where an explicit null goes missing. The `PID` segment ends with
        // it; the message does not, because §5.5 appends `PV1`.
        let rendered = short.to_er7();
        let pid_segment = rendered
            .split('\r')
            .find(|segment| segment.starts_with("PID|"))
            .expect("the message has a PID");
        assert!(
            pid_segment.ends_with(datetime::EXPLICIT_NULL),
            "{pid_segment}"
        );

        // And a message with no PID-29 at all still gets one: §5.3.3 is
        // applied unconditionally, which tells MPI to clear any death date
        // it holds.
        let absent = Pims.map(&a04("PID|||1^01^^^NI")).unwrap();
        assert!(ihb_fields::is_explicit_null(&absent, "PID-29.1"));
    }

    #[test]
    fn pv1_and_pd1_are_mapped_only_for_a04_and_a08() {
        // §5.4, §5.5 and §5.9.
        let a04_message = Pims.map(&a04("PID|||1^01^^^NI")).unwrap();
        // `PV1-2` is `N`, so the segment renders `PV1||N`: field 1 is empty.
        assert!(
            a04_message.to_er7().contains("PV1||N"),
            "{}",
            a04_message.to_er7()
        );

        let a40 = ihb_validation::parse(
            "MSH|^~\\&|PIMS|BroMor HL7Sender|CPI|BroMor|20250630155035+0000||\
             ADT^A40^ADT_A40|73711860|P|2.3.1\rEVN||20250630155034+0000\r\
             PID|||1^01^^^NI\rPD1||||G7000001~W90001\rPV1||NA",
        )
        .unwrap();
        let mapped = Pims.map(&a40).unwrap();

        assert!(!mapped.to_er7().contains("PV1"), "{}", mapped.to_er7());
        assert!(!mapped.to_er7().contains("PD1"), "{}", mapped.to_er7());
    }

    /// A lookup that answers from a table and refuses anything else, as
    /// upstream's `StubLookupClient` does.
    struct Stub(Vec<(Dataset, &'static str, &'static str)>);

    impl ReferenceData for Stub {
        fn lookup(
            &self,
            dataset: Dataset,
            code: &str,
        ) -> Result<String, crate::reference_data::LookupError> {
            self.0
                .iter()
                .find(|(d, c, _)| *d == dataset && *c == code)
                .map(|(_, _, target)| (*target).to_owned())
                .ok_or_else(|| {
                    crate::reference_data::LookupError(format!(
                        "no stub mapping for dataset '{}'",
                        dataset.code_type()
                    ))
                })
        }
    }

    /// A source with all four coded fields (gender `F`, marital `M`, ethnic
    /// `1`, NHS status `3`).
    fn coded() -> Message {
        a04(&pid(&[
            (3, "N4000000001^3^^^NI~N1000001^^^^PI"),
            (8, "F"),
            (16, "M"),
            (22, "1"),
        ]))
    }

    fn full_table() -> Stub {
        Stub(vec![
            (Dataset::Gender, "F", "2"),
            (Dataset::MaritalStatus, "M", "M1"),
            (Dataset::EthnicGroup, "1", "A"),
            (Dataset::NhsStatus, "3", "7"),
        ])
    }

    #[test]
    fn all_four_codes_are_translated() {
        // spec/09-transformers.md §5.10, upstream test_pid_mapper.
        let out = Pims.map_enriched(&coded(), &full_table()).unwrap();

        assert_eq!(read(&out, "PID-8"), "2");
        assert_eq!(read(&out, "PID-16.1"), "M1");
        assert_eq!(read(&out, "PID-22.1"), "A");
        assert_eq!(read(&out, "PID-32"), "7");
    }

    #[test]
    fn without_a_lookup_nothing_is_enriched() {
        // §5.10 vs the golden corpus: `Pims::map` is the unenriched mapping.
        let out = Pims.map(&coded()).unwrap();

        assert_eq!(read(&out, "PID-8"), "F", "the §5.3 copy");
        assert_eq!(read(&out, "PID-16.1"), "");
        assert_eq!(read(&out, "PID-32"), "");
    }

    #[test]
    fn a_blank_target_leaves_marital_ethnic_and_nhs_unset() {
        // §5.10.2; upstream test_blank_target_code_leaves_field_unset.
        let table = Stub(vec![
            (Dataset::Gender, "F", "F"),
            (Dataset::MaritalStatus, "M", ""),
            (Dataset::EthnicGroup, "1", ""),
            (Dataset::NhsStatus, "3", ""),
        ]);
        let out = Pims.map_enriched(&coded(), &table).unwrap();

        assert_eq!(read(&out, "PID-8"), "F");
        assert_eq!(read(&out, "PID-16.1"), "");
        assert_eq!(read(&out, "PID-22.1"), "");
        assert_eq!(read(&out, "PID-32"), "");
    }

    #[test]
    fn a_blank_gender_target_clears_the_copied_source_code() {
        // §5.10.2; upstream test_blank_gender_target_clears_copied_source_code.
        let table = Stub(vec![
            (Dataset::Gender, "F", ""),
            (Dataset::MaritalStatus, "M", ""),
            (Dataset::EthnicGroup, "1", ""),
            (Dataset::NhsStatus, "3", ""),
        ]);
        let out = Pims.map_enriched(&coded(), &table).unwrap();

        assert_eq!(read(&out, "PID-8"), "", "the PIMS code must not leak");
        assert!(!out.to_er7().contains("|F|"), "{}", out.to_er7());
    }

    #[test]
    fn absent_and_null_sources_are_not_looked_up() {
        // §5.10.1: an empty table would fail any lookup that was attempted.
        let none = Stub(vec![]);
        let bare = a04(&pid(&[(3, "N4000000001^3^^^XX"), (16, "\"\""), (22, "  ")]));

        let out = Pims.map_enriched(&bare, &none).unwrap();

        assert_eq!(read(&out, "PID-16.1"), "");
        assert_eq!(read(&out, "PID-32"), "", "no NI repetition, no source");
    }

    #[test]
    fn the_nhs_status_comes_from_the_ni_repetition_wherever_it_is() {
        // §5.10: first repetition whose CX.5 is NI, case-insensitive.
        let message = a04(&pid(&[(3, "N1^^^^PI~N4000000001^3^^^ni")]));
        let table = Stub(vec![(Dataset::NhsStatus, "3", "7")]);

        let out = Pims.map_enriched(&message, &table).unwrap();

        assert_eq!(read(&out, "PID-32"), "7");
    }

    #[test]
    fn a_failed_lookup_fails_the_transform_without_the_code() {
        // §5.10.3 and spec/index.md §0.8.
        let error = Pims
            .map_enriched(&coded(), &Stub(vec![]))
            .unwrap_err()
            .to_string();

        assert!(error.contains("PID-8"), "{error}");
        assert!(error.contains("gender"), "{error}");
    }

    #[test]
    fn the_outgoing_message_type_is_the_transformed_trigger() {
        // §5.11.1; upstream test_get_egress_properties_*.
        let mut transformer = PimsTransformer::new(Arc::new(full_table()));
        let mut incoming = Properties::new();
        incoming.insert("SourceSystem".into(), "PIMS".into());
        incoming.insert("CorrelationId".into(), "abc".into());
        incoming.insert("MessageType".into(), "ORIGINAL".into());

        let out = transformer.transform(&coded()).unwrap();
        let properties = transformer.egress_properties(&incoming, &out);

        assert_eq!(properties["MessageType"], "A28");
        assert_eq!(properties["SourceSystem"], "PIMS");
        assert_eq!(properties["CorrelationId"], "abc");
        assert_eq!(incoming["MessageType"], "ORIGINAL", "not mutated");

        let alone = transformer.egress_properties(&Properties::new(), &out);
        assert_eq!(alone.len(), 1);
        assert_eq!(alone["MessageType"], "A28");
    }

    #[test]
    fn the_runtime_hook_sends_the_corrected_properties() {
        // §1.3.3 through the blanket `PayloadTransformer` implementation.
        use ihb_transformer::PayloadTransformer;
        let mut transformer = PimsTransformer::new(Arc::new(full_table()));
        let mut incoming = Properties::new();
        incoming.insert("MessageType".into(), "A08".into());

        let (_, properties) = transformer
            .transform_payload_with_properties(&coded().to_er7(), &incoming)
            .unwrap();

        assert_eq!(properties["MessageType"], "A28");
    }

    #[test]
    fn the_routing_identifiers_become_constants() {
        // §5.1 — PIMS addresses `PIMS`/`EMPI`; MPI expects `103`/`200`.
        let mapped = Pims.map(&a04("PID|||1^01^^^NI")).unwrap();

        assert_eq!(read(&mapped, "MSH-3.1"), "103");
        assert_eq!(read(&mapped, "MSH-4.1"), "103");
        assert_eq!(read(&mapped, "MSH-5.1"), "200");
        assert_eq!(read(&mapped, "MSH-6.1"), "200");
        assert_eq!(read(&mapped, "MSH-12.1"), "2.5");
        assert_eq!(read(&mapped, "MSH-17"), "GBR");
        assert_eq!(read(&mapped, "MSH-19.1"), "EN");
    }
}
