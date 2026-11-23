//! Code lookups.
//!
//! Implements `spec/16-proms-transformer.md` §7.
//!
//! # Two kinds of lookup, two behaviours
//!
//! §7.1. They differ on an unrecognised code, and treating them alike gets
//! one of them wrong:
//!
//! - The **resolver seam** ([`ReferenceDataResolver`]) returns `None`, so
//!   the element is omitted (§7.2). It stands in for the Fiorano
//!   workflow's `CoreReferenceDataLookup_Service`, which this estate has
//!   no equivalent of.
//! - The **wiki lookups** ([`nhs_certification_display`], [`dha_code_name`])
//!   return the **input unchanged** (§7.1.2). That is what the JavaScript
//!   on the INSE wiki does, and it is preserved so the output matches the
//!   workflow being replaced — see §9.10 for why it is worth questioning.

/// `NHS_CERTIFICATION` codes and their display text (§7.1.2).
pub const NHS_CERTIFICATION_DISPLAYS: [(&str, &str); 8] = [
    ("01", "Number present & traced"),
    ("02", "Number present but not traced"),
    ("03", "Trace required"),
    ("04", "Trace attempted - no match or multiple match found"),
    (
        "05",
        "Trace needs to be resolved (NHS number or patient detail conflict)",
    ),
    ("06", "Trace in progress"),
    ("07", "Number not present and trace not required"),
    ("08", "Trace postponed (baby under six weeks old)"),
];

/// `DHA_CODE`s and their health board names (§7.1.2).
///
/// The names are reproduced exactly, including their upper-case spelling and
/// the mixed `LHB` / `LOCAL HEALTH BOARD` forms.
pub const DHA_CODE_NAMES: [(&str, &str); 6] = [
    ("7A1", "BETSI CADWALADR UNIVERSITY LHB"),
    ("7A2", "HYWEL DDA UNIVERSITY LHB"),
    ("7A3", "SWANSEA BAY UNIVERSITY LOCAL HEALTH BOARD"),
    ("7A5", "CWM TAF MORGANNWG UNIVERSITY LOCAL HEALTH BOARD"),
    ("7A6", "ANEURIN BEVAN UNIVERSITY LHB"),
    ("7A7", "POWYS TEACHING LOCAL HEALTH BOARD"),
];

/// The display text for an NHS-number certification code (§7.1.2).
///
/// **Returns the code itself when unrecognised**, which is the wiki's
/// behaviour and not §7.2's. See §9.10.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::reference_data::nhs_certification_display;
///
/// assert_eq!(nhs_certification_display("01"), Some("Number present & traced".to_string()));
///
/// // §9.10 — an unrecognised code becomes its own display text.
/// assert_eq!(nhs_certification_display("99"), Some("99".to_string()));
/// assert_eq!(nhs_certification_display(""), None);
/// ```
#[must_use]
pub fn nhs_certification_display(code: &str) -> Option<String> {
    let key = code.trim();
    if key.is_empty() {
        return None;
    }

    Some(
        NHS_CERTIFICATION_DISPLAYS
            .iter()
            .find(|(c, _)| *c == key)
            .map_or_else(|| key.to_string(), |(_, display)| (*display).to_string()),
    )
}

/// The health board name for a `DHA_CODE` (§7.1.2).
///
/// **Returns the code itself when unrecognised.** See §9.10.
#[must_use]
pub fn dha_code_name(code: &str) -> Option<String> {
    let key = code.trim();
    if key.is_empty() {
        return None;
    }

    let upper = key.to_ascii_uppercase();
    Some(
        DHA_CODE_NAMES
            .iter()
            .find(|(c, _)| *c == upper)
            .map_or_else(|| key.to_string(), |(_, name)| (*name).to_string()),
    )
}

/// The codes the Fiorano workflow resolved through a web service (§7.1.1).
///
/// An implementation MUST return `None` for a code it cannot resolve, so
/// the mapper omits the element rather than emitting an unmapped code
/// (§7.2).
pub trait ReferenceDataResolver {
    /// A FHIR `AdministrativeGender` code (`male`, `female`, `other`,
    /// `unknown`), or `None` for a code that cannot be resolved (§7.2).
    /// `system_id` identifies the sending board, for resolvers that hold a
    /// per-board table.
    fn resolve_gender(&self, code: &str, system_id: &str) -> Option<&'static str>;

    /// A `UKCore` `HumanLanguage` code, or `None` for a code that cannot be
    /// resolved (§7.2). The interim resolver always returns `None`.
    fn resolve_language(&self, code: &str, system_id: &str) -> Option<String>;
}

/// §7.3's gender table: WPAS sends either a letter or a digit, and both map
/// to the same FHIR code. Unlisted codes are unresolved, not defaulted.
const GENDER_CODES: [(&str, &str); 10] = [
    ("M", "male"),
    ("1", "male"),
    ("F", "female"),
    ("2", "female"),
    ("O", "other"),
    ("I", "other"),
    ("9", "other"),
    ("U", "unknown"),
    ("0", "unknown"),
    ("X", "unknown"),
];

/// The interim resolver, using a local table (§7.1.1).
///
/// # `resolve_language` always returns `None`
///
/// Not a stub to fill in later — it is the Python's behaviour, and it means
/// `Patient.communication` has never been emitted by this service. §9.9
/// records what that costs. The seam exists so a service-backed resolver
/// can replace this without touching a mapper.
#[derive(Debug, Clone, Copy, Default)]
pub struct StaticResolver;

impl ReferenceDataResolver for StaticResolver {
    fn resolve_gender(&self, code: &str, _system_id: &str) -> Option<&'static str> {
        let key = code.trim().to_ascii_uppercase();
        if key.is_empty() {
            return None;
        }

        let found = GENDER_CODES
            .iter()
            .find(|(c, _)| *c == key)
            .map(|(_, gender)| *gender);

        if found.is_none() {
            // §7.2 — omitted rather than guessed, and said out loud
            // because a patient's recorded sex silently vanishing is worth
            // noticing.
            tracing::warn!(
                code = key,
                "an unmapped WPAS SEX value was received; Patient.gender is omitted"
            );
        }

        found
    }

    fn resolve_language(&self, code: &str, _system_id: &str) -> Option<String> {
        if !code.trim().is_empty() {
            tracing::info!(
                code = code.trim(),
                "PREFERRED_LANGUAGE cannot be resolved without the Core Reference \
                 Data service; Patient.communication is omitted (spec/16 §9.9)"
            );
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_certification_code_has_its_display_text() {
        // spec/16-proms-transformer.md §7.1.2.
        assert_eq!(
            nhs_certification_display("01").as_deref(),
            Some("Number present & traced")
        );
        assert_eq!(
            nhs_certification_display("08").as_deref(),
            Some("Trace postponed (baby under six weeks old)")
        );
        assert_eq!(NHS_CERTIFICATION_DISPLAYS.len(), 8);
    }

    #[test]
    fn an_unrecognised_certification_code_becomes_its_own_display() {
        // §7.1.2 and §9.10 — the opposite of §7.2, and deliberate: it is
        // what the wiki's JavaScript does and what the workflow being
        // replaced emits.
        assert_eq!(nhs_certification_display("99").as_deref(), Some("99"));
        assert_eq!(nhs_certification_display(" 99 ").as_deref(), Some("99"));
    }

    #[test]
    fn an_empty_certification_code_is_absent() {
        for code in ["", "   "] {
            assert_eq!(nhs_certification_display(code), None, "{code:?}");
        }
    }

    #[test]
    fn every_dha_code_names_a_health_board() {
        // §7.1.2.
        assert_eq!(
            dha_code_name("7A1").as_deref(),
            Some("BETSI CADWALADR UNIVERSITY LHB")
        );
        assert_eq!(
            dha_code_name("7a1").as_deref(),
            Some("BETSI CADWALADR UNIVERSITY LHB"),
            "matched case-insensitively"
        );
        assert_eq!(DHA_CODE_NAMES.len(), 6);
    }

    #[test]
    fn an_unrecognised_dha_code_becomes_its_own_name() {
        // §9.10 again — `Organization.name = "7Z9"`.
        assert_eq!(dha_code_name("7Z9").as_deref(), Some("7Z9"));
        assert_eq!(dha_code_name(""), None);
    }

    #[test]
    fn an_unrecognised_code_keeps_its_original_case() {
        // The lookup upper-cases to *match*; the echo returns what
        // arrived, so a reader sees the payload's value rather than a
        // transformed one.
        assert_eq!(dha_code_name("abc").as_deref(), Some("abc"));
    }

    #[test]
    fn every_gender_code_of_section_seven_three() {
        // §7.3 — both the letter and the numeric WPAS dialects.
        let resolver = StaticResolver;

        for (code, expected) in [
            ("M", "male"),
            ("1", "male"),
            ("F", "female"),
            ("2", "female"),
            ("O", "other"),
            ("I", "other"),
            ("9", "other"),
            ("U", "unknown"),
            ("0", "unknown"),
            ("X", "unknown"),
        ] {
            assert_eq!(
                resolver.resolve_gender(code, "108"),
                Some(expected),
                "{code}"
            );
        }
    }

    #[test]
    fn a_gender_code_is_trimmed_and_upper_cased() {
        let resolver = StaticResolver;

        assert_eq!(resolver.resolve_gender(" m ", ""), Some("male"));
        assert_eq!(resolver.resolve_gender("f", ""), Some("female"));
    }

    #[test]
    fn an_unmapped_gender_is_absent_rather_than_guessed() {
        // §7.2 — THE rule for the resolver seam. A guessed sex on a
        // clinical record is worse than a missing one.
        let resolver = StaticResolver;

        for code in ["", "   ", "Z", "male", "3", "🏥"] {
            assert_eq!(resolver.resolve_gender(code, "108"), None, "{code:?}");
        }
    }

    #[test]
    fn language_is_never_resolved() {
        // §9.9 — not a stub. This is the Python's behaviour, and it means
        // `Patient.communication` has never been emitted by this service.
        let resolver = StaticResolver;

        for code in ["cy", "en", "WEL", "", "anything"] {
            assert_eq!(resolver.resolve_language(code, "108"), None, "{code:?}");
        }
    }

    #[test]
    fn the_resolver_is_usable_behind_a_reference() {
        // The mappers take `&dyn ReferenceDataResolver`, so a
        // service-backed one can replace the static one without a mapper
        // change (§7.1.1).
        fn take(resolver: &dyn ReferenceDataResolver) -> Option<&'static str> {
            resolver.resolve_gender("M", "108")
        }

        assert_eq!(take(&StaticResolver), Some("male"));
    }

    #[test]
    fn a_substituted_resolver_changes_what_the_mappers_see() {
        // The seam is the point of §7.1.1: this is what a Core Reference
        // Data implementation would look like from a mapper's side.
        struct Welsh;

        impl ReferenceDataResolver for Welsh {
            fn resolve_gender(&self, _code: &str, _system_id: &str) -> Option<&'static str> {
                Some("unknown")
            }
            fn resolve_language(&self, code: &str, _system_id: &str) -> Option<String> {
                (code == "WEL").then(|| "cy".to_string())
            }
        }

        assert_eq!(Welsh.resolve_language("WEL", "108").as_deref(), Some("cy"));
        assert_eq!(Welsh.resolve_language("XXX", "108"), None);
    }
}
