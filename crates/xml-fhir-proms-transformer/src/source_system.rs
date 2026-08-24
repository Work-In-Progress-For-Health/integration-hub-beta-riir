//! Health boards, keyed on the WPAS `SYSTEM_ID`.
//!
//! Implements `spec/16-proms-transformer.md` §5.
//!
//! # One documented endpoint out of eight
//!
//! §5.2. The wiki's routing table sends eight `SYSTEM_ID`s to the PROMs
//! queue, and only Swansea Bay has a stated `MessageHeader.source.name` and
//! `endpoint`. The other seven have a PAS identifier system and nothing
//! else.
//!
//! What happens then is §5.2.2, and it is not what the specification first
//! claimed: `name` is omitted, but `endpoint` is **fabricated** as
//! `https://wpas-source.invalid/system-id/{id}`. It is inert — RFC 2606
//! reserves `.invalid` — and FHIR requires `endpoint` when `source` is
//! present, so the alternative is omitting `source` entirely. It is still a
//! synthetic value in a clinical message, which is why §9.2 records it.

/// A WPAS source health board (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSystem {
    /// The WPAS `SYSTEM_ID`.
    pub system_id: &'static str,
    /// `MessageHeader.source.name`, where the wiki states one.
    pub name: Option<&'static str>,
    /// `MessageHeader.source.endpoint`, where the wiki states one.
    pub endpoint: Option<&'static str>,
    /// `Patient.identifier[PAS].system` — the board's local PAS identifier.
    pub pas_identifier_system: &'static str,
}

/// The placeholder endpoint prefix (§5.2.2).
pub const UNKNOWN_ENDPOINT_PREFIX: &str = "https://wpas-source.invalid/system-id/";

/// What §5.2.2 uses when there is no `SYSTEM_ID` at all.
pub const UNKNOWN_SYSTEM_ID: &str = "unknown";

/// §5.2's eight health boards.
pub const SOURCE_SYSTEMS: [SourceSystem; 8] = [
    SourceSystem {
        system_id: "108",
        name: Some("Swansea Bay Health Board"),
        endpoint: Some("https://nhspsom.swanseabayhealthboard.com"),
        pas_identifier_system: "https://fhir.sbuhb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "109",
        name: None,
        endpoint: None,
        pas_identifier_system: "https://fhir.bcuhb.nhs.wales/Id/central-pas-identifier",
    },
    SourceSystem {
        system_id: "139",
        name: None,
        endpoint: None,
        pas_identifier_system: "https://fhir.abuhb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "149",
        name: None,
        endpoint: None,
        pas_identifier_system: "https://fhir.hduhb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "170",
        name: None,
        endpoint: None,
        pas_identifier_system: "https://fhir.pthb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "126",
        name: None,
        endpoint: None,
        pas_identifier_system: "https://fhir.ctmuhb.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "310",
        name: None,
        endpoint: None,
        pas_identifier_system: "https://fhir.vunhst.nhs.wales/Id/pas-identifier",
    },
    SourceSystem {
        system_id: "140",
        name: None,
        endpoint: None,
        pas_identifier_system: "https://fhir.cavuhb.nhs.wales/Id/pas-identifier",
    },
];

/// Look up a health board (§5.1).
///
/// A pure table lookup. The per-board endpoint override lives in
/// [`endpoint_for`] rather than here: it is the only field a deployment can
/// supply, and threading it through this function would mean either a
/// runtime `String` in a `'static` table or an allocation that outlives the
/// message it was made for.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::source_system::lookup;
///
/// let swansea = lookup("108").expect("a documented board");
/// assert_eq!(swansea.name, Some("Swansea Bay Health Board"));
///
/// // Seven of the eight have a PAS system and nothing else.
/// let other = lookup("149").expect("a configured board");
/// assert_eq!(other.name, None);
/// assert_eq!(other.endpoint, None);
///
/// // And an unknown id is not a board at all.
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
/// Never `None`: a board with no documented endpoint gets the placeholder,
/// and so does a payload with no `SYSTEM_ID` at all.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::source_system::endpoint_for;
///
/// assert_eq!(
///     endpoint_for("108", |_| None),
///     "https://nhspsom.swanseabayhealthboard.com"
/// );
///
/// // §5.2.2 — fabricated, and inert: RFC 2606 reserves `.invalid`.
/// assert_eq!(
///     endpoint_for("149", |_| None),
///     "https://wpas-source.invalid/system-id/149"
/// );
/// assert_eq!(
///     endpoint_for("", |_| None),
///     "https://wpas-source.invalid/system-id/unknown"
/// );
/// ```
#[must_use]
pub fn endpoint_for<F>(system_id: &str, endpoint_override: F) -> String
where
    F: FnOnce(&str) -> Option<String>,
{
    let key = system_id.trim();

    // A deployment's override wins over the documented value, and applies
    // only to a board that exists — there is nothing to override otherwise.
    if lookup(key).is_some()
        && let Some(url) = endpoint_override(key)
    {
        return url;
    }

    if let Some(endpoint) = lookup(key).and_then(|found| found.endpoint) {
        return endpoint.to_string();
    }

    let suffix = if key.is_empty() {
        UNKNOWN_SYSTEM_ID
    } else {
        key
    };

    format!("{UNKNOWN_ENDPOINT_PREFIX}{suffix}")
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
    fn eight_health_boards_route_to_the_proms_queue() {
        // spec/16-proms-transformer.md §5.2.
        assert_eq!(SOURCE_SYSTEMS.len(), 8);

        let ids: Vec<&str> = SOURCE_SYSTEMS.iter().map(|s| s.system_id).collect();
        assert_eq!(
            ids,
            vec!["108", "109", "139", "149", "170", "126", "310", "140"]
        );
    }

    #[test]
    fn only_swansea_bay_has_a_documented_name_and_endpoint() {
        // §5.2 — the others are left unset rather than guessed, which is
        // true of `name` and *not* of `endpoint` (§5.2.2).
        let documented: Vec<&str> = SOURCE_SYSTEMS
            .iter()
            .filter(|s| s.name.is_some() || s.endpoint.is_some())
            .map(|s| s.system_id)
            .collect();

        assert_eq!(documented, vec!["108"]);
    }

    #[test]
    fn every_board_has_a_pas_identifier_system() {
        // §6.3 — this is the one field all eight carry, and it is what
        // makes `Patient.identifier[PAS]` possible at all.
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
        // identifier namespace, which is a patient-matching problem rather
        // than a tidiness one.
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
        // §5.2 — "an unknown source produces a bundle with the field
        // absent". True of `name` and of the PAS identifier.
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
    fn an_undocumented_board_gets_the_placeholder_endpoint() {
        // §5.2.2 and §9.2 — THE thing this file records. Not absent:
        // fabricated, and inert by construction.
        assert_eq!(
            endpoint_for("149", |_| None),
            "https://wpas-source.invalid/system-id/149"
        );

        // RFC 2606 reserves `.invalid`, so it can never resolve.
        assert!(endpoint_for("149", |_| None).contains(".invalid/"));
    }

    #[test]
    fn no_system_id_at_all_gets_the_unknown_placeholder() {
        // §5.2.2's second form, which the `MINIMAL_REFERRAL_MESSAGE`
        // fixture produces.
        for id in ["", "   ", "999"] {
            let endpoint = endpoint_for(id, |_| None);
            assert!(endpoint.starts_with(UNKNOWN_ENDPOINT_PREFIX), "{id:?}");
        }

        assert_eq!(
            endpoint_for("", |_| None),
            "https://wpas-source.invalid/system-id/unknown"
        );
        // An id that is not a board keeps its value in the placeholder, so
        // a reader can see which board was meant.
        assert_eq!(
            endpoint_for("999", |_| None),
            "https://wpas-source.invalid/system-id/999"
        );
    }

    #[test]
    fn a_documented_endpoint_is_used_as_it_stands() {
        assert_eq!(
            endpoint_for("108", |_| None),
            "https://nhspsom.swanseabayhealthboard.com"
        );
    }

    #[test]
    fn an_override_supplies_the_endpoint_the_mapping_omits() {
        // §5.2 — "the endpoint is overridable by environment variable so a
        // deployment can supply real values without a code change".
        let endpoint = endpoint_for("149", |id| {
            assert_eq!(id, "149", "the override is asked for by id");
            Some("https://real.hduhb.nhs.wales".to_string())
        });

        assert_eq!(endpoint, "https://real.hduhb.nhs.wales");
    }

    #[test]
    fn an_override_changes_the_endpoint_and_nothing_else() {
        // A deployment supplying a URL is not claiming to know the board's
        // name, and it must not change which PAS system its patients are
        // identified under — that would be a patient-matching change made
        // by setting an environment variable.
        assert_eq!(
            endpoint_for("149", |_| Some("https://real.example".to_string())),
            "https://real.example"
        );

        let board = lookup("149").unwrap();
        assert_eq!(board.name, None);
        assert_eq!(
            board.pas_identifier_system,
            "https://fhir.hduhb.nhs.wales/Id/pas-identifier"
        );
        assert_eq!(
            pas_identifier_system("149"),
            Some("https://fhir.hduhb.nhs.wales/Id/pas-identifier")
        );
    }

    #[test]
    fn an_override_for_an_unknown_board_changes_nothing() {
        // The override is per-board; there is no board here to override,
        // so the placeholder still names the id that was asked for.
        assert_eq!(
            endpoint_for("999", |_| Some("https://real.example".to_string())),
            "https://wpas-source.invalid/system-id/999"
        );
    }

    #[test]
    fn an_override_can_replace_a_documented_endpoint() {
        // Swansea Bay's endpoint is documented, and a deployment may still
        // need to point it somewhere else.
        assert_eq!(
            endpoint_for("108", |_| Some("https://staging.example".to_string())),
            "https://staging.example"
        );
    }
}
