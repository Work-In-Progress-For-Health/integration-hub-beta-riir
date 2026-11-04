//! Rule-based classification of LIMS ADT messages.
//!
//! Implements `spec/09-transformers.md` §10.2 and §10.3. Upstream:
//! `hl7_lims_wrrs_transformer/classifier.py`.

use hl7_2::Message;

use crate::mapping::MappingTypes;

/// The assigning authority of an NHS number: the `NHS` code in a `PID-3`
/// identifier's assigning-authority component (§10.3). Matched exactly.
pub const NHS_ASSIGNING_AUTHORITY: &str = "NHS";

/// The assigning authority of a LIMS identifier (§10.3): `328`, LIMS's own
/// sending-application code. Matched exactly, never as a substring.
pub const LIMS_ASSIGNING_AUTHORITY: &str = "328";

/// The trigger event `A31` (patient information update); eligible only for
/// the LIMS/NHS ID Update rule (§10.2).
pub const TRIGGER_A31: &str = "A31";

/// The trigger event `A40` (merge); eligible for both rules (§10.2).
pub const TRIGGER_A40: &str = "A40";

/// `MSH-9.2`, trimmed (`§10.2`).
///
/// Keyed on the trigger event, not `MSH-9.3`: the structure varies
/// (`ADT_A05`, `ADT_A39`) and the REST ingress may not carry it.
///
/// # Examples
///
/// ```
/// let message = ihb_validation::parse("MSH|^~\\&|328||WRRS||2026||ADT^A31^ADT_A05|M1|P|2.5")?;
/// assert_eq!(hl7_lims_wrrs_transformer::trigger_event(&message), "A31");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn trigger_event(message: &Message) -> String {
    ihb_fields::read(message, "MSH-9.2").trim().to_string()
}

/// Every `MappingType` whose rule the message satisfies, in the stable
/// order of `§10.2`. Empty means unsupported.
///
/// # Examples
///
/// ```
/// use hl7_lims_wrrs_transformer::{MappingTypes, classify};
///
/// let a40 = ihb_validation::parse(
///     "MSH|^~\\&|328|LIMS|WRRS|DHCW|2026||ADT^A40^ADT_A39|M1|P|2.5\r\
///      PID|1||1234567^^^NHS^NH~L123^^^328^PI",
/// )?;
/// assert_eq!(
///     classify(&a40, &MappingTypes::default()),
///     ["LIMS_NHS_ID_UPDATE", "ADT_A40"]
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn classify(message: &Message, types: &MappingTypes) -> Vec<String> {
    let trigger = trigger_event(message);
    let mut matches = Vec::new();

    if (trigger == TRIGGER_A31 || trigger == TRIGGER_A40) && has_nhs_and_lims_identifiers(message) {
        matches.push(types.nhs_id_update.clone());
    }
    if trigger == TRIGGER_A40 {
        matches.push(types.a40.clone());
    }
    matches
}

/// Whether any single `PID` carries both authorities (`§10.3`).
///
/// Per `PID`, not per message: an `A40` with an NHS-only surviving patient
/// and a 328-only merged patient must not match (upstream
/// `test_identifiers_split_across_patient_groups...`).
fn has_nhs_and_lims_identifiers(message: &Message) -> bool {
    // Walking the rendered segments rather than `ihb_fields` paths: a path
    // like `PID-3` addresses the first PID only, and `ADT_A39` nests PID in
    // a patient group, where the Python's `msg.pid` shortcut came back empty
    // (the bug upstream fixed in `_find_pid_segments`). Every segment is
    // visited, wherever it sits.
    let separators = message.separators();
    let rendered = message.to_er7();

    rendered
        .split('\r')
        // `segment[3..]` is safe: the guard before it proves the first three
        // bytes are the ASCII `PID`, so index 3 is a character boundary. The
        // field-separator test stops `PIDX|...` matching.
        .filter(|segment| segment.starts_with("PID") && segment[3..].starts_with(separators.field))
        .any(|segment| {
            let codes = pid3_authority_codes(
                segment,
                separators.field,
                separators.repetition,
                separators.component,
                separators.subcomponent,
            );
            codes.iter().any(|c| c == NHS_ASSIGNING_AUTHORITY)
                && codes.iter().any(|c| c == LIMS_ASSIGNING_AUTHORITY)
        })
}

/// The assigning-authority codes of one `PID` segment's `PID-3` (`§10.3`).
///
/// For each repetition, the first subcomponent of every component after the
/// first. The REST server publishes `xml_to_er7` output, which drops empty
/// components, so `1^^^NHS^NH` arrives as `1^NHS^NH` and `CX.4` cannot be
/// relied on to be in position 4. Skipping `CX.1` keeps an identifier whose
/// *value* is `328` from counting. A code in another `CX` component (type,
/// facility) is also accepted; that is the stated trade-off, which is why
/// matching is exact and confined to one repetition.
fn pid3_authority_codes(
    segment: &str,
    field: char,
    repetition: char,
    component: char,
    subcomponent: char,
) -> Vec<String> {
    // fields[0] is "PID", so PID-3 is fields[3].
    let Some(pid3) = segment.split(field).nth(3) else {
        return Vec::new();
    };

    pid3.split(repetition)
        .flat_map(|rep| rep.split(component).skip(1))
        .map(|c| c.split(subcomponent).next().unwrap_or_default().to_string())
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::assert_is_empty,
    clippy::type_complexity,
    clippy::format_push_string,
    reason = "test fixtures favour plain asserts and tuple tables over ceremony"
)]
mod tests {
    use super::*;

    const NHS_AND_LIMS: &str = "1234567^^^NHS^NH~L123^^^328^PI";
    const LIMS_ONLY: &str = "L123^^^328^PI";
    const NHS_ONLY: &str = "1234567^^^NHS^NH";

    /// Mirrors upstream `tests/messages.py :: build_adt`.
    fn adt(trigger: &str, pid3: &str, structure: &str, mrg: bool) -> String {
        let mut segments = vec![
            format!(
                "MSH|^~\\&|328|LIMS|WRRS|DHCW|20260101120000||ADT^{trigger}^{structure}|MSG0001|P|2.5"
            ),
            "EVN||20260101120000".to_string(),
            format!("PID|1||{pid3}||SMITH^JOHN||19800101|M"),
            "PV1|1|N".to_string(),
        ];
        if mrg {
            segments.push("MRG|OLD123^^^328^PI".to_string());
        }
        segments.join("\r")
    }

    fn run(er7: &str) -> Vec<String> {
        classify(
            &ihb_validation::parse(er7).unwrap(),
            &MappingTypes::default(),
        )
    }

    #[test]
    fn a31_with_both_ids_is_nhs_id_update_only() {
        assert_eq!(
            run(&adt("A31", NHS_AND_LIMS, "ADT_A05", false)),
            ["LIMS_NHS_ID_UPDATE"]
        );
    }

    #[test]
    fn a40_with_both_ids_matches_both_rules_in_order() {
        assert_eq!(
            run(&adt("A40", NHS_AND_LIMS, "ADT_A39", true)),
            ["LIMS_NHS_ID_UPDATE", "ADT_A40"]
        );
    }

    #[test]
    fn a40_without_nhs_is_a40_only() {
        assert_eq!(run(&adt("A40", LIMS_ONLY, "ADT_A39", true)), ["ADT_A40"]);
    }

    #[test]
    fn a40_needs_no_mrg() {
        // §10.2.
        assert_eq!(run(&adt("A40", NHS_ONLY, "ADT_A39", false)), ["ADT_A40"]);
    }

    #[test]
    fn a31_missing_an_authority_is_unsupported() {
        assert!(run(&adt("A31", LIMS_ONLY, "ADT_A05", false)).is_empty());
        assert!(run(&adt("A31", NHS_ONLY, "ADT_A05", false)).is_empty());
    }

    #[test]
    fn other_triggers_are_unsupported_even_with_both_ids() {
        assert!(run(&adt("A04", NHS_AND_LIMS, "ADT_A05", false)).is_empty());
    }

    #[test]
    fn a_message_without_a_pid_is_unsupported() {
        let er7 = "MSH|^~\\&|328|LIMS|WRRS|DHCW|2026||ADT^A31^ADT_A05|M1|P|2.5\rEVN||2026";
        assert!(run(er7).is_empty());
    }

    #[test]
    fn authorities_split_across_patient_groups_do_not_match() {
        // §10.3: NHS on one PID, 328 on another.
        let er7 = [
            "MSH|^~\\&|328|LIMS|WRRS|DHCW|20260101120000||ADT^A40^ADT_A39|MSG0001|P|2.5",
            "EVN||20260101120000",
            &format!("PID|1||{NHS_ONLY}||SMITH^JOHN||19800101|M"),
            "MRG|OLD123^^^328^PI",
            &format!("PID|1||{LIMS_ONLY}||SMITH^JOHN||19800101|M"),
            "MRG|OLD456^^^328^PI",
        ]
        .join("\r");
        assert_eq!(run(&er7), ["ADT_A40"]);
    }

    #[test]
    fn custom_mapping_types_are_returned() {
        let types = MappingTypes {
            nhs_id_update: "X_NHS".into(),
            a40: "X_A40".into(),
        };
        let message = ihb_validation::parse(&adt("A40", NHS_AND_LIMS, "ADT_A39", true)).unwrap();
        assert_eq!(classify(&message, &types), ["X_NHS", "X_A40"]);
    }

    #[test]
    fn a_lower_case_trigger_is_not_a_trigger() {
        // Exact comparison, as the Python's set membership.
        assert!(run(&adt("a31", NHS_AND_LIMS, "ADT_A05", false)).is_empty());
    }

    // --- The compacted layout the REST server publishes (§10.3). -----------

    /// Builds LIMS v2 XML as upstream `tests/lims_xml.py` does and converts
    /// it with the real `xml_to_er7`, so the fixtures are in the layout the
    /// transformer actually receives.
    fn lims_er7(
        trigger: &str,
        structure: &str,
        patients: &[(&[(&str, &str)], Option<&str>)],
    ) -> String {
        fn cx(tag: &str, value: &str, authority: &str, id_type: &str) -> String {
            format!(
                "<ns0:{tag}><ns0:CX.1>{value}</ns0:CX.1><ns0:CX.2/><ns0:CX.3/>\
                 <ns0:CX.4><ns0:HD.1>{authority}</ns0:HD.1><ns0:HD.2/><ns0:HD.3/></ns0:CX.4>\
                 <ns0:CX.5>{id_type}</ns0:CX.5></ns0:{tag}>"
            )
        }
        let pid = |ids: &[(&str, &str)]| {
            let ids: String = ids
                .iter()
                .map(|(v, a)| cx("PID.3", v, a, if *a == "NHS" { "NH" } else { "PI" }))
                .collect();
            format!("<ns0:PID><ns0:PID.1>1</ns0:PID.1>{ids}<ns0:PID.8>M</ns0:PID.8></ns0:PID>")
        };
        let msh = format!(
            "<ns0:MSH><ns0:MSH.1>|</ns0:MSH.1><ns0:MSH.2>^~\\&amp;</ns0:MSH.2>\
             <ns0:MSH.3><ns0:HD.1>328</ns0:HD.1></ns0:MSH.3>\
             <ns0:MSH.9><ns0:MSG.1>ADT</ns0:MSG.1><ns0:MSG.2>{trigger}</ns0:MSG.2>\
             <ns0:MSG.3>{structure}</ns0:MSG.3></ns0:MSH.9>\
             <ns0:MSH.10>LIMS_TEST_MSG</ns0:MSH.10>\
             <ns0:MSH.11><ns0:PT.1>P</ns0:PT.1></ns0:MSH.11>\
             <ns0:MSH.12><ns0:VID.1>2.5</ns0:VID.1></ns0:MSH.12></ns0:MSH>"
        );
        let mut body = format!("{msh}<ns0:EVN><ns0:EVN.1>{trigger}</ns0:EVN.1></ns0:EVN>");
        if structure == "ADT_A39" {
            for (ids, merged) in patients {
                let mrg = merged
                    .map(|v| format!("<ns0:MRG>{}</ns0:MRG>", cx("MRG.1", v, "328", "PI")))
                    .unwrap_or_default();
                body += &format!(
                    "<ns0:ADT_A39.PATIENT>{}{mrg}</ns0:ADT_A39.PATIENT>",
                    pid(ids)
                );
            }
        } else {
            for (ids, _) in patients {
                body += &pid(ids);
            }
            body += "<ns0:PV1><ns0:PV1.1>1</ns0:PV1.1><ns0:PV1.2>N</ns0:PV1.2></ns0:PV1>";
        }
        let xml =
            format!(r#"<ns0:{structure} xmlns:ns0="urn:hl7-org:v2xml">{body}</ns0:{structure}>"#);
        compact_pid3(&ihb_validation::convert::xml_to_er7(&xml).unwrap())
    }

    /// Drop the empty components of `PID-3`, as the Python's `hl7apy`
    /// `xml_to_er7` did (`1^^^NHS^NH` becomes `1^NHS^NH`). This workspace's
    /// converter keeps them, so the compacted layout the REST server
    /// published upstream (`§10.3`) is produced here by hand.
    fn compact_pid3(er7: &str) -> String {
        er7.split('\r')
            .map(|segment| {
                if !segment.starts_with("PID|") {
                    return segment.to_string();
                }
                let mut fields: Vec<String> = segment.split('|').map(str::to_string).collect();
                fields[3] = fields[3]
                    .split('~')
                    .map(|rep| {
                        rep.split('^')
                            .filter(|c| !c.is_empty())
                            .collect::<Vec<_>>()
                            .join("^")
                    })
                    .collect::<Vec<_>>()
                    .join("~");
                fields.join("|")
            })
            .collect::<Vec<_>>()
            .join("\r")
    }

    const NHS: (&str, &str) = ("9876543210", "NHS");
    const LIMS: (&str, &str) = ("B1", "328");
    const OTHER: (&str, &str) = ("X9", "ABC");

    #[test]
    fn the_fixture_really_is_compacted() {
        // Guards the premise of this section: if the converter ever stops
        // dropping empty components these tests would pass for the wrong reason.
        let er7 = lims_er7("A31", "ADT_A05", &[(&[NHS, LIMS], None)]);
        assert!(er7.contains("9876543210^NHS^NH~B1^328^PI"), "{er7}");
    }

    #[test]
    fn compacted_a31_with_both_is_nhs_id_update() {
        let er7 = lims_er7("A31", "ADT_A05", &[(&[NHS, LIMS], None)]);
        assert_eq!(run(&er7), ["LIMS_NHS_ID_UPDATE"]);
    }

    #[test]
    fn compacted_a40_with_both_and_mrg_matches_both() {
        let er7 = lims_er7("A40", "ADT_A39", &[(&[NHS, LIMS], Some("OLD"))]);
        assert_eq!(run(&er7), ["LIMS_NHS_ID_UPDATE", "ADT_A40"]);
    }

    #[test]
    fn compacted_a40_with_lims_only_is_a40_only() {
        let er7 = lims_er7("A40", "ADT_A39", &[(&[LIMS], Some("OLD"))]);
        assert_eq!(run(&er7), ["ADT_A40"]);
    }

    #[test]
    fn compacted_a31_missing_either_is_unsupported() {
        assert!(run(&lims_er7("A31", "ADT_A05", &[(&[LIMS], None)])).is_empty());
        assert!(run(&lims_er7("A31", "ADT_A05", &[(&[NHS], None)])).is_empty());
    }

    #[test]
    fn compacted_a40_finds_both_in_the_second_patient_group() {
        let er7 = lims_er7(
            "A40",
            "ADT_A39",
            &[(&[LIMS], Some("O1")), (&[NHS, LIMS], Some("O2"))],
        );
        assert_eq!(run(&er7), ["LIMS_NHS_ID_UPDATE", "ADT_A40"]);
    }

    #[test]
    fn compacted_authorities_in_different_pids_do_not_match() {
        let er7 = lims_er7(
            "A40",
            "ADT_A39",
            &[(&[NHS], Some("O1")), (&[LIMS], Some("O2"))],
        );
        assert_eq!(run(&er7), ["ADT_A40"]);
    }

    #[test]
    fn an_identifier_value_of_328_under_another_authority_does_not_count() {
        // CX.1 is never an authority.
        let er7 = lims_er7("A31", "ADT_A05", &[(&[NHS, ("328", "ABC")], None)]);
        assert!(run(&er7).is_empty());
    }

    #[test]
    fn an_identifier_value_of_nhs_under_another_authority_does_not_count() {
        let er7 = lims_er7("A31", "ADT_A05", &[(&[("NHS", "ABC"), LIMS], None)]);
        assert!(run(&er7).is_empty());
    }

    #[test]
    fn an_unrelated_extra_repetition_is_ignored() {
        let er7 = lims_er7("A31", "ADT_A05", &[(&[NHS, OTHER, LIMS], None)]);
        assert_eq!(run(&er7), ["LIMS_NHS_ID_UPDATE"]);
    }

    #[test]
    fn codes_are_not_combined_across_repetitions() {
        // "NHS" as CX.1 of one repetition and "328" as the authority of
        // another must not satisfy the rule.
        let er7 = lims_er7(
            "A31",
            "ADT_A05",
            &[(&[("NHS", "ABC"), ("B1", "328")], None)],
        );
        assert!(run(&er7).is_empty());
    }
}
