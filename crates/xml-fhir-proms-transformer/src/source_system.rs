//! Health boards, keyed on the WPAS `SYSTEM_ID`.
//!
//! Implements `spec/16-proms-transformer.md` §5.
//!
//! # Seven boards, all named
//!
//! §5.2. Upstream #423 (`source_systems.py`, from the "Amy_Questions"
//! mapping spreadsheet) trimmed the table to the seven WPAS boards and gave
//! every one a name; the endpoint is derived from the id
//! ([`crate::fhir::WPAS_SOURCE_ENDPOINT_BASE`] plus `/{id}`) rather than
//! stored. Cardiff and Vale (`140`) was removed because it does not use
//! WPAS.
//!
//! # The endpoint is never absent
//!
//! §5.2.2. FHIR requires `source.endpoint` when `source` is present, so an
//! unknown board still gets the derived URL; it just gets no name. That
//! replaces the older `.invalid` placeholder (§10.4).

use crate::fhir::WPAS_SOURCE_ENDPOINT_BASE;

/// A WPAS source health board (§5.1): the fixed facts about one sender, keyed
/// by the `SYSTEM_ID` it puts in the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSystem {
    /// The WPAS `SYSTEM_ID`.
    pub system_id: &'static str,
    /// `MessageHeader.source.name` and the `Organization` name (§6.10).
    pub name: &'static str,
    /// `Patient.identifier[PAS].system` — the board's local PAS identifier.
    pub pas_identifier_system: &'static str,
}

/// What §5.2.2 uses when there is no `SYSTEM_ID` at all; builds the
/// `source.endpoint` as `{base}/unknown`.
pub const UNKNOWN_SYSTEM_ID: &str = "unknown";

/// §5.2's seven health boards, in upstream's order.
///
/// Looked up by `system_id` with [`lookup`].
pub const SOURCE_SYSTEMS: [SourceSystem; 7] = [
    SourceSystem {
        system_id: "108",
        name: "Swansea Bay Health Board",
        pas_identifier_system: "https://fhir.sbuhb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "109",
        name: "Betsi Cadwaladr University Health Board",
        pas_identifier_system: "https://fhir.bcuhb.nhs.wales/Id/central-pas-identifier",
    },
    SourceSystem {
        system_id: "139",
        name: "Aneurin Bevan University Health Board",
        pas_identifier_system: "https://fhir.abuhb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "149",
        name: "Hywel Dda University Health Board",
        pas_identifier_system: "https://fhir.hduhb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "170",
        name: "Powys Teaching Health Board",
        pas_identifier_system: "https://fhir.pthb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "126",
        name: "Cwm Taf Morgannwg University Health Board",
        pas_identifier_system: "https://fhir.ctmuhb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "310",
        name: "Velindre University NHS Trust",
        pas_identifier_system: "https://fhir.vunhst.nhs.wales/Id/pas-identifier",
    },
];

/// Look up a health board (§5.1).
///
/// A pure table lookup. The per-board endpoint override lives in
/// [`endpoint_for`] rather than here: it is the only thing a deployment can
/// supply, and threading it through would mean a runtime `String` in a
/// `'static` table.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::source_system::lookup;
///
/// let hywel_dda = lookup("149").expect("a WPAS board");
/// assert_eq!(hywel_dda.name, "Hywel Dda University Health Board");
///
/// // 140 (Cardiff and Vale) does not use WPAS, and 999 is nothing at all.
/// assert!(lookup("140").is_none());
/// assert!(lookup("999").is_none());
/// ```
#[must_use]
pub fn lookup(system_id: &str) -> Option<SourceSystem> {
    let key = system_id.trim();
    if key.is_empty() {
        return None;
    }

    SOURCE_SYSTEMS.iter().find(|s| s.system_id == key).copied()
}

/// The `MessageHeader.source.endpoint` for a `SYSTEM_ID` (§5.2.2).
///
/// Never empty. A known board gets the deployment's override if there is
/// one (upstream `WPAS_SOURCE_ENDPOINT_{id}`), otherwise
/// `{base}/{id}`; an unknown board gets `{base}/{id as sent}` and a payload
/// with no id gets `{base}/unknown`. Upstream applies the override only to a
/// board that exists, and so does this.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::source_system::endpoint_for;
///
/// assert_eq!(
///     endpoint_for("108", |_| None),
///     "https://fhir.nhs.uk/source-system/wpas/108"
/// );
/// assert_eq!(
///     endpoint_for("", |_| None),
///     "https://fhir.nhs.uk/source-system/wpas/unknown"
/// );
/// assert_eq!(
///     endpoint_for("108", |_| Some("https://example.test".to_string())),
///     "https://example.test"
/// );
/// ```
#[must_use]
pub fn endpoint_for<F>(system_id: &str, endpoint_override: F) -> String
where
    F: FnOnce(&str) -> Option<String>,
{
    let key = system_id.trim();

    if lookup(key).is_some()
        && let Some(url) = endpoint_override(key)
    {
        return url;
    }

    // Upstream: `(system_id or 'unknown').strip()`.
    let suffix = if key.is_empty() {
        UNKNOWN_SYSTEM_ID
    } else {
        key
    };

    format!("{WPAS_SOURCE_ENDPOINT_BASE}/{suffix}")
}

/// The board's PAS identifier system (§6.3).
///
/// `None` for an unknown board, which omits `Patient.identifier[PAS]`
/// entirely rather than guessing a system for a real identifier value.
#[must_use]
pub fn pas_identifier_system(system_id: &str) -> Option<&'static str> {
    lookup(system_id).map(|s| s.pas_identifier_system)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_wpas_health_boards_are_sources() {
        // spec/16-proms-transformer.md §5.2. 103 (PIMS) and 140 (Cardiff and
        // Vale) do not use WPAS, so 140 — configured before #423 — is gone.
        assert_eq!(SOURCE_SYSTEMS.len(), 7);

        let ids: Vec<&str> = SOURCE_SYSTEMS.iter().map(|s| s.system_id).collect();
        assert_eq!(ids, vec!["108", "109", "139", "149", "170", "126", "310"]);
    }

    #[test]
    fn non_wpas_boards_are_not_sources() {
        // §5.2 — 131 was a spreadsheet error for Velindre (310).
        for id in ["103", "131", "140"] {
            assert!(lookup(id).is_none(), "{id}");
        }
    }

    #[test]
    fn every_board_has_a_name() {
        // §5.2 — the names of upstream's `test_source_name_and_endpoint_per_health_board`.
        let expected = [
            ("108", "Swansea Bay Health Board"),
            ("109", "Betsi Cadwaladr University Health Board"),
            ("126", "Cwm Taf Morgannwg University Health Board"),
            ("139", "Aneurin Bevan University Health Board"),
            ("149", "Hywel Dda University Health Board"),
            ("170", "Powys Teaching Health Board"),
            ("310", "Velindre University NHS Trust"),
        ];
        for (id, name) in expected {
            assert_eq!(lookup(id).unwrap().name, name, "{id}");
            assert_eq!(
                endpoint_for(id, |_| None),
                format!("https://fhir.nhs.uk/source-system/wpas/{id}")
            );
        }
    }

    #[test]
    fn every_board_has_a_pas_identifier_system() {
        // §6.3 — this is what makes `Patient.identifier[PAS]` possible.
        for board in &SOURCE_SYSTEMS {
            assert!(
                board.pas_identifier_system.starts_with("https://fhir.")
                    && board.pas_identifier_system.contains(".nhs.wales/Id/"),
                "{}: {}",
                board.system_id,
                board.pas_identifier_system
            );
        }
    }

    #[test]
    fn no_two_boards_share_an_id_or_a_pas_system() {
        // A shared PAS system would put two boards' patients under one
        // identifier namespace, a patient-matching problem.
        let mut ids: Vec<&str> = SOURCE_SYSTEMS.iter().map(|s| s.system_id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate SYSTEM_ID");

        let mut systems: Vec<&str> = SOURCE_SYSTEMS
            .iter()
            .map(|s| s.pas_identifier_system)
            .collect();
        systems.sort_unstable();
        systems.dedup();
        assert_eq!(systems.len(), count, "two boards share a PAS system");
    }

    #[test]
    fn an_unknown_or_absent_id_is_not_a_board() {
        for id in ["999", "", "   ", "abc", "108x"] {
            assert!(lookup(id).is_none(), "{id:?}");
            assert!(pas_identifier_system(id).is_none(), "{id:?}");
        }
    }

    #[test]
    fn an_id_is_trimmed_before_lookup() {
        assert_eq!(lookup(" 108 ").unwrap().system_id, "108");
    }

    #[test]
    fn an_unknown_board_gets_an_endpoint_derived_from_its_id() {
        // §5.2.2 — same base, id as sent, and no name.
        assert_eq!(
            endpoint_for("999", |_| None),
            "https://fhir.nhs.uk/source-system/wpas/999"
        );
        assert_eq!(
            endpoint_for("", |_| None),
            "https://fhir.nhs.uk/source-system/wpas/unknown"
        );
    }

    #[test]
    fn an_override_replaces_the_derived_endpoint_of_a_known_board() {
        // §5.2.2 — upstream `test_endpoint_can_be_overridden_by_environment`.
        let endpoint = endpoint_for("109", |id| {
            assert_eq!(id, "109", "the override is asked for by id");
            Some("https://psom.bcuhb.example".to_string())
        });

        assert_eq!(endpoint, "https://psom.bcuhb.example");
    }

    #[test]
    fn an_override_changes_the_endpoint_and_nothing_else() {
        // An environment variable must not change which PAS system a
        // board's patients are identified under — that would be a
        // patient-matching change made by configuration.
        let board = lookup("149").unwrap();
        assert_eq!(board.name, "Hywel Dda University Health Board");
        assert_eq!(
            pas_identifier_system("149"),
            Some("https://fhir.hduhb.nhs.wales/Id/pas-identifier")
        );
    }

    #[test]
    fn an_override_for_an_unknown_board_changes_nothing() {
        // The override is per-board; there is no board here to override.
        assert_eq!(
            endpoint_for("999", |_| Some("https://real.example".to_string())),
            "https://fhir.nhs.uk/source-system/wpas/999"
        );
    }
}
