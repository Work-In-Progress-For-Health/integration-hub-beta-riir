//! Routing properties: the MPI table, the standard set, and each flow's mix.
//!
//! Implements `spec/03-message-bus.md` §4 and `spec/07-hl7-server.md` §6.2.
//!
//! These properties ride on the Service Bus message so that a subscription
//! can filter on them without opening the payload. [`flow_properties`] says
//! which flow gets which; [`mpi_properties`] is the five-key MPI table.
//!
//! # Why a failure here is not fatal
//!
//! `spec/07-hl7-server.md` §5.4 makes step 9 non-blocking: a message whose
//! routing properties could not be built is still forwarded. That is the
//! right trade. Routing properties are an optimisation — a subscriber that
//! does not match on them still receives the message and reads the payload —
//! so losing them costs efficiency, while dropping the message costs a
//! patient record. This module reflects that by never failing: every field
//! it cannot read becomes an empty value, which is what §4.1 says an empty
//! list renders as anyway.

use hl7_2::Message;
use ihb_bus::metadata::Properties;
use ihb_fields::{authority_codes, pipe_wrap, read};

/// `MSH-9.2`, the trigger event.
pub const MESSAGE_TYPE: &str = "MessageType";

/// Pipe-wrapped `PID-2` assigning authority codes.
pub const UPDATE_SOURCES: &str = "UpdateSources";

/// Pipe-wrapped `PID-3` assigning authority codes.
pub const ASSIGNING_AUTHORITIES: &str = "AssigningAuthorities";

/// `PID-29.1`, the date of death.
pub const DATE_DEATH: &str = "DateDeath";

/// `PID-30`, the death indicator.
pub const REASON_DEATH: &str = "ReasonDeath";

/// `MSH-3`, the sending application.
pub const SENDING_APPLICATION: &str = "SendingApplication";

/// `MSH-9.2` again, under the name Mosaiq's subscription filters use.
pub const TRIGGER_EVENT: &str = "TriggerEvent";

/// The standard routing set (`spec/03-message-bus.md` §4, `d9cc216`).
///
/// Every key is always present, empty when the message cannot supply it, for
/// the same reason as [`mpi_properties`].
#[must_use]
pub fn standard_properties(message: &Message) -> Properties {
    let mut properties = Properties::new();

    properties.insert(SENDING_APPLICATION.to_string(), read(message, "MSH-3"));
    properties.insert(TRIGGER_EVENT.to_string(), read(message, "MSH-9.2"));
    properties.insert(MESSAGE_TYPE.to_string(), read(message, "MSH-9.2"));
    properties.insert(
        UPDATE_SOURCES.to_string(),
        pipe_wrap(&authority_codes(message, 2)),
    );
    properties.insert(
        ASSIGNING_AUTHORITIES.to_string(),
        pipe_wrap(&authority_codes(message, 3)),
    );

    properties
}

/// Mosaiq's two properties and nothing else (`spec/07-hl7-server.md` §6.2
/// item 3).
#[must_use]
pub fn mosaiq_properties(message: &Message) -> Properties {
    let mut properties = Properties::new();

    properties.insert(SENDING_APPLICATION.to_string(), read(message, "MSH-3"));
    properties.insert(TRIGGER_EVENT.to_string(), read(message, "MSH-9.2"));

    properties
}

/// The routing properties a flow adds to the tracking ones
/// (`spec/07-hl7-server.md` §6.2).
///
/// `flow` is the raw `HL7_VALIDATION_FLOW`: `None` is a server with no flow
/// configured, which still gets the standard set, as it does upstream.
///
/// # Examples
///
/// ```
/// use hl7_server::routing::{flow_properties, TRIGGER_EVENT};
///
/// let message = ihb_validation::parse(
///     "MSH|^~\\&|285|285|100|100|20250505232332||ADT^A28^ADT_A05|M1|P|2.5",
/// )?;
///
/// let mosaiq = flow_properties(Some("mosaiq"), &message);
/// assert_eq!(mosaiq.len(), 2);
/// assert_eq!(mosaiq[TRIGGER_EVENT], "A28");
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
#[must_use]
pub fn flow_properties(flow: Option<&str>, message: &Message) -> Properties {
    // Mosaiq skips the standard set rather than overriding it: the extra
    // keys would reach a subscription filter that was written without them.
    if flow == Some("mosaiq") {
        return mosaiq_properties(message);
    }

    let mut properties = standard_properties(message);
    if matches!(flow, Some("mpi" | "wds")) {
        properties.extend(mpi_properties(message));
    }
    properties
}

/// Build the MPI routing properties.
///
/// Implements the second table of `spec/03-message-bus.md` §4.
///
/// All five keys are always present, even when their values are empty. A
/// subscription rule referring to a property that does not exist behaves
/// differently from one referring to an empty property, and the Python's
/// `custom_message_properties.py` sets all five unconditionally.
///
/// # Examples
///
/// ```
/// use hl7_server::routing::{mpi_properties, UPDATE_SOURCES, ASSIGNING_AUTHORITIES};
///
/// let message = ihb_validation::parse(
///     "MSH|^~\\&|100|100|252|252|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
///      PID|1|8888888^^^252^PI|4444444444^^^NHS^NH~9999^^^108^PI",
/// )?;
///
/// let properties = mpi_properties(&message);
///
/// assert_eq!(properties[UPDATE_SOURCES], "|252|");
/// assert_eq!(properties[ASSIGNING_AUTHORITIES], "|NHS|108|");
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
///
/// A message carrying none of them still gets all five keys, with the empty
/// rendering §4.1 requires:
///
/// ```
/// use hl7_server::routing::{mpi_properties, UPDATE_SOURCES};
///
/// let message = ihb_validation::parse(
///     "MSH|^~\\&|100|100|252|252|20250505232332||ADT^A31^ADT_A05|M1|P|2.5",
/// )?;
///
/// let properties = mpi_properties(&message);
///
/// assert_eq!(properties.len(), 5);
/// assert_eq!(properties[UPDATE_SOURCES], "", "not \"||\" — see §4.1");
/// # Ok::<(), ihb_validation::ValidationError>(())
/// ```
#[must_use]
pub fn mpi_properties(message: &Message) -> Properties {
    let mut properties = Properties::new();

    properties.insert(MESSAGE_TYPE.to_string(), read(message, "MSH-9.2"));
    properties.insert(
        UPDATE_SOURCES.to_string(),
        pipe_wrap(&authority_codes(message, 2)),
    );
    properties.insert(
        ASSIGNING_AUTHORITIES.to_string(),
        pipe_wrap(&authority_codes(message, 3)),
    );
    properties.insert(DATE_DEATH.to_string(), read(message, "PID-29.1"));
    properties.insert(REASON_DEATH.to_string(), read(message, "PID-30"));

    properties
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(er7: &str) -> Message {
        ihb_validation::parse(er7).expect("the test message parses")
    }

    const A28: &str = "MSH|^~\\&|169|169|100|100|20250505232332||ADT^A28^ADT_A05|M1|P|2.5\r\
                       PID|1|8888888^^^169^PI|4444444444^^^NHS^NH";

    #[test]
    fn mosaiq_gets_its_two_and_nothing_else() {
        // spec/07-hl7-server.md §6.2 item 3: no MessageType, no UpdateSources.
        let properties = flow_properties(Some("mosaiq"), &parse(A28));

        assert_eq!(properties.len(), 2);
        assert_eq!(properties[SENDING_APPLICATION], "169");
        assert_eq!(properties[TRIGGER_EVENT], "A28");
    }

    #[test]
    fn paris_and_unconfigured_flows_get_the_standard_set() {
        // spec/07-hl7-server.md §6.2 items 1 and 4.
        for flow in [Some("paris"), Some("chemo"), None] {
            let properties = flow_properties(flow, &parse(A28));

            assert_eq!(properties.len(), 5, "{flow:?}");
            assert_eq!(properties[MESSAGE_TYPE], "A28");
            assert_eq!(properties[UPDATE_SOURCES], "|169|");
            assert_eq!(properties[ASSIGNING_AUTHORITIES], "|NHS|");
            assert!(!properties.contains_key(DATE_DEATH));
        }
    }

    #[test]
    fn mpi_and_wds_add_the_death_fields_to_the_standard_set() {
        // spec/07-hl7-server.md §6.2 item 2: seven keys, no duplicates.
        for flow in ["mpi", "wds"] {
            let properties = flow_properties(Some(flow), &parse(A28));

            assert_eq!(properties.len(), 7, "{flow}");
            assert!(properties.contains_key(DATE_DEATH));
            assert!(properties.contains_key(REASON_DEATH));
            assert_eq!(properties[SENDING_APPLICATION], "169");
        }
    }

    #[test]
    fn pims_gets_the_standard_set_whose_message_type_is_the_source_trigger() {
        // spec/09-transformers.md §5.12.1; upstream `build_pims_properties`
        // (04b30e7) returns only `{"MessageType": MSH-9.2}`, which the
        // standard set already contains, so `pims` needs no branch here.
        let properties = flow_properties(Some("pims"), &parse(A28));

        assert_eq!(properties[MESSAGE_TYPE], "A28");
        assert_eq!(properties.len(), 5, "the standard set, no death fields");
        assert!(!properties.contains_key(DATE_DEATH));
    }

    /// A `PID` segment built by field number.
    ///
    /// `PID-29` and `PID-30` sit far enough along the segment that writing
    /// them as a run of twenty-odd pipes is unreadable and — as the first
    /// draft of this file proved — easy to get wrong by one. Placing fields
    /// by index makes the fixture say which field it means.
    fn pid(fields: &[(usize, &str)]) -> String {
        let highest = fields.iter().map(|(index, _)| *index).max().unwrap_or(0);
        let mut segment = vec![String::new(); highest + 1];
        segment[0] = "PID".to_string();
        for (index, value) in fields {
            segment[*index] = (*value).to_string();
        }
        segment.join("|")
    }

    /// A deceased patient with identifiers from two authorities, which is
    /// the shape that exercises every row of the table at once.
    fn full() -> Message {
        parse(&format!(
            "MSH|^~\\&|100|100|252|252|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r{}",
            pid(&[
                (1, "1"),
                (2, "8888888^^^252^PI~7777777^^^109^PI"),
                (3, "4444444444^^^NHS^NH"),
                (5, "SURNAME^FORENAME"),
                (7, "19990101"),
                (8, "M"),
                (29, "20240101120000"),
                (30, "Y"),
            ])
        ))
    }

    #[test]
    fn every_row_of_the_mpi_table() {
        // spec/03-message-bus.md §4, second table.
        let properties = mpi_properties(&full());

        assert_eq!(properties[MESSAGE_TYPE], "A31");
        assert_eq!(properties[UPDATE_SOURCES], "|252|109|");
        assert_eq!(properties[ASSIGNING_AUTHORITIES], "|NHS|");
        assert_eq!(properties[DATE_DEATH], "20240101120000");
        assert_eq!(properties[REASON_DEATH], "Y");
    }

    #[test]
    fn the_keys_are_frozen() {
        // spec/03-message-bus.md §4 — a cross-service contract, so the
        // spelling is asserted rather than assumed. A subscription rule in
        // Terraform names these strings.
        let properties = mpi_properties(&full());
        let keys: Vec<&str> = properties.keys().map(String::as_str).collect();

        assert_eq!(
            keys,
            [
                "AssigningAuthorities",
                "DateDeath",
                "MessageType",
                "ReasonDeath",
                "UpdateSources",
            ],
            "note the exact spelling and casing of each"
        );
    }

    #[test]
    fn all_five_keys_exist_even_when_nothing_fills_them() {
        // The Python sets all five unconditionally. A subscription rule
        // testing an absent property is not the same as one testing an
        // empty property, so "no value" must still mean "the key is there".
        let bare = parse("MSH|^~\\&|100|100|252|252|20250505232332||ADT^A31^ADT_A05|M1|P|2.5");
        let properties = mpi_properties(&bare);

        assert_eq!(properties.len(), 5);
        for key in [
            UPDATE_SOURCES,
            ASSIGNING_AUTHORITIES,
            DATE_DEATH,
            REASON_DEATH,
        ] {
            assert_eq!(properties[key], "", "{key}");
        }
        assert_eq!(
            properties[MESSAGE_TYPE], "A31",
            "this one is still readable"
        );
    }

    #[test]
    fn an_empty_authority_list_is_empty_not_two_pipes() {
        // spec/03-message-bus.md §4.1 — stated explicitly there because
        // `"||"` would match a dashboard query for the empty code.
        let no_authorities = parse(&format!(
            "MSH|^~\\&|100|100|252|252|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r{}",
            pid(&[(1, "1")])
        ));

        assert_eq!(mpi_properties(&no_authorities)[UPDATE_SOURCES], "");
    }

    #[test]
    fn the_wrapping_stops_a_substring_match() {
        // Why §4.1 exists at all: the dashboard queries these with
        // `LIKE '%|108|%'`.
        let similar = parse(&format!(
            "MSH|^~\\&|100|100|252|252|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r{}",
            pid(&[(1, "1"), (2, "1^^^1084^PI")])
        ));

        let sources = &mpi_properties(&similar)[UPDATE_SOURCES];
        assert_eq!(sources, "|1084|");
        assert!(!sources.contains("|108|"));
    }

    #[test]
    fn the_date_of_death_is_the_first_component_only() {
        // spec/03-message-bus.md §4 says PID-29.1, not PID-29. A degree of
        // precision carrying a whole DTM would break a date comparison
        // downstream.
        let message = parse(&format!(
            "MSH|^~\\&|100|100|252|252|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r{}",
            pid(&[
                (1, "1"),
                (2, "1^^^252^PI"),
                (29, "20240101120000^S"),
                (30, "Y")
            ])
        ));

        assert_eq!(mpi_properties(&message)[DATE_DEATH], "20240101120000");
    }
}
