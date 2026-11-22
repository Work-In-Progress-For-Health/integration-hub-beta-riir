//! The TRRR identifier mappings: LIMS to NHS association, and patient merge.
//!
//! Implements `spec/10-senders.md` §7.10. Upstream:
//! `senders/db_sender_trrr/db_sender_trrr/identifier_mapper.py` (`#458`).
//!
//! A pure function from a message to the parameters of one TRRR stored
//! procedure, or to a refusal, so every rule is tested without SQL Server.
//! **No identifier appears in a refusal**: each message names what was
//! missing, never a value (`§7.5`).
//!
//! This is deliberately not [`crate::ids`]. The two services read `PID-3`
//! differently, and the differences are the behaviour:
//!
//! - an authority is `CX.4.1` only; the compacted `ID^AUTH` layout that
//!   `ids` accepts is *not* an authority here;
//! - authorities compare **exactly**, `ids` ignores case;
//! - identifier text is kept as received, never trimmed, so a leading zero or
//!   a padded value reaches the procedure unchanged.
//!
//! Two shapes of message arrive, pipe-delimited ER7 and HL7 v2 XML, as for
//! `ids`.

// New module: the checked-arithmetic lints are denied from the start
// (RCA-ENF-3). Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice
    )
)]

use crate::idupdate::Rejected;

/// Dead-letter reason for every message the mapping cannot use.
pub const MAPPING_FAILED: &str = "MappingFailed";

/// The fixed WLIMS authority of the `A31` (`ADT_A05`) variant.
pub const LIMS_A05_AUTHORITY: &str = "328";

/// The authority of an NHS number.
pub const NHS_AUTHORITY: &str = "NHS";

/// What a [`crate::ProcedureType`] asks of the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapping {
    /// `A31` or `A40`: link a LIMS identifier to an NHS number.
    NhsIdUpdate,
    /// `A40`: replace a superseded identifier with the surviving one.
    Merge,
}

/// The parameters of one TRRR procedure call, all text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrrrRequest {
    /// `(LIMSIdentifier, NewIdentifier, NewIdentifierAssigningAuthority)`.
    Association {
        /// The sending LIMS's local identifier.
        lims_identifier: String,
        /// The NHS number.
        new_identifier: String,
        /// Always `NHS`.
        new_authority: String,
    },
    /// `(OldIdentifier, OldIdentifierAssigningAuthority, NewIdentifier,
    /// NewIdentifierAssigningAuthority)`.
    Merge {
        /// `MRG-1.1`, as received.
        old_identifier: String,
        /// `MRG-1.4.1`, as received.
        old_authority: String,
        /// The `PID-3` identifier of the old authority.
        new_identifier: String,
        /// The same authority.
        new_authority: String,
    },
}

/// One `PID-3` repetition: `CX.1` and `CX.4.1`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Cx {
    /// `CX.1`, as received.
    value: String,
    /// `CX.4.1`, trimmed.
    authority: String,
}

/// The fields the mapping needs, read from either format.
#[derive(Debug, Default)]
struct Parsed {
    /// `MSH-3.1`: the sender, and the LIMS authority of an `A40`.
    sending_application: String,
    /// `MSH-9.2`.
    trigger_event: String,
    /// Every `PID-3` repetition that has an identifier, in order.
    identifiers: Vec<Cx>,
    /// `None` when there is no `MRG`; blank strings when `MRG-1` is blank.
    old: Option<Cx>,
    /// How many `PID` segments the message has.
    pid_segments: usize,
    /// How many `MRG` segments the message has.
    mrg_segments: usize,
}

/// A refusal with the fixed reason.
fn refuse(description: impl Into<String>) -> Rejected {
    Rejected {
        reason: MAPPING_FAILED,
        description: description.into(),
    }
}

/// Map `body` for `mapping`.
///
/// Routing is done by the subscription, so every message that arrives is
/// mapped; one that cannot be mapped can never succeed on redelivery and is
/// refused (`§7.10`).
///
/// # Errors
///
/// [`Rejected`] with reason [`MAPPING_FAILED`] for an empty or malformed body,
/// an unsupported trigger, a second `PID` or `MRG` in a merge, or a missing
/// identifier.
///
/// # Examples
///
/// ```
/// use db_sender::trrr::{Mapping, TrrrRequest, map};
///
/// let a31 = "MSH|^~\\&|328|328|||2026||ADT^A31^ADT_A05|C1|P|2.5\rPID|1||007^^^328~9999999999^^^NHS";
/// assert_eq!(
///     map(a31, Mapping::NhsIdUpdate)?,
///     TrrrRequest::Association {
///         lims_identifier: "007".into(),
///         new_identifier: "9999999999".into(),
///         new_authority: "NHS".into(),
///     }
/// );
/// # Ok::<(), db_sender::idupdate::Rejected>(())
/// ```
pub fn map(body: &str, mapping: Mapping) -> Result<TrrrRequest, Rejected> {
    let parsed = parse(body)?;
    let trigger = parsed.trigger_event.to_uppercase();
    let unsupported = || {
        refuse(format!(
            "Unsupported trigger event: {}",
            if parsed.trigger_event.is_empty() {
                "UNKNOWN"
            } else {
                &parsed.trigger_event
            }
        ))
    };

    match (mapping, trigger.as_str()) {
        (Mapping::Merge, "A40") => merge(&parsed),
        // The sending application names the LIMS authority.
        (Mapping::NhsIdUpdate, "A40") => {
            if parsed.sending_application.is_empty() {
                return Err(refuse("Missing sending LIMS authority"));
            }
            association(&parsed, &parsed.sending_application)
        }
        (Mapping::NhsIdUpdate, "A31") => association(&parsed, LIMS_A05_AUTHORITY),
        _ => Err(unsupported()),
    }
}

/// The first identifier of `authority`, exactly as received.
fn first_for<'a>(identifiers: &'a [Cx], authority: &str) -> Option<&'a str> {
    identifiers
        .iter()
        .find(|cx| cx.authority == authority)
        .map(|cx| cx.value.as_str())
}

/// `NHS_ID_UPDATE`.
fn association(parsed: &Parsed, lims_authority: &str) -> Result<TrrrRequest, Rejected> {
    let lims = first_for(&parsed.identifiers, lims_authority)
        .ok_or_else(|| refuse("Missing LIMS identifier"))?;
    let nhs = first_for(&parsed.identifiers, NHS_AUTHORITY)
        .ok_or_else(|| refuse("Missing NHS identifier"))?;
    Ok(TrrrRequest::Association {
        lims_identifier: lims.to_string(),
        new_identifier: nhs.to_string(),
        new_authority: NHS_AUTHORITY.to_string(),
    })
}

/// `MERGE`.
fn merge(parsed: &Parsed) -> Result<TrrrRequest, Rejected> {
    if parsed.pid_segments > 1 || parsed.mrg_segments > 1 {
        return Err(refuse("Multiple patient groups are not supported"));
    }
    let old = parsed
        .old
        .as_ref()
        .filter(|old| !old.value.trim().is_empty())
        .ok_or_else(|| refuse("Missing old identifier"))?;
    if old.authority.is_empty() {
        return Err(refuse("Missing old assigning authority"));
    }
    let new = first_for(&parsed.identifiers, &old.authority)
        .ok_or_else(|| refuse("Missing new identifier"))?;
    Ok(TrrrRequest::Merge {
        old_identifier: old.value.clone(),
        old_authority: old.authority.clone(),
        new_identifier: new.to_string(),
        new_authority: old.authority.clone(),
    })
}

/// Read either format.
fn parse(body: &str) -> Result<Parsed, Rejected> {
    if body.trim().is_empty() {
        return Err(refuse("Empty message body"));
    }
    let cleaned = body.trim();
    if cleaned.starts_with('<') || cleaned.contains("<MSH") {
        parse_xml(cleaned)
    } else {
        Ok(parse_er7(cleaned))
    }
}

/// The authority of a `CX` component list: `CX.4.1`, trimmed.
fn er7_authority(components: &[&str]) -> String {
    components.get(3).map_or_else(String::new, |c| {
        c.split('&').next().unwrap_or("").trim().to_string()
    })
}

/// Read pipe-delimited ER7.
fn parse_er7(message: &str) -> Parsed {
    let mut parsed = Parsed::default();
    // Carriage-return delimited; CRLF and LF are tolerated from other producers.
    let normalised = message.replace("\r\n", "\r").replace('\n', "\r");
    // Counted, not incremented (RCA-INT-1): no arithmetic on an external figure.
    let count = |prefix: &str| {
        normalised
            .split('\r')
            .filter(|segment| segment.starts_with(prefix))
            .count()
    };
    parsed.pid_segments = count("PID");
    parsed.mrg_segments = count("MRG");
    for segment in normalised.split('\r') {
        if segment.starts_with("MSH") {
            let fields: Vec<&str> = segment.split('|').collect();
            // MSH-3 is fields[2] and MSH-9 fields[8]: MSH-1 is the separator.
            if let Some(field) = fields.get(2) {
                parsed.sending_application = field.split('^').next().unwrap_or("").trim().into();
            }
            if let Some(field) = fields.get(8)
                && let Some(trigger) = field.split('^').nth(1)
            {
                parsed.trigger_event = trigger.trim().to_string();
            }
        } else if segment.starts_with("PID") {
            let fields: Vec<&str> = segment.split('|').collect();
            let Some(pid_3) = fields.get(3) else { continue };
            for repetition in pid_3.split('~') {
                let components: Vec<&str> = repetition.split('^').collect();
                // `split` always yields a first piece; `first` keeps the
                // read free of indexing (RCA-LEN-2).
                let Some(value) = components.first().copied() else {
                    continue;
                };
                if value.trim().is_empty() {
                    continue;
                }
                parsed.identifiers.push(Cx {
                    value: value.to_string(),
                    authority: er7_authority(&components),
                });
            }
        } else if segment.starts_with("MRG") {
            if parsed.old.is_some() {
                continue;
            }
            let fields: Vec<&str> = segment.split('|').collect();
            let mut old = Cx::default();
            if let Some(mrg_1) = fields.get(1) {
                // MRG-1 may repeat; the first repetition is the superseded one.
                let components: Vec<&str> =
                    mrg_1.split('~').next().unwrap_or("").split('^').collect();
                if let Some(first) = components.first() {
                    old.value = (*first).to_string();
                }
                old.authority = er7_authority(&components);
            }
            parsed.old = Some(old);
        }
    }
    parsed
}

/// A `roxmltree` node, shortened.
type Node<'a, 'b> = roxmltree::Node<'a, 'b>;

/// An element's name without its namespace.
fn name<'a>(node: Node<'a, '_>) -> &'a str {
    node.tag_name().name()
}

/// The first child element called `wanted`.
fn child<'a, 'b>(node: Node<'a, 'b>, wanted: &str) -> Option<Node<'a, 'b>> {
    node.children()
        .find(|c| c.is_element() && name(*c) == wanted)
}

/// An element's own leading text (`ElementTree`'s `.text`).
fn leading_text<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    node.first_child()
        .filter(Node::is_text)
        .and_then(|text| text.text())
}

/// `HD.1`: a nested `<HD.1>`, else the element's own text, trimmed.
fn hd1(node: Node<'_, '_>) -> String {
    for candidate in [
        child(node, "HD.1").and_then(leading_text),
        leading_text(node),
    ] {
        if let Some(text) = candidate.map(str::trim).filter(|t| !t.is_empty()) {
            return text.to_string();
        }
    }
    String::new()
}

/// Read HL7 v2 XML, with or without a namespace.
fn parse_xml(message: &str) -> Result<Parsed, Rejected> {
    let document =
        roxmltree::Document::parse(message).map_err(|_| refuse("Malformed XML message"))?;
    let root = document.root_element();
    let mut parsed = Parsed::default();

    let msh = if name(root) == "MSH" {
        Some(root)
    } else {
        root.descendants()
            .find(|n| n.is_element() && name(*n) == "MSH")
    };
    if let Some(msh) = msh {
        if let Some(msh_3) = child(msh, "MSH.3") {
            parsed.sending_application = hd1(msh_3);
        }
        if let Some(trigger) = child(msh, "MSH.9")
            .and_then(|msh_9| child(msh_9, "MSG.2"))
            .and_then(leading_text)
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            parsed.trigger_event = trigger.to_string();
        }
    }

    let all = |wanted: &'static str| {
        root.descendants()
            .filter(move |n| n.is_element() && name(*n) == wanted)
    };
    parsed.pid_segments = all("PID").count();
    parsed.mrg_segments = all("MRG").count();

    for pid_3 in all("PID.3") {
        let value = child(pid_3, "CX.1").and_then(leading_text).unwrap_or("");
        if value.trim().is_empty() {
            continue;
        }
        parsed.identifiers.push(Cx {
            value: value.to_string(),
            authority: child(pid_3, "CX.4").map_or_else(String::new, hd1),
        });
    }

    if let Some(mrg) = all("MRG").next() {
        let mut old = Cx::default();
        if let Some(mrg_1) = child(mrg, "MRG.1") {
            old.value = child(mrg_1, "CX.1")
                .and_then(leading_text)
                .unwrap_or("")
                .to_string();
            old.authority = child(mrg_1, "CX.4").map_or_else(String::new, hd1);
        }
        parsed.old = Some(old);
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A31: &str = "MSH|^~\\&|328|328|||2026||ADT^A31^ADT_A05|C1|P|2.5\r\
                       PID|1||0000123^^^328^PI~9999999999^^^NHS^NH||SECRET^PATIENT";
    const A40: &str = "MSH|^~\\&|SENDER|SENDER|||2026||ADT^A40^ADT_A39|C2|P|2.5\r\
                       PID|1||0042^^^SENDER~9999999999^^^NHS\rMRG|0007^^^SENDER";

    fn reason(body: &str, mapping: Mapping) -> String {
        map(body, mapping).unwrap_err().description
    }

    #[test]
    fn an_a31_links_the_328_identifier_to_the_nhs_number_keeping_leading_zeroes() {
        assert_eq!(
            map(A31, Mapping::NhsIdUpdate).unwrap(),
            TrrrRequest::Association {
                lims_identifier: "0000123".into(),
                new_identifier: "9999999999".into(),
                new_authority: "NHS".into(),
            }
        );
    }

    #[test]
    fn an_a40_takes_its_lims_authority_from_msh_3() {
        assert_eq!(
            map(A40, Mapping::NhsIdUpdate).unwrap(),
            TrrrRequest::Association {
                lims_identifier: "0042".into(),
                new_identifier: "9999999999".into(),
                new_authority: "NHS".into(),
            }
        );
        // MSH-3 composite: only its first component names the authority.
        let composite = A40.replace("|SENDER|SENDER|", "|SENDER^1.2.3^ISO|SENDER|");
        assert!(map(&composite, Mapping::NhsIdUpdate).is_ok());
    }

    #[test]
    fn an_a40_with_no_sending_application_has_no_lims_authority() {
        let blank = A40.replace("|SENDER|SENDER|", "||SENDER|");
        assert_eq!(
            reason(&blank, Mapping::NhsIdUpdate),
            "Missing sending LIMS authority"
        );
    }

    #[test]
    fn a_merge_pairs_mrg_1_with_the_pid_3_of_the_same_authority() {
        assert_eq!(
            map(A40, Mapping::Merge).unwrap(),
            TrrrRequest::Merge {
                old_identifier: "0007".into(),
                old_authority: "SENDER".into(),
                new_identifier: "0042".into(),
                new_authority: "SENDER".into(),
            }
        );
    }

    #[test]
    fn an_a40_without_an_nhs_number_is_refused_for_association_but_merges() {
        let no_nhs = "MSH|^~\\&|SENDER|SENDER|||2026||ADT^A40|C|P|2.5\r\
                      PID|1||0042^^^SENDER\rMRG|0007^^^SENDER";
        assert_eq!(
            reason(no_nhs, Mapping::NhsIdUpdate),
            "Missing NHS identifier"
        );
        assert!(map(no_nhs, Mapping::Merge).is_ok());
    }

    #[test]
    fn each_instance_refuses_triggers_it_does_not_serve() {
        let a28 = A31.replace("A31", "A28");
        assert_eq!(
            reason(&a28, Mapping::NhsIdUpdate),
            "Unsupported trigger event: A28"
        );
        assert_eq!(
            reason(A31, Mapping::Merge),
            "Unsupported trigger event: A31"
        );
        assert_eq!(
            reason("MSH|^~\\&|a", Mapping::Merge),
            "Unsupported trigger event: UNKNOWN"
        );
    }

    #[test]
    fn a_merge_names_each_missing_value() {
        let msh = "MSH|^~\\&|S|S|||2026||ADT^A40|C|P|2.5\r";
        let run = |tail: &str| reason(&format!("{msh}{tail}"), Mapping::Merge);
        assert_eq!(run("PID|1||1^^^V"), "Missing old identifier");
        assert_eq!(run("PID|1||1^^^V\rMRG|"), "Missing old identifier");
        assert_eq!(
            run("PID|1||1^^^V\rMRG|2"),
            "Missing old assigning authority"
        );
        assert_eq!(run("PID|1||1^^^X\rMRG|2^^^V"), "Missing new identifier");
    }

    #[test]
    fn a_merge_refuses_more_than_one_patient() {
        let two = format!("{A40}\rPID|1||5^^^SENDER\rMRG|6^^^SENDER");
        assert_eq!(
            reason(&two, Mapping::Merge),
            "Multiple patient groups are not supported"
        );
        // An association is not a merge: several PIDs are not its concern.
        let two_pid = format!("{A31}\rPID|1||5^^^328");
        assert!(map(&two_pid, Mapping::NhsIdUpdate).is_ok());
    }

    #[test]
    fn the_compacted_layout_and_a_different_case_are_not_an_authority() {
        // `ids` would read `328` here, and `nhs` as `NHS`: TRRR does not.
        let compact = "MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\r\
                       PID|1||007^328~9999999999^^^NHS";
        assert_eq!(
            reason(compact, Mapping::NhsIdUpdate),
            "Missing LIMS identifier"
        );
        let lower = "MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\r\
                     PID|1||007^^^328~9999999999^^^nhs";
        assert_eq!(
            reason(lower, Mapping::NhsIdUpdate),
            "Missing NHS identifier"
        );
    }

    #[test]
    fn identifier_text_is_not_trimmed() {
        let padded = "MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\r\
                      PID|1|| 007 ^^^328~9999999999^^^NHS";
        let TrrrRequest::Association {
            lims_identifier, ..
        } = map(padded, Mapping::NhsIdUpdate).unwrap()
        else {
            panic!("expected an association");
        };
        assert_eq!(lims_identifier, " 007 ");
    }

    #[test]
    fn line_endings_do_not_matter() {
        let lf = A40.replace('\r', "\n");
        let crlf = A40.replace('\r', "\r\n");
        assert_eq!(
            map(&lf, Mapping::Merge).unwrap(),
            map(&crlf, Mapping::Merge).unwrap()
        );
    }

    const XML_A40: &str = "<ADT_A39 xmlns=\"urn:hl7-org:v2xml\"><MSH>\
        <MSH.3><HD.1>SENDER</HD.1></MSH.3>\
        <MSH.9><MSG.1>ADT</MSG.1><MSG.2>A40</MSG.2></MSH.9></MSH>\
        <PATIENT><PID>\
        <PID.3><CX.1>0042</CX.1><CX.4><HD.1>SENDER</HD.1></CX.4></PID.3>\
        <PID.3><CX.1>9999999999</CX.1><CX.4><HD.1>NHS</HD.1></CX.4></PID.3>\
        </PID><MRG><MRG.1><CX.1>0007</CX.1><CX.4><HD.1>SENDER</HD.1></CX.4></MRG.1></MRG>\
        </PATIENT></ADT_A39>";

    #[test]
    fn xml_is_read_with_the_same_rules() {
        assert_eq!(
            map(XML_A40, Mapping::Merge).unwrap(),
            TrrrRequest::Merge {
                old_identifier: "0007".into(),
                old_authority: "SENDER".into(),
                new_identifier: "0042".into(),
                new_authority: "SENDER".into(),
            }
        );
        assert_eq!(
            map(XML_A40, Mapping::NhsIdUpdate).unwrap(),
            TrrrRequest::Association {
                lims_identifier: "0042".into(),
                new_identifier: "9999999999".into(),
                new_authority: "NHS".into(),
            }
        );
    }

    #[test]
    fn an_empty_or_broken_body_is_refused_not_guessed_at() {
        assert_eq!(reason("  ", Mapping::Merge), "Empty message body");
        assert_eq!(
            reason("<MSH><oops", Mapping::Merge),
            "Malformed XML message"
        );
    }

    #[test]
    fn no_refusal_contains_an_identifier() {
        for body in [A31, A40, XML_A40] {
            for mapping in [Mapping::NhsIdUpdate, Mapping::Merge] {
                let broken = body.replace("NHS", "XXX").replace("MRG", "ZZZ");
                if let Err(rejected) = map(&broken, mapping) {
                    for secret in ["0007", "0042", "0000123", "9999999999", "SECRET"] {
                        assert!(
                            !rejected.description.contains(secret),
                            "{}",
                            rejected.description
                        );
                    }
                }
            }
        }
    }
}
