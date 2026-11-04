//! Shared field conversions.
//!
//! Implements `spec/16-proms-transformer.md` §6.1.
//!
//! # Dates carry no timezone and UTC is assumed
//!
//! §6.1.2 and §9.3. WPAS supplies no offset, so every parsed value is
//! stamped UTC. If WPAS means Europe/London, an appointment time is up to
//! an hour early for seven months of the year — and in the direction that
//! makes it look earlier than it is. The Python's own comment says
//! "confirm with the spec owner"; that confirmation does not appear to
//! have happened.

use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::{Date, OffsetDateTime, PrimitiveDateTime, UtcOffset};

/// Date-only formats, tried in order (§6.1.1).
///
/// The WPAS date format is not stated in the wiki mapping, so the common
/// NHS and ISO representations are all accepted.
const DATE_ONLY: [&[BorrowedFormatItem<'_>]; 4] = [
    format_description!("[year]-[month]-[day]"),
    format_description!("[year][month][day]"),
    format_description!("[day]/[month]/[year]"),
    format_description!("[day]-[month]-[year]"),
];

/// Date-and-time formats, tried in order (§6.1.2).
///
/// The first format that parses wins. No zone is parsed: a value is taken as
/// UTC (see `render`).
const DATE_TIME: [&[BorrowedFormatItem<'_>]; 7] = [
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]"),
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
    format_description!("[year]-[month]-[day]T[hour]:[minute]"),
    format_description!("[year]-[month]-[day] [hour]:[minute]"),
    format_description!("[year][month][day][hour][minute][second]"),
    format_description!("[day]/[month]/[year] [hour]:[minute]:[second]"),
    format_description!("[day]/[month]/[year] [hour]:[minute]"),
];

/// Title prefixes seen in the WPAS consultant name field (§6.1.3).
const NAME_PREFIXES: [&str; 14] = [
    "DR",
    "DR.",
    "MR",
    "MR.",
    "MRS",
    "MRS.",
    "MS",
    "MS.",
    "MISS",
    "PROF",
    "PROF.",
    "PROFESSOR",
    "SIR",
    "DAME",
];

/// Parse a WPAS date/time (§6.1.2).
///
/// Returns a **UTC** value: WPAS carries no offset, and a naive datetime is
/// not a FHIR `dateTime`. A date-only value becomes midnight UTC.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::mapping::to_fhir_datetime;
///
/// assert_eq!(
///     to_fhir_datetime("2026-08-22T09:15:00").as_deref(),
///     Some("2026-08-22T09:15:00Z")
/// );
///
/// // A date alone is midnight.
/// assert_eq!(
///     to_fhir_datetime("2026-08-22").as_deref(),
///     Some("2026-08-22T00:00:00Z")
/// );
///
/// // Unparseable leaves the element unset, never a guess.
/// assert_eq!(to_fhir_datetime("not a date"), None);
/// ```
#[must_use]
pub fn to_fhir_datetime(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }

    // An ISO value carrying its own offset is taken as it stands.
    if let Ok(parsed) = OffsetDateTime::parse(
        &value.replace('Z', "+00:00"),
        &time::format_description::well_known::Rfc3339,
    ) {
        return Some(render(parsed.to_offset(UtcOffset::UTC)));
    }

    for format in DATE_TIME {
        if let Ok(parsed) = PrimitiveDateTime::parse(value, format) {
            return Some(render(parsed.assume_utc()));
        }
    }

    // §6.1.2 — a date-only value is a valid dateTime at midnight.
    to_fhir_date(value).map(|date| format!("{date}T00:00:00Z"))
}

/// Parse a WPAS date (§6.1.1).
///
/// Accepts an ISO datetime as well, taking its date part.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::mapping::to_fhir_date;
///
/// assert_eq!(to_fhir_date("2026-08-22").as_deref(), Some("2026-08-22"));
/// assert_eq!(to_fhir_date("22/08/2026").as_deref(), Some("2026-08-22"));
/// assert_eq!(to_fhir_date("20260822").as_deref(), Some("2026-08-22"));
/// ```
#[must_use]
pub fn to_fhir_date(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }

    // §6.1.1 — an ISO datetime may be supplied where only a date is needed.
    for format in DATE_TIME {
        if let Ok(parsed) = PrimitiveDateTime::parse(value, format) {
            return Some(iso_date(parsed.date()));
        }
    }

    for format in DATE_ONLY {
        if let Ok(parsed) = Date::parse(value, format) {
            return Some(iso_date(parsed));
        }
    }

    None
}

/// A date as FHIR spells it: zero-padded `YYYY-MM-DD`.
fn iso_date(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

/// A datetime as FHIR spells it: `YYYY-MM-DDThh:mm:ssZ`. The `Z` is asserted,
/// not converted, because the source carries no zone (§6.1.2).
fn render(at: OffsetDateTime) -> String {
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        iso_date(at.date()),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// A person's name, split for FHIR (§6.1.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonName {
    /// Recognised titles, in source order (§6.1.3's list).
    pub prefixes: Vec<String>,
    /// Given names.
    pub given: Vec<String>,
    /// The family name.
    pub family: String,
}

impl PersonName {
    /// Whether anything usable was found; an empty name is omitted from the
    /// resource rather than emitted as `{}` (§6.0.1).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty() && self.given.is_empty() && self.family.is_empty()
    }
}

/// Split a single WPAS name field (§6.1.3).
///
/// Two conventions occur and they are **opposite**, so the comma decides:
///
/// - `"Chess, James"` — family first, which is unambiguous.
/// - `"Dr James Chess"` — title, given names, then family last. This is the
///   opposite of the "surname, given" form used elsewhere in HL7, so
///   guessing wrongly swaps a clinician's names.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::mapping::split_person_name;
///
/// let spaced = split_person_name("Dr James Chess");
/// assert_eq!(spaced.prefixes, vec!["Dr"]);
/// assert_eq!(spaced.given, vec!["James"]);
/// assert_eq!(spaced.family, "Chess");
///
/// // A comma means family first.
/// let comma = split_person_name("Jones, Hannah");
/// assert_eq!(comma.family, "Jones");
/// assert_eq!(comma.given, vec!["Hannah"]);
/// ```
#[must_use]
pub fn split_person_name(person_name: &str) -> PersonName {
    let value = person_name.trim();
    if value.is_empty() {
        return PersonName::default();
    }

    let is_prefix = |token: &str| NAME_PREFIXES.contains(&token.to_ascii_uppercase().as_str());

    if let Some((family, remainder)) = value.split_once(',') {
        let tokens: Vec<&str> = remainder.split_whitespace().collect();

        return PersonName {
            prefixes: tokens
                .iter()
                .filter(|t| is_prefix(t))
                .map(|t| (*t).to_string())
                .collect(),
            given: tokens
                .iter()
                .filter(|t| !is_prefix(t))
                .map(|t| (*t).to_string())
                .collect(),
            family: family.trim().to_string(),
        };
    }

    let mut tokens: Vec<&str> = value.split_whitespace().collect();

    let mut prefixes = Vec::new();
    while tokens.first().is_some_and(|t| is_prefix(t)) {
        prefixes.push(tokens.remove(0).to_string());
    }

    // A value that was nothing but a title is not a usable name.
    let Some(family) = tokens.pop() else {
        return PersonName {
            prefixes,
            given: Vec::new(),
            family: String::new(),
        };
    };

    PersonName {
        prefixes,
        given: tokens.into_iter().map(ToString::to_string).collect(),
        family: family.to_string(),
    }
}

/// Join the parts of a `Reference.display`, skipping empty values.
///
/// `None` when nothing is available, so the element is omitted rather than
/// emitted blank.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::mapping::join_display;
///
/// assert_eq!(join_display(&["Ann", "Jones"]).as_deref(), Some("Ann Jones"));
/// assert_eq!(join_display(&["", "Jones"]).as_deref(), Some("Jones"));
/// assert_eq!(join_display(&["", "  "]), None);
/// ```
#[must_use]
pub fn join_display(parts: &[&str]) -> Option<String> {
    let present: Vec<&str> = parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    (!present.is_empty()).then(|| present.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_date_only_format_is_accepted() {
        // spec/16-proms-transformer.md §6.1.1 — the WPAS format is not
        // stated, so the common NHS and ISO representations all work.
        for raw in ["2026-08-22", "20260822", "22/08/2026", "22-08-2026"] {
            assert_eq!(to_fhir_date(raw).as_deref(), Some("2026-08-22"), "{raw}");
        }
    }

    #[test]
    fn every_date_time_format_is_accepted() {
        // §6.1.2.
        for raw in [
            "2026-08-22T09:15:00",
            "2026-08-22 09:15:00",
            "20260822091500",
            "22/08/2026 09:15:00",
        ] {
            assert_eq!(
                to_fhir_datetime(raw).as_deref(),
                Some("2026-08-22T09:15:00Z"),
                "{raw}"
            );
        }

        // Minute precision, seconds defaulting to zero.
        for raw in ["2026-08-22T09:15", "2026-08-22 09:15", "22/08/2026 09:15"] {
            assert_eq!(
                to_fhir_datetime(raw).as_deref(),
                Some("2026-08-22T09:15:00Z"),
                "{raw}"
            );
        }
    }

    #[test]
    fn a_naive_value_is_stamped_utc() {
        // §6.1.2 and §9.3 — THE assumption. If WPAS means Europe/London,
        // every one of these is an hour out for seven months of the year.
        assert!(
            to_fhir_datetime("2026-08-22T09:15:00")
                .unwrap()
                .ends_with('Z')
        );
    }

    #[test]
    fn a_value_carrying_its_own_offset_is_converted_not_relabelled() {
        // A payload that does supply an offset must not have it
        // overwritten — that would move the appointment.
        assert_eq!(
            to_fhir_datetime("2026-08-22T11:15:00+02:00").as_deref(),
            Some("2026-08-22T09:15:00Z")
        );
        assert_eq!(
            to_fhir_datetime("2026-08-22T09:15:00Z").as_deref(),
            Some("2026-08-22T09:15:00Z")
        );
    }

    #[test]
    fn a_date_only_value_becomes_midnight() {
        // §6.1.2's last resort.
        assert_eq!(
            to_fhir_datetime("2026-08-22").as_deref(),
            Some("2026-08-22T00:00:00Z")
        );
        assert_eq!(
            to_fhir_datetime("22/08/2026").as_deref(),
            Some("2026-08-22T00:00:00Z")
        );
    }

    #[test]
    fn a_datetime_supplied_where_a_date_is_wanted_keeps_its_date() {
        // §6.1.1.
        assert_eq!(
            to_fhir_date("2026-08-22T09:15:00").as_deref(),
            Some("2026-08-22")
        );
    }

    #[test]
    fn an_unparseable_value_leaves_the_element_unset() {
        // §6.0.1 — never a default. A birth date guessed as today, or an
        // appointment guessed as the epoch, is worse than a missing one.
        for raw in ["", "   ", "not a date", "2026-13-45", "99/99/9999", "🏥"] {
            assert_eq!(to_fhir_date(raw), None, "{raw:?}");
            assert_eq!(to_fhir_datetime(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn a_spaced_name_is_title_given_family() {
        // §6.1.3 — "Dr James Chess". This is the **opposite** of HL7's
        // usual surname-first, so getting it wrong swaps a clinician's
        // names rather than failing.
        let name = split_person_name("Dr James Chess");

        assert_eq!(name.prefixes, vec!["Dr"]);
        assert_eq!(name.given, vec!["James"]);
        assert_eq!(name.family, "Chess");
    }

    #[test]
    fn a_comma_name_is_family_first() {
        // §6.1.3 — "Jones, Hannah", the consultant form.
        let name = split_person_name("Jones, Hannah");

        assert_eq!(name.family, "Jones");
        assert_eq!(name.given, vec!["Hannah"]);
        assert!(name.prefixes.is_empty());
    }

    #[test]
    fn the_referrer_form_has_no_title_and_an_initial() {
        // §6.9 — "ARDERN-JONES L" is what a referral carries.
        let name = split_person_name("ARDERN-JONES L");

        assert_eq!(name.given, vec!["ARDERN-JONES"]);
        assert_eq!(name.family, "L");
    }

    #[test]
    fn several_titles_and_middle_names_are_kept() {
        let name = split_person_name("Prof Dr Ann Marie Jones");

        assert_eq!(name.prefixes, vec!["Prof", "Dr"]);
        assert_eq!(name.given, vec!["Ann", "Marie"]);
        assert_eq!(name.family, "Jones");
    }

    #[test]
    fn a_title_is_recognised_whatever_its_case_or_full_stop() {
        for raw in ["dr James Chess", "DR James Chess", "Dr. James Chess"] {
            let name = split_person_name(raw);
            assert_eq!(name.prefixes.len(), 1, "{raw}");
            assert_eq!(name.family, "Chess", "{raw}");
        }
    }

    #[test]
    fn a_title_inside_a_comma_name_is_still_a_prefix() {
        let name = split_person_name("Chess, Dr James");

        assert_eq!(name.family, "Chess");
        assert_eq!(name.prefixes, vec!["Dr"]);
        assert_eq!(name.given, vec!["James"]);
    }

    #[test]
    fn a_single_token_is_a_family_name() {
        let name = split_person_name("Chess");

        assert_eq!(name.family, "Chess");
        assert!(name.given.is_empty());
    }

    #[test]
    fn a_value_that_is_only_a_title_is_not_a_usable_name() {
        // Emitting `family: "Dr"` would put a title in a clinician's
        // surname on every downstream screen.
        let name = split_person_name("Dr");

        assert_eq!(name.prefixes, vec!["Dr"]);
        assert!(name.family.is_empty());
        assert!(name.given.is_empty());
    }

    #[test]
    fn an_empty_name_is_empty() {
        for raw in ["", "   "] {
            assert!(split_person_name(raw).is_empty(), "{raw:?}");
        }
    }

    #[test]
    fn a_display_skips_empty_parts_and_is_absent_when_there_are_none() {
        assert_eq!(
            join_display(&["Ann", "Jones"]).as_deref(),
            Some("Ann Jones")
        );
        assert_eq!(join_display(&["", "Jones"]).as_deref(), Some("Jones"));
        assert_eq!(join_display(&[" Ann ", " "]).as_deref(), Some("Ann"));
        assert_eq!(join_display(&["", "  "]), None);
        assert_eq!(join_display(&[]), None);
    }
}
