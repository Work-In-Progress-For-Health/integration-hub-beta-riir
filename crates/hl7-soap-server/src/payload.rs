//! Taking the business payload back out of the request text.
//!
//! Implements the `XmlPayload` half of `spec/08-hl7-soap-server.md` §3
//! step 12, and feeds the conversion of step 10.
//!
//! # Why this is not done from the parsed tree
//!
//! `hl7-2-soap` parses the envelope into an `Element` tree, which is what
//! §3's element-name checks read. That tree has no serialiser, and writing
//! one would mean this crate deciding how to re-render attributes,
//! namespaces and empty elements — inventing an XML document that never
//! arrived, and then storing it as if it had.
//!
//! `XmlPayload` is an audit record. It should be the bytes the sender sent,
//! character for character, so that an operator comparing it against the
//! sender's own logs sees the same thing. So the payload is *located* in the
//! original text and sliced out of it, rather than rebuilt.
//!
//! # Namespace declarations
//!
//! A payload whose `xmlns` is declared on an ancestor rather than on itself
//! loses that declaration when it is sliced out. That is accepted, because
//! nothing downstream resolves it: `hl7-2-from-xml-into-er7` 0.6 matches on
//! local name (`spec/06-validation.md` §5.8.1), and the message store keeps
//! the whole SOAP request as `RawPayload` beside this, so the full context
//! is never actually lost.

/// The raw XML of the first element with this local name.
///
/// Returns the slice from its opening `<` to the end of its closing tag,
/// including both. Handles any namespace prefix, self-closing elements, and
/// elements nested inside others of the same name.
///
/// # Examples
///
/// ```
/// use hl7_soap_server::payload::extract;
///
/// let request = concat!(
///     r#"<SOAP-ENV:Envelope xmlns:SOAP-ENV="http://schemas.xmlsoap.org/soap/envelope/">"#,
///     r#"<SOAP-ENV:Body><ns0:ADT_A05 xmlns:ns0="urn:hl7-org:v2xml">"#,
///     r#"<ns0:MSH><ns0:MSH.10>M1</ns0:MSH.10></ns0:MSH>"#,
///     r#"</ns0:ADT_A05></SOAP-ENV:Body></SOAP-ENV:Envelope>"#,
/// );
///
/// let payload = extract(request, "ADT_A05").expect("the payload is there");
///
/// assert!(payload.starts_with(r#"<ns0:ADT_A05 xmlns:ns0="urn:hl7-org:v2xml">"#));
/// assert!(payload.ends_with("</ns0:ADT_A05>"));
/// assert!(!payload.contains("Envelope"), "and nothing outside it");
/// ```
///
/// An unprefixed payload works the same way:
///
/// ```
/// use hl7_soap_server::payload::extract;
///
/// let request = "<Envelope><Body><ADT_A39><MSH/></ADT_A39></Body></Envelope>";
///
/// assert_eq!(extract(request, "ADT_A39"), Some("<ADT_A39><MSH/></ADT_A39>"));
/// ```
#[must_use]
pub fn extract<'a>(request: &'a str, local_name: &str) -> Option<&'a str> {
    let (start, qualified) = find_open_tag(request, local_name, 0)?;

    // A self-closing payload — `<ADT_A05/>` — has no closing tag to find.
    let open_end = tag_end(request, start)?;
    let open_tag = request.get(start..open_end)?;
    if open_tag.trim_end_matches('>').ends_with('/') {
        return Some(open_tag);
    }

    let close = format!("</{qualified}>");
    let mut depth = 1_usize;
    let mut cursor = open_end;

    while depth > 0 {
        let next_open = find_open_tag(request, local_name, cursor)
            .filter(|(_, name)| name == &qualified)
            .map(|(at, _)| at);
        let next_close = request
            .get(cursor..)?
            .find(&close)
            .and_then(|at| at.checked_add(cursor));

        match (next_open, next_close) {
            // An element of the same name opening before the close belongs
            // to this one, so the close after it is not ours.
            (Some(open), Some(closing)) if open < closing => {
                depth = depth.checked_add(1)?;
                cursor = tag_end(request, open)?;
            }
            (_, Some(closing)) => {
                depth = depth.checked_sub(1)?;
                cursor = closing.checked_add(close.len())?;
            }
            // The document ends without closing what it opened. Malformed,
            // and `hl7-2-soap` will already have refused it; returning
            // `None` keeps that the single place the fault is decided.
            (_, None) => return None,
        }
    }

    request.get(start..cursor)
}

/// Find `<`, optional prefix, then `local_name` at a tag boundary.
///
/// Returns where the `<` is and the qualified name as written, so the
/// closing tag can be matched on the same spelling.
fn find_open_tag<'a>(request: &'a str, local_name: &str, from: usize) -> Option<(usize, &'a str)> {
    // An empty name matches everywhere and would never advance.
    if local_name.is_empty() {
        return None;
    }
    let mut cursor = from;

    while let Some(offset) = request.get(cursor..)?.find(local_name) {
        let at = cursor.checked_add(offset)?;
        cursor = at.checked_add(local_name.len())?;

        // What follows must end the name: `ADT_A05` must not match inside
        // `ADT_A05X`, and `MSH` must not match inside `MSH.10`.
        let after_ends_name = request
            .get(cursor..)?
            .chars()
            .next()
            .is_none_or(|c| c == '>' || c == '/' || c.is_whitespace());
        if !after_ends_name {
            continue;
        }

        // Walk back over an optional `prefix:` to the `<`.
        let before = request.get(..at)?;
        let name_start = match before.rfind(|c: char| c == '<' || c == '>' || c.is_whitespace()) {
            Some(delimiter) => delimiter.checked_add(1)?,
            None => 0,
        };
        let Some(open) = name_start.checked_sub(1) else {
            continue;
        };
        if before.as_bytes().get(open) != Some(&b'<') {
            continue;
        }

        let Some(qualified) = request.get(name_start..cursor) else {
            continue;
        };
        // A prefix, if present, is everything before a single colon.
        if qualified.matches(':').count() > 1 {
            continue;
        }
        if !qualified.ends_with(local_name) {
            continue;
        }

        return Some((open, qualified));
    }

    None
}

/// One past the `>` that ends the tag beginning at `start`.
fn tag_end(request: &str, start: usize) -> Option<usize> {
    let at = request.get(start..)?.find('>')?;
    start.checked_add(at)?.checked_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real LIMS request.
    const LIMS: &str = include_str!("../../../local/sample_messages/lims-to-mpi.sample.xml");

    #[test]
    fn the_real_lims_payload_comes_out_whole() {
        // spec/08-hl7-soap-server.md §3 step 12 — this is what is stored as
        // `XmlPayload`, so it is checked against the actual sample rather
        // than a synthetic envelope.
        let payload = extract(LIMS, "ADT_A05").expect("the sample carries an ADT_A05");

        assert!(payload.starts_with("<ns0:ADT_A05 "), "{}", &payload[..40]);
        assert!(payload.ends_with("</ns0:ADT_A05>"));
        assert!(!payload.contains("SOAP-ENV"), "nothing from the envelope");
        assert!(
            payload.contains("6778031837018553261z82215"),
            "MSH-10 is in it"
        );
    }

    #[test]
    fn the_extracted_payload_still_converts() {
        // The point of extracting it. If this passes, the SOAP flow works
        // end to end at the conversion boundary.
        let payload = extract(LIMS, "ADT_A05").unwrap();
        let er7 = ihb_validation::xml_to_er7(payload).expect("the real payload converts");

        assert!(er7.starts_with("MSH|^~\\&"), "{er7}");
        assert!(er7.contains("ADT^A28"), "{er7}");
    }

    #[test]
    fn every_prefix_spelling_is_read_alike() {
        // spec/08-hl7-soap-server.md §3.2 — the same envelope arrives as
        // `soapenv:`, `soap:`, `SOAP-ENV:` or unprefixed depending on the
        // sending stack.
        for prefix in ["soapenv:", "soap:", "SOAP-ENV:", ""] {
            let request = format!(
                "<{prefix}Envelope><{prefix}Body>\
                 <ADT_A05><MSH><MSH.10>M1</MSH.10></MSH></ADT_A05>\
                 </{prefix}Body></{prefix}Envelope>"
            );

            assert_eq!(
                extract(&request, "ADT_A05"),
                Some("<ADT_A05><MSH><MSH.10>M1</MSH.10></MSH></ADT_A05>"),
                "prefix {prefix:?}"
            );
        }
    }

    #[test]
    fn a_payload_prefix_may_differ_from_the_envelopes() {
        // Which is exactly what the real LIMS message does: `SOAP-ENV:` on
        // the envelope, `ns0:` on the payload.
        let request = "<S:Envelope><S:Body><p:ADT_A05 xmlns:p=\"urn:hl7-org:v2xml\">\
                       <p:MSH/></p:ADT_A05></S:Body></S:Envelope>";

        assert_eq!(
            extract(request, "ADT_A05"),
            Some("<p:ADT_A05 xmlns:p=\"urn:hl7-org:v2xml\"><p:MSH/></p:ADT_A05>")
        );
    }

    #[test]
    fn a_self_closing_payload_has_no_closing_tag_to_find() {
        assert_eq!(
            extract("<Envelope><Body><ADT_A05/></Body></Envelope>", "ADT_A05"),
            Some("<ADT_A05/>")
        );
        assert_eq!(
            extract(
                "<Envelope><Body><ns0:ADT_A05 /></Body></Envelope>",
                "ADT_A05"
            ),
            Some("<ns0:ADT_A05 />")
        );
    }

    #[test]
    fn a_name_that_is_a_prefix_of_another_is_not_matched() {
        // `MSH` must not match inside `MSH.10`, and `ADT_A05` must not
        // match inside `ADT_A05X`. Getting this wrong slices the payload at
        // the wrong place and stores a fragment.
        let request = "<Body><ADT_A05X><MSH.10>M1</MSH.10></ADT_A05X></Body>";
        assert_eq!(extract(request, "ADT_A05"), None);

        let nested = "<Body><X><MSH.10>M1</MSH.10><MSH>h</MSH></X></Body>";
        assert_eq!(extract(nested, "MSH"), Some("<MSH>h</MSH>"));
    }

    #[test]
    fn an_element_nested_inside_one_of_the_same_name_is_counted() {
        // Not a shape HL7 produces, but the depth counting is the part of
        // this that is easy to get wrong, and a wrong answer here would
        // truncate a payload silently.
        let request = "<Body><A><A>inner</A>outer</A></Body>";
        assert_eq!(extract(request, "A"), Some("<A><A>inner</A>outer</A>"));
    }

    #[test]
    fn a_missing_payload_is_none_rather_than_a_panic() {
        assert_eq!(extract("<Envelope><Body/></Envelope>", "ADT_A05"), None);
        assert_eq!(extract("", "ADT_A05"), None);
        assert_eq!(extract("<ADT_A05>never closed", "ADT_A05"), None);
    }

    #[test]
    fn arbitrary_text_never_panics() {
        // This runs on input from a public endpoint, before any of it has
        // been trusted. `hl7-2-soap` has already parsed it, but this
        // function reads the raw bytes, so it gets its own check.
        for input in [
            "<<<<>>>>",
            "</>",
            "<:>",
            "<a:b:ADT_A05>",
            "<ADT_A05",
            "ADT_A05",
            "<ADT_A05></ADT_A05",
            "< ADT_A05 >",
            "<ADT_A05/></ADT_A05>",
        ] {
            let _ = extract(input, "ADT_A05");
        }

        // And with multi-byte characters around the boundaries, which is
        // the shape that broke `ihb_telemetry::redact`.
        for input in ["<ADT_A05>🏥</ADT_A05>", "🏥<ADT_A05/>🏥", "<ADT_A05>é"] {
            let _ = extract(input, "ADT_A05");
        }
    }
}
