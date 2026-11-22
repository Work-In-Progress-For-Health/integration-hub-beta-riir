//! Reading patient identifiers out of an HL7 message, for the Queue ID Update.
//!
//! Implements `spec/10-senders.md` §7.8. Upstream:
//! `db_sender/parsers/hl7_id_extractor.py` (`#455`).
//!
//! Unlike [`crate::msh`], this reads `PID` and `MRG`, which is the point: the
//! identifiers are the parameters of `dbo.prQueueIdUpdate`. **Nothing here
//! logs a value it reads**, and callers must not either (`§7.5`).
//!
//! Two shapes of message arrive: pipe-delimited ER7 and HL7 v2 XML. Both are
//! handled, and a CX field is read in two layouts, because the estate
//! produces both:
//!
//! - standard, `ID^^^AUTH^TYPE`: the authority is `CX.4`, component 4;
//! - compacted, `ID^AUTH` or `ID^AUTH^TYPE`: the authority is component 2.

/// A parsed `CX` repetition: the identifier and its assigning authority.
type Cx = (Option<String>, Option<String>);

/// `MRG-1`: the identifier being merged away and its assigning authority.
///
/// The first repetition that has either is returned, as upstream.
///
/// # Examples
///
/// ```
/// use db_sender::ids::mrg_identifiers;
///
/// let (id, authority) = mrg_identifiers("MSH|^~\\&|a\rMRG|1111111111^^^NHS^NH");
/// assert_eq!(id.as_deref(), Some("1111111111"));
/// assert_eq!(authority.as_deref(), Some("NHS"));
/// assert_eq!(mrg_identifiers("MSH|^~\\&|a"), (None, None));
/// ```
#[must_use]
pub fn mrg_identifiers(body: &str) -> Cx {
    let cleaned = body.trim();
    if cleaned.is_empty() {
        return (None, None);
    }
    if cleaned.starts_with('<') || cleaned.contains("<MRG") {
        mrg_from_xml(cleaned)
    } else {
        mrg_from_er7(cleaned)
    }
}

/// The identifier in `PID-3` whose assigning authority is `authority`
/// (compared case-insensitively), or `None`.
///
/// # Examples
///
/// ```
/// use db_sender::ids::pid_identifier_by_authority;
///
/// let message = "MSH|^~\\&|a\rPID|1||111^^^328~9999999999^^^NHS||SURNAME";
/// assert_eq!(pid_identifier_by_authority(message, "NHS").as_deref(), Some("9999999999"));
/// assert_eq!(pid_identifier_by_authority(message, "nhs").as_deref(), Some("9999999999"));
/// assert_eq!(pid_identifier_by_authority(message, "999"), None);
/// ```
#[must_use]
pub fn pid_identifier_by_authority(body: &str, authority: &str) -> Option<String> {
    let cleaned = body.trim();
    if cleaned.is_empty() || authority.trim().is_empty() {
        return None;
    }
    if cleaned.starts_with('<') || cleaned.contains("<PID") {
        pid_from_xml(cleaned, authority)
    } else {
        pid_from_er7(cleaned, authority)
    }
}

/// How many `segment` (`"MRG"`, `"PID"`) segments the message has, in XML or
/// ER7 (`count_segments`).
///
/// `mrg_identifiers` reads only the first `MRG` but the PID search covers
/// every `PID`, so a multi-patient message could pair one patient's old
/// identifier with another's new one; callers use this to refuse it
/// (`spec/10-senders.md` §7.9).
///
/// # Examples
///
/// ```
/// use db_sender::ids::count_segments;
///
/// assert_eq!(count_segments("MSH|^~\\&|a\rPID|1\rPID|2", "PID"), 2);
/// assert_eq!(count_segments("<A><PID/><x:PID xmlns:x='u'/></A>", "PID"), 2);
/// assert_eq!(count_segments("  ", "PID"), 0);
/// ```
#[must_use]
pub fn count_segments(body: &str, segment: &str) -> usize {
    let cleaned = body.trim();
    if cleaned.is_empty() {
        return 0;
    }
    if cleaned.starts_with('<') {
        // Not well-formed XML is invalid data, so it is not guessed at (the
        // Python's text-scan fallback is not ported; module doc, `§7.8`;
        // RCA-DECL-6). It counts as none, the identifier readers find
        // nothing in it either, so `plan_update_identifier` dead-letters it
        // as missing its identifiers.
        return roxmltree::Document::parse(cleaned).map_or(0, |document| {
            document
                .descendants()
                .filter(|node| node.is_element() && local(*node) == segment)
                .count()
        });
    }
    lines(cleaned)
        .filter(|line| {
            line.strip_prefix(segment)
                .is_some_and(|rest| rest.starts_with('|'))
        })
        .count()
}

/// Whether two authorities are the same (`_authority_matches`).
fn authority_matches(actual: &str, expected: &str) -> bool {
    actual.trim().eq_ignore_ascii_case(expected.trim())
}

/// A non-blank trimmed string.
fn nonblank(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// One repetition of a CX field, in either layout.
fn parse_cx(repetition: &str) -> Cx {
    let components: Vec<&str> = repetition.split('^').collect();
    let id = components.first().copied().and_then(nonblank);
    let authority_of = |component: &str| nonblank(component.split('&').next().unwrap_or(""));
    let authority = if components.len() > 3 {
        components.get(3).and_then(|c| authority_of(c))
    } else if matches!(components.len(), 2 | 3) {
        components.get(1).and_then(|c| authority_of(c))
    } else {
        None
    };
    (id, authority)
}

/// The lines of an ER7 message, as `str.splitlines()` sees them.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\r', '\n']).map(str::trim)
}

/// The fields of a segment line, split on the character after its name.
fn fields(line: &str) -> Vec<&str> {
    let separator = line.chars().nth(3).unwrap_or('|');
    line.split(separator).collect()
}

/// [`mrg_identifiers`] for ER7.
fn mrg_from_er7(cleaned: &str) -> Cx {
    for line in lines(cleaned) {
        if !line.starts_with("MRG") {
            continue;
        }
        let fields = fields(line);
        let Some(mrg_1) = fields.get(1).and_then(|f| nonblank(f)) else {
            return (None, None);
        };
        for repetition in mrg_1.split('~') {
            let Some(repetition) = nonblank(repetition) else {
                continue;
            };
            let (id, authority) = parse_cx(&repetition);
            if id.is_some() || authority.is_some() {
                return (id, authority);
            }
        }
        return (None, None);
    }
    (None, None)
}

/// [`pid_identifier_by_authority`] for ER7.
fn pid_from_er7(cleaned: &str, authority: &str) -> Option<String> {
    for line in lines(cleaned) {
        if !line.starts_with("PID") {
            continue;
        }
        let fields = fields(line);
        let Some(pid_3) = fields.get(3).and_then(|f| nonblank(f)) else {
            continue;
        };
        for repetition in pid_3.split('~') {
            let Some(repetition) = nonblank(repetition) else {
                continue;
            };
            let (id, found) = parse_cx(&repetition);
            if let (Some(id), Some(found)) = (id, found)
                && authority_matches(&found, authority)
            {
                return Some(id);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// XML
// ---------------------------------------------------------------------------
//
// Upstream falls back to regular expressions when the document does not
// parse. This port does not: a body that is not well-formed XML yields no
// identifiers, so the message is dead-lettered as missing one (`§7.8`). The
// producers here (`rest-server`, `hl7-soap-server`) emit ER7.

/// A `roxmltree` node, shortened.
type Node<'a, 'b> = roxmltree::Node<'a, 'b>;

/// An element's name without its namespace.
fn local<'a>(node: Node<'a, '_>) -> &'a str {
    node.tag_name().name()
}

/// The element's own leading text, as `ElementTree`'s `.text`.
fn own_text(node: Node<'_, '_>) -> Option<String> {
    node.first_child()
        .filter(roxmltree::Node::is_text)
        .and_then(|child| child.text())
        .and_then(nonblank)
}

/// The identifier and authority inside one `CX` (or `PID.3` / `MRG.1`)
/// element: the first `CX.1`-like text, and the first `HD.1`-like text under
/// a `CX.4`-like element.
fn cx_from_xml(
    root: Node<'_, '_>,
    id_tags: &[&str],
    authority_tags: &[&str],
    hd_tags: &[&str],
) -> Cx {
    let mut id = None;
    let mut authority = None;
    for node in root.descendants().filter(Node::is_element) {
        let tag = local(node);
        if id_tags.contains(&tag) {
            if id.is_none() {
                id = own_text(node);
            }
        } else if authority_tags.contains(&tag) && authority.is_none() {
            authority = node
                .descendants()
                .filter(Node::is_element)
                .find_map(|hd| hd_tags.contains(&local(hd)).then(|| own_text(hd)).flatten());
        }
    }
    (id, authority)
}

/// [`mrg_identifiers`] for XML.
fn mrg_from_xml(cleaned: &str) -> Cx {
    let Ok(document) = roxmltree::Document::parse(cleaned) else {
        return (None, None);
    };
    let Some(mrg) = document
        .descendants()
        .find(|n| n.is_element() && local(*n) == "MRG")
    else {
        return (None, None);
    };

    let mut repetitions: Vec<Node<'_, '_>> = mrg
        .children()
        .filter(|c| c.is_element() && local(*c) == "MRG.1")
        .collect();
    if repetitions.is_empty() {
        repetitions.push(mrg);
    }

    for repetition in repetitions {
        let (id, authority) = cx_from_xml(
            repetition,
            &["CX.1", "MRG.1.1"],
            &["CX.4", "MRG.1.4"],
            &["HD.1", "MRG.1.4.1", "CX.4.1"],
        );
        if id.is_some() || authority.is_some() {
            return (id, authority);
        }
    }
    (None, None)
}

/// [`pid_identifier_by_authority`] for XML.
fn pid_from_xml(cleaned: &str, authority: &str) -> Option<String> {
    let document = roxmltree::Document::parse(cleaned).ok()?;
    for pid in document
        .descendants()
        .filter(|n| n.is_element() && local(*n) == "PID")
    {
        for pid_3 in pid
            .children()
            .filter(|c| c.is_element() && local(*c) == "PID.3")
        {
            let (id, found) = cx_from_xml(
                pid_3,
                &["CX.1", "PID.3.1"],
                &["CX.4", "PID.3.4"],
                &["HD.1", "PID.3.4.1", "CX.4.1"],
            );
            if let (Some(id), Some(found)) = (id, found)
                && authority_matches(&found, authority)
            {
                return Some(id);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cx_is_read_in_the_standard_and_the_compacted_layout() {
        // spec/10-senders.md §7.8.
        assert_eq!(
            parse_cx("123^^^328^PI"),
            (Some("123".into()), Some("328".into()))
        );
        assert_eq!(
            parse_cx("123^328"),
            (Some("123".into()), Some("328".into()))
        );
        assert_eq!(
            parse_cx("123^328^PI"),
            (Some("123".into()), Some("328".into()))
        );
        // A hierarchic designator: only its first component.
        assert_eq!(
            parse_cx("1^^^NHS&x&y^NH"),
            (Some("1".into()), Some("NHS".into()))
        );
        assert_eq!(parse_cx("123"), (Some("123".into()), None));
        assert_eq!(parse_cx("^^^328"), (None, Some("328".into())));
    }

    #[test]
    fn pid_3_repetitions_are_searched_in_order_and_the_authority_is_case_insensitive() {
        let message = "MSH|^~\\&|a\rPID|1||111^^^328~222^^^NHS~333^^^nhs";
        assert_eq!(
            pid_identifier_by_authority(message, "328").as_deref(),
            Some("111")
        );
        assert_eq!(
            pid_identifier_by_authority(message, "NHS").as_deref(),
            Some("222")
        );
        assert_eq!(pid_identifier_by_authority(message, "999"), None);
        assert_eq!(pid_identifier_by_authority(message, ""), None);
        assert_eq!(pid_identifier_by_authority("", "NHS"), None);
    }

    #[test]
    fn a_pid_with_no_pid_3_or_an_id_without_an_authority_matches_nothing() {
        assert_eq!(
            pid_identifier_by_authority("MSH|^~\\&|a\rPID|1", "NHS"),
            None
        );
        assert_eq!(
            pid_identifier_by_authority("MSH|^~\\&|a\rPID|1||999", "NHS"),
            None
        );
    }

    #[test]
    fn mrg_1_gives_the_first_repetition_with_something_in_it() {
        let (id, authority) = mrg_identifiers("MSH|^~\\&|a\rMRG|~1111^^^NHS^NH~2222^^^NHS");
        assert_eq!(
            (id.as_deref(), authority.as_deref()),
            (Some("1111"), Some("NHS"))
        );
        // The compacted layout.
        let (id, authority) = mrg_identifiers("MSH|^~\\&|a\rMRG|1111^NHS");
        assert_eq!(
            (id.as_deref(), authority.as_deref()),
            (Some("1111"), Some("NHS"))
        );
        // An id with no authority still counts as found.
        let (id, authority) = mrg_identifiers("MSH|^~\\&|a\rMRG|1111");
        assert_eq!((id.as_deref(), authority.as_deref()), (Some("1111"), None));
        assert_eq!(mrg_identifiers("MSH|^~\\&|a\rMRG|"), (None, None));
        assert_eq!(mrg_identifiers("MSH|^~\\&|a\rPID|1"), (None, None));
        assert_eq!(mrg_identifiers("  "), (None, None));
    }

    #[test]
    fn line_endings_do_not_matter() {
        let message = "MSH|^~\\&|a\r\nPID|1||9^^^NHS\r\nMRG|8^^^NHS";
        assert_eq!(
            pid_identifier_by_authority(message, "NHS").as_deref(),
            Some("9")
        );
        assert_eq!(mrg_identifiers(message).0.as_deref(), Some("8"));
    }

    const XML_A40: &str = "<ADT_A39 xmlns=\"urn:hl7-org:v2xml\"><MSH/><PATIENT>\
        <PID><PID.3><CX.1>9999999999</CX.1><CX.4><HD.1>NHS</HD.1></CX.4></PID.3>\
        <PID.3><CX.1>555</CX.1><CX.4><HD.1>328</HD.1></CX.4></PID.3></PID>\
        <MRG><MRG.1><CX.1>1111111111</CX.1><CX.4><HD.1>NHS</HD.1></CX.4></MRG.1></MRG></PATIENT></ADT_A39>";

    #[test]
    fn xml_is_read_with_or_without_a_namespace_and_through_nesting() {
        assert_eq!(
            pid_identifier_by_authority(XML_A40, "328").as_deref(),
            Some("555")
        );
        assert_eq!(
            pid_identifier_by_authority(XML_A40, "NHS").as_deref(),
            Some("9999999999")
        );
        let (id, authority) = mrg_identifiers(XML_A40);
        assert_eq!(
            (id.as_deref(), authority.as_deref()),
            (Some("1111111111"), Some("NHS"))
        );

        let plain = XML_A40.replace(" xmlns=\"urn:hl7-org:v2xml\"", "");
        assert_eq!(
            pid_identifier_by_authority(&plain, "328").as_deref(),
            Some("555")
        );
    }

    #[test]
    fn xml_that_does_not_parse_yields_nothing_rather_than_a_guess() {
        // Deviation from upstream's regex fallback (module doc).
        let broken = "<ADT><PID><PID.3><CX.1>1</CX.1><CX.4><HD.1>NHS</HD.1></CX.4></PID.3>";
        assert_eq!(pid_identifier_by_authority(broken, "NHS"), None);
        assert_eq!(mrg_identifiers("<ADT><MRG><MRG.1>"), (None, None));
    }

    #[test]
    fn segments_are_counted_in_er7_with_any_line_ending() {
        let er7 = "MSH|^~\\&|a\rPID|1\r\nPID|2\nMRG|3\rPIDX|4";
        assert_eq!(count_segments(er7, "PID"), 2, "PIDX is not a PID");
        assert_eq!(count_segments(er7, "MRG"), 1);
        assert_eq!(count_segments("", "PID"), 0);
    }

    #[test]
    fn segments_are_counted_in_xml_with_or_without_a_namespace() {
        let xml = "<A xmlns:n='u'><PID/><n:PID><PID.3/></n:PID><MRG/></A>";
        assert_eq!(count_segments(xml, "PID"), 2, "PID.3 is not a PID");
        assert_eq!(count_segments(xml, "MRG"), 1);
    }

    #[test]
    fn broken_xml_is_not_guessed_at() {
        // Deviation from upstream's regex fallback (module doc): the message
        // is refused later as missing its identifiers, not counted here.
        let broken = "<A><PID><x/></PID><PID/><oops";
        assert_eq!(count_segments(broken, "PID"), 0);
        assert_eq!(count_segments("<A><MRG>", "MRG"), 0);
    }
}
