//! Between ER7 and the v2.xml representation.
//!
//! Implements `spec/06-validation.md` §4 and §5.
//!
//! Two directions, and they are not symmetrical in importance:
//!
//! - **ER7 → v2.xml** is what the message store keeps alongside the raw
//!   payload, and what the flow schema validates. Every ingress message goes
//!   through it.
//! - **v2.xml → ER7** is the SOAP ingress path: LIMS sends XML and the rest
//!   of the pipeline speaks ER7.
//!
//! The conversion itself is `hl7-2-from-er7-into-xml` and its sibling. What
//! this module adds is the **flow dictionary** and **schema shape**, which
//! together are what make the output validate against the XSDs the
//! dictionary was built from.

use hl7_2_from_er7_into_xml::Options as XmlOptions;

use crate::error::ValidationError;
use crate::flow::Flow;

/// Convert ER7 to v2.xml against a flow's schema.
///
/// Schema shape is **on**: the dictionary decides which elements appear, so
/// a field the schema requires is written even when the message leaves it
/// empty, a field the schema does not mark as repeating keeps its `~` as
/// ordinary text, and nothing undeclared is written at all
/// (`spec/06-validation.md` §4.3, §4.5).
///
/// That is the difference between a document that happens to describe the
/// message and one that validates against the schema.
///
/// `override_structure` names the root element when the message did not
/// carry `MSH-9.3`; see
/// [`structure::resolve`](crate::structure::resolve).
///
/// # Errors
///
/// [`ValidationError::Xml`] when the message has no usable `MSH` header.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ihb_validation::{Flow, convert::er7_to_xml_for_flow};
///
/// let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
///            EVN||20250502092900\r\
///            PID|||8888888^^^252^PI||SURNAME^FORENAME\r\
///            PV1||U";
///
/// let xml = er7_to_xml_for_flow(er7, Flow::Phw, None)?;
///
/// assert!(xml.contains("urn:hl7-org:v2xml"));
/// assert!(xml.contains("<ADT_A05"));
/// // Components are named for their HL7 data type.
/// assert!(xml.contains("<CX.1>8888888</CX.1>"));
/// # Ok(())
/// # }
/// ```
pub fn er7_to_xml_for_flow(
    er7: &str,
    flow: Flow,
    override_structure: Option<&str>,
) -> Result<String, ValidationError> {
    let options = XmlOptions {
        flat: false,
        schema_shape: true,
    };

    let text = with_structure(er7, override_structure);

    hl7_2_from_er7_into_xml::convert_with_dictionary(&text, &flow.dictionary(), options)
        .map_err(ValidationError::xml)
}

/// Convert ER7 to v2.xml without a flow schema.
///
/// The message store's fallback, for flows with no schema group — `mpi` —
/// and for senders, which store what they forward without re-validating it
/// (`spec/06-validation.md` §4.8, `spec/07-hl7-server.md` §3.5).
///
/// Schema shape is **off**: with no schema to be shaped by, the message
/// decides what appears. No required-field padding, no repetition gating,
/// no segment insertion.
///
/// # Errors
///
/// [`ValidationError::Xml`] when the message has no usable `MSH` header.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ihb_validation::convert::er7_to_xml;
///
/// let xml = er7_to_xml("MSH|^~\\&|100|100|200|200|20260218||ADT^A31^ADT_A05|M1|P|2.5")?;
/// assert!(xml.contains("<ADT_A05"));
/// # Ok(())
/// # }
/// ```
pub fn er7_to_xml(er7: &str) -> Result<String, ValidationError> {
    hl7_2_from_er7_into_xml::convert(er7).map_err(ValidationError::xml)
}

/// Convert a v2.xml document back to ER7.
///
/// Group elements are flattened, so grouped and flat input produce the same
/// message (`spec/06-validation.md` §5.1).
///
/// # Errors
///
/// [`ValidationError::Xml`] when the document cannot be read.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ihb_validation::convert::xml_to_er7;
///
/// let xml = r#"<ADT_A05 xmlns="urn:hl7-org:v2xml">
///   <MSH><MSH.1>|</MSH.1><MSH.2>^~\&amp;</MSH.2>
///        <MSH.9><MSG.1>ADT</MSG.1><MSG.2>A31</MSG.2></MSH.9></MSH>
///   <PID><PID.5><XPN.1><FN.1>SURNAME</FN.1></XPN.1></PID.5></PID>
/// </ADT_A05>"#;
///
/// let er7 = xml_to_er7(xml)?;
/// assert!(er7.starts_with("MSH|^~\\&"));
/// assert!(er7.contains("SURNAME"));
/// # Ok(())
/// # }
/// ```
///
/// # Namespace prefixes
///
/// Prefixed documents (`<ns0:MSH>`) convert as readily as ones using a
/// default namespace. That is worth stating because it was not always true:
/// `hl7-2-from-xml-into-er7` 0.5 matched element names verbatim and failed
/// a prefixed document with `MissingMsh`, which this module worked around
/// by rewriting every tag. 0.6 matches on local name and the workaround is
/// gone. `spec/06-validation.md` §5.8.1 has the history; the test below is
/// what keeps it fixed, since the real LIMS payload is `ns0:`-prefixed and
/// a regression upstream would take the whole SOAP flow down.
///
pub fn xml_to_er7(xml: &str) -> Result<String, ValidationError> {
    let normalised = drop_empty_elements_repeatedly(xml);
    hl7_2_from_xml_into_er7::convert(&normalised).map_err(ValidationError::xml)
}

/// Remove elements that carry nothing: `<HD.2/>` and `<HD.2></HD.2>`.
///
/// **A workaround, not a design**, and the second of two in this module.
///
/// `hl7-2-from-xml-into-er7` renders an *empty* element as HL7's explicit
/// null, `""` — the same as an element whose text genuinely is `""`. The two
/// mean opposite things: an explicit null says "delete this value", and an
/// empty element says "nothing here". Conflating them turns a padded
/// document into ER7 full of deletion markers:
///
/// ```text
/// <HD.2/>            ->  ""     wrong: nothing becomes a deletion
/// <HD.2>""</HD.2>    ->  ""     right
/// (element absent)   ->  (none) right
/// ```
///
/// That matters because the real LIMS payload is padded — it was produced by
/// the schema-shaped conversion, which writes every declared position — so
/// without this, every SOAP message would reach MPI carrying `""` in most of
/// its components.
///
/// Dropping the empty ones is safe because the converter takes each
/// component's **position from its element name**, not from its order among
/// its siblings: `HD.3` lands in component 3 whether or not `HD.2` is
/// present. That is also precisely why the Python's converter corrupts these
/// documents and this one does not — see `spec/06-validation.md` §5.9.
///
/// See `spec/06-validation.md` §5.8.
fn drop_empty_elements_repeatedly(xml: &str) -> String {
    // One pass removes only elements that were already empty. A container
    // whose children were all empty — `<VID.2><CWE.1/><CWE.2/></VID.2>` —
    // becomes empty *because* of that pass, so it takes another to remove
    // it. The schema-shaped documents this exists for nest three deep, so
    // the loop runs to a fixed point rather than a guessed number of times.
    //
    // It terminates: every pass either removes at least one element or
    // changes nothing, and the document is finite.
    let mut current = xml.to_string();
    loop {
        let next = drop_empty_elements(&current);
        if next == current {
            return current;
        }
        current = next;
    }
}

/// One pass of [`drop_empty_elements_repeatedly`].
fn drop_empty_elements(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;

    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];

        let Some(close) = rest.find('>') else {
            out.push_str(rest);
            return out;
        };

        let tag = &rest[..=close];
        let after = &rest[close + 1..];

        // `<X/>`: nothing in it by construction.
        if tag.ends_with("/>") && !tag.starts_with("<?") && !tag.starts_with("<!") {
            rest = after;
            continue;
        }

        // `<X></X>`: an open tag followed immediately by its close.
        if let Some(name) = element_name(tag) {
            let closing = format!("</{name}>");
            if after.starts_with(&closing) {
                rest = &after[closing.len()..];
                continue;
            }
        }

        out.push_str(tag);
        rest = after;
    }

    out.push_str(rest);
    out
}

/// The element name of an opening tag, or `None` for anything else.
fn element_name(tag: &str) -> Option<&str> {
    let inner = tag.strip_prefix('<')?.strip_suffix('>')?;
    if inner.starts_with('?') || inner.starts_with('!') || inner.starts_with('/') {
        return None;
    }
    inner.split_whitespace().next().filter(|n| !n.is_empty())
}

/// Write a structure identifier into `MSH-9.3` when the message lacks one.
///
/// The conversion names its root element from `MSH-9`, so a message that did
/// not say what structure it is would otherwise get a root named for its
/// trigger — `<ADT_A28>` rather than `<ADT_A05>` — and validate against
/// nothing.
///
/// Done as a text edit on a parsed copy rather than by asking the converter
/// to take a root name, because the converter derives grouping from the same
/// place and the two must agree.
fn with_structure(er7: &str, override_structure: Option<&str>) -> String {
    let Some(structure) = override_structure else {
        return er7.to_string();
    };

    let Ok(mut message) = hl7_2::parse(er7) else {
        // Unparseable input is the caller's problem, reported by the
        // conversion itself with a better message than anything here.
        return er7.to_string();
    };

    if message.set("MSH-9.3", structure).is_err() {
        return er7.to_string();
    }

    message.to_er7()
}

#[cfg(test)]
mod tests {

    /// `spec/06-validation.md` §5.8.1 — was a defect, is now a guarantee.
    ///
    /// The real LIMS payload is `ns0:`-prefixed. `hl7-2-from-xml-into-er7`
    /// 0.5 could not read it and this module rewrote every tag to cope; 0.6
    /// reads it directly. This test is what stands between an upstream
    /// regression and a silently dead SOAP flow, so it asserts on the
    /// prefixed form specifically rather than on conversion in general.
    #[test]
    fn a_namespace_prefixed_document_needs_no_help() {
        let xml = concat!(
            r#"<?xml version="1.0"?>"#,
            r#"<ns0:ADT_A05 xmlns:ns0="urn:hl7-org:v2xml">"#,
            r#"<ns0:MSH><ns0:MSH.1>|</ns0:MSH.1><ns0:MSH.2>^~\&amp;</ns0:MSH.2>"#,
            r#"<ns0:MSH.9><ns0:MSG.1>ADT</ns0:MSG.1><ns0:MSG.2>A31</ns0:MSG.2></ns0:MSH.9>"#,
            r#"<ns0:MSH.10>M1</ns0:MSH.10>"#,
            r#"<ns0:MSH.12><ns0:VID.1>2.5</ns0:VID.1></ns0:MSH.12></ns0:MSH>"#,
            r#"<ns0:PID><ns0:PID.5><ns0:XPN.1><ns0:FN.1>SURNAME</ns0:FN.1>"#,
            r#"</ns0:XPN.1></ns0:PID.5></ns0:PID></ns0:ADT_A05>"#,
        );

        let er7 = xml_to_er7(xml).expect("a prefixed document converts");

        assert!(er7.starts_with("MSH|^~\\&"), "{er7}");
        assert!(er7.contains("ADT^A31"), "{er7}");
        assert!(er7.contains("SURNAME"), "{er7}");
    }
    use super::*;

    /// A ChemoCare message: `ADT^A28` with no `MSH-9.3`, which is the case
    /// `with_structure` exists for.
    const CHEMO: &str = concat!(
        "MSH|^~\\&|224|224|100|100|20250624165855||ADT^A28|M1|P|2.4\r",
        "EVN|A28|20250624165855\r",
        "PID|1||8888888||SURNAME^FORENAME||19990101|M\r",
        "PV1||N"
    );

    /// A PHW message that states its structure.
    const PHW: &str = concat!(
        "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r",
        "EVN||20250502092900\r",
        "PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r",
        "PV1||U"
    );

    #[test]
    fn the_document_is_in_the_hl7_namespace() {
        // spec/06-validation.md §4.1
        let xml = er7_to_xml_for_flow(PHW, Flow::Phw, None).unwrap();
        assert!(xml.contains("urn:hl7-org:v2xml"));
    }

    #[test]
    fn the_root_element_is_the_structure() {
        // spec/06-validation.md §4.1
        let xml = er7_to_xml_for_flow(PHW, Flow::Phw, None).unwrap();
        assert!(xml.contains("<ADT_A05"), "{xml}");
    }

    #[test]
    fn components_are_named_for_their_data_type() {
        // spec/06-validation.md §4.2
        let xml = er7_to_xml_for_flow(PHW, Flow::Phw, None).unwrap();
        assert!(xml.contains("<CX.1>8888888</CX.1>"), "{xml}");
        assert!(xml.contains("<FN.1>SURNAME</FN.1>"), "{xml}");
    }

    #[test]
    fn an_inferred_structure_names_the_root() {
        // spec/06-validation.md §2.2 and §4.1 together. Without this, a
        // ChemoCare ADT^A28 would produce <ADT_A28> and validate against
        // nothing.
        let xml = er7_to_xml_for_flow(CHEMO, Flow::Chemo, Some("ADT_A05")).unwrap();
        assert!(xml.contains("<ADT_A05"), "{xml}");
        assert!(!xml.contains("<ADT_A28"), "{xml}");
    }

    #[test]
    fn without_an_override_the_message_names_its_own_root() {
        let xml = er7_to_xml_for_flow(PHW, Flow::Phw, None).unwrap();
        assert!(xml.contains("<ADT_A05"), "{xml}");
    }

    #[test]
    fn the_schema_unaware_conversion_still_works() {
        // spec/06-validation.md §4.8 — the message store's fallback.
        let xml = er7_to_xml(PHW).unwrap();
        assert!(xml.contains("<ADT_A05"), "{xml}");
        assert!(xml.contains("SURNAME"), "{xml}");
    }

    #[test]
    fn an_unreadable_message_is_an_error_rather_than_a_panic() {
        assert!(er7_to_xml("").is_err());
        assert!(er7_to_xml_for_flow("", Flow::Phw, None).is_err());
    }

    #[test]
    fn xml_converts_back_to_er7() {
        // spec/06-validation.md §5
        let xml = er7_to_xml_for_flow(PHW, Flow::Phw, None).unwrap();
        let er7 = xml_to_er7(&xml).unwrap();

        assert!(er7.starts_with("MSH|^~\\&"), "{er7}");
        assert!(er7.contains("SURNAME"), "{er7}");
    }

    #[test]
    fn the_round_trip_keeps_the_fields_that_matter() {
        // spec/06-validation.md §7.4, in miniature: the full corpus
        // round-trip is the integration test.
        let xml = er7_to_xml_for_flow(PHW, Flow::Phw, None).unwrap();
        let back = xml_to_er7(&xml).unwrap();
        let reparsed = hl7_2::parse(&back).unwrap();

        assert_eq!(reparsed.get("MSH-10").unwrap().as_deref(), Some("M1"));
        assert_eq!(reparsed.get("PID-5.1").unwrap().as_deref(), Some("SURNAME"));
        assert_eq!(reparsed.get("PID-7").unwrap().as_deref(), Some("19990101"));
    }

    /// Upstream `c8bb7c0` (#458), `test_xml_to_er7.py`: the positional
    /// components of a CX survive the way back, the assigning authority in
    /// `CX.4` above all. Here the converter is the `hl7-2` crate's, so these
    /// pin it rather than port Python's hand-written one. The Rust converter
    /// wants a whole message, so each case goes ER7, XML, ER7.
    fn pid_after_round_trip(pid: &str) -> String {
        let er7 = format!(
            "MSH|^~\\&|328|328|200|200|20250101120000||ADT^A31^ADT_A05|C1|P|2.5\rEVN||2025\r{pid}\rPV1||U"
        );
        let back = xml_to_er7(&er7_to_xml(&er7).unwrap()).unwrap();
        back.split('\r')
            .find(|line| line.starts_with("PID"))
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn internal_empty_components_are_kept_and_trailing_ones_trimmed() {
        let pid = pid_after_round_trip("PID|1||B0000010612^^^328^PI");
        assert!(pid.contains("B0000010612^^^328^PI"), "{pid}");

        let pid = pid_after_round_trip("PID|1||A^B^^");
        assert!(pid.ends_with("A^B"), "{pid}");
    }

    #[test]
    fn subcomponents_are_joined_with_an_ampersand() {
        let pid = pid_after_round_trip("PID|1||B0000010612^^^328&X^PI");
        assert!(pid.contains("B0000010612^^^328&X^PI"), "{pid}");
    }

    #[test]
    fn the_assigning_authority_survives_a_round_trip() {
        let er7 = "MSH|^~\\&|328|328|200|200|20250101120000||ADT^A31^ADT_A05|CTRL1|P|2.5|||||GBR||EN\r\
                   EVN||20250101120000\r\
                   PID|1||B0000010612^^^328^PI~9876543210^^^NHS^NH||Lims2val^Test||20010909|M\r\
                   PV1||U";
        let back = xml_to_er7(&er7_to_xml_for_flow(er7, Flow::Phw, None).unwrap()).unwrap();
        assert!(back.contains("B0000010612^^^328^PI"), "{back}");
        assert!(back.contains("9876543210^^^NHS^NH"), "{back}");

        let back = xml_to_er7(&er7_to_xml(er7).unwrap()).unwrap();
        assert!(back.contains("B0000010612^^^328^PI"), "{back}");
    }

    #[test]
    fn an_unreadable_document_is_an_error() {
        assert!(xml_to_er7("not xml").is_err());
    }

    #[test]
    fn writing_a_structure_into_a_message_that_has_none() {
        let written = with_structure(CHEMO, Some("ADT_A05"));
        let message = hl7_2::parse(&written).unwrap();
        assert_eq!(message.get("MSH-9.3").unwrap().as_deref(), Some("ADT_A05"));
        // The rest of the header is untouched.
        assert_eq!(message.get("MSH-9.1").unwrap().as_deref(), Some("ADT"));
        assert_eq!(message.get("MSH-9.2").unwrap().as_deref(), Some("A28"));
    }

    #[test]
    fn no_override_leaves_the_message_exactly_as_it_was() {
        assert_eq!(with_structure(CHEMO, None), CHEMO);
    }

    #[test]
    fn an_unparseable_message_passes_through_the_structure_rewrite() {
        // The conversion reports a better failure than this helper could.
        assert_eq!(with_structure("rubbish", Some("ADT_A05")), "rubbish");
    }
}
