//! Extracting only the MSH, for logging.
//!
//! Implements `spec/10-senders.md` §7.5. Upstream:
//! `db_sender/msh_extractor.py`.
//!
//! The message goes to SQL whole; what is *logged* about it is the header and
//! nothing else, so no `PID`, `OBX` or other clinical segment can reach a log
//! line through this service. Accepts HL7 v2 XML and pipe-delimited ER7.

/// What is safe to say about a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MshInfo {
    /// The MSH segment or element, or a bracketed placeholder.
    pub msh_text: String,
    /// `MSH-10`, or `UNKNOWN`.
    pub control_id: String,
    /// `MSH-9` as `ADT^A40^ADT_A39`, or `UNKNOWN`.
    pub message_type: String,
}

/// The value used where nothing could be read.
pub const UNKNOWN: &str = "UNKNOWN";

impl MshInfo {
    /// A placeholder for a body with nothing usable in it.
    fn placeholder(text: &str) -> Self {
        Self {
            msh_text: text.to_string(),
            control_id: UNKNOWN.to_string(),
            message_type: UNKNOWN.to_string(),
        }
    }
}

/// Extract the MSH of `body` (`§7.5`).
///
/// # Examples
///
/// ```
/// use db_sender::msh::extract_msh_for_logging;
///
/// let info = extract_msh_for_logging(
///     "MSH|^~\\&|349|349|WRRS|DHCW|2026||ADT^A40^ADT_A39|MSG-1|P|2.5\rPID|1||9999999999^^^NHS",
/// );
/// assert_eq!(info.control_id, "MSG-1");
/// assert_eq!(info.message_type, "ADT^A40^ADT_A39");
/// assert!(!info.msh_text.contains("9999999999"));
///
/// assert_eq!(extract_msh_for_logging("").msh_text, "[Empty message body]");
/// ```
#[must_use]
pub fn extract_msh_for_logging(body: &str) -> MshInfo {
    let cleaned = body.trim();
    if cleaned.is_empty() {
        return MshInfo::placeholder("[Empty message body]");
    }

    if cleaned.starts_with('<') || cleaned.contains("<MSH") {
        return extract_xml(cleaned);
    }

    // `str.splitlines()` upstream: ER7 uses `\r`, but a file-sourced message
    // may use `\n` or both.
    for line in cleaned.split(['\r', '\n']) {
        let line = line.trim();
        if line.starts_with("MSH") {
            // MSH-1 is the character after the name, so it is the separator.
            let separator = line.chars().nth(3).unwrap_or('|');
            let fields: Vec<&str> = line.split(separator).collect();
            // fields[0] is "MSH" and MSH-1 is the separator itself, so
            // fields[8] is MSH-9 and fields[9] is MSH-10.
            let pick = |index: usize| {
                fields
                    .get(index)
                    .filter(|f| !f.is_empty())
                    .map_or(UNKNOWN, |f| *f)
                    .to_string()
            };
            return MshInfo {
                msh_text: line.to_string(),
                control_id: pick(9),
                message_type: pick(8),
            };
        }
    }

    MshInfo::placeholder("[No MSH segment identified]")
}

/// The XML half of [`extract_msh_for_logging`].
fn extract_xml(cleaned: &str) -> MshInfo {
    if let Ok(document) = roxmltree::Document::parse(cleaned) {
        // `tag.endswith("MSH")`: the root of a bare MSH document, or the
        // first descendant, namespaced or not.
        let found = document
            .root()
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name().ends_with("MSH"));
        if let Some(msh) = found {
            let control_id =
                text_of(msh, &["MSH.10.1", "MSH.10"]).unwrap_or_else(|| UNKNOWN.into());
            let message_type = message_type(msh).unwrap_or_else(|| UNKNOWN.into());
            return MshInfo {
                // The source slice rather than a re-serialisation: it is
                // exactly the MSH element and cannot include a sibling.
                msh_text: cleaned[msh.range()].trim().to_string(),
                control_id,
                message_type,
            };
        }
    } else if let Some(info) = salvage_partial_xml(cleaned) {
        // Upstream's regex fallback for a document that does not parse but
        // still holds a closed <MSH>..</MSH>.
        return info;
    }
    MshInfo::placeholder("[XML MSH extraction failed - structure malformed]")
}

/// `MSH-9` as `MSG.1^MSG.2^MSG.3`, or the element's own text.
fn message_type(msh: roxmltree::Node<'_, '_>) -> Option<String> {
    let parts: Vec<String> = [
        ["MSG.1", "MSH.9.1"],
        ["MSG.2", "MSH.9.2"],
        ["MSG.3", "MSH.9.3"],
    ]
    .iter()
    .filter_map(|names| text_of(msh, names))
    .collect();
    if parts.is_empty() {
        text_of(msh, &["MSH.9"])
    } else {
        Some(parts.join("^"))
    }
}

/// The first non-blank direct text of a descendant named, in order of
/// preference, one of `names` (`_extract_xml_text`).
fn text_of(root: roxmltree::Node<'_, '_>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        root.descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == *name)
            .find_map(|n| {
                n.first_child()
                    .and_then(|c| c.text())
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
            })
    })
}

/// A closed `<MSH>..</MSH>` out of a document that does not parse.
fn salvage_partial_xml(cleaned: &str) -> Option<MshInfo> {
    let lower = cleaned.to_ascii_lowercase();
    let start = lower.find("<msh")?;
    let end = start + lower[start..].find("</msh>")? + "</msh>".len();
    let raw = cleaned[start..end].trim();

    // `<MSH.10 ...>` then any nested tags, then the text.
    let control_id = raw
        .find("<MSH.10")
        .and_then(|at| raw[at..].find('>').map(|gt| at + gt + 1))
        .and_then(|mut cursor| {
            while raw[cursor..].starts_with('<') {
                cursor += raw[cursor..].find('>')? + 1;
            }
            let text = raw[cursor..].split('<').next()?.trim();
            (!text.is_empty()).then(|| text.to_string())
        })
        .unwrap_or_else(|| UNKNOWN.into());

    Some(MshInfo {
        msh_text: raw.to_string(),
        control_id,
        message_type: UNKNOWN.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_adt_a39() {
        // §7.7 (upstream test_extract_msh_xml_adt_a39).
        let xml = r#"
        <ADT_A39 xmlns="urn:hl7-org:v2xml">
            <MSH>
                <MSH.1>|</MSH.1>
                <MSH.2>^~\&amp;</MSH.2>
                <MSH.3><HD.1>349</HD.1></MSH.3>
                <MSH.9><MSG.1>ADT</MSG.1><MSG.2>A40</MSG.2><MSG.3>ADT_A39</MSG.3></MSH.9>
                <MSH.10><MSH.10.1>MSG-XML-100492</MSH.10.1></MSH.10>
            </MSH>
            <PID>
                <PID.3><CX.1>9876543210</CX.1></PID.3>
                <PID.5><XPN.1><FN.1>SECRET_PATIENT_NAME</FN.1></XPN.1></PID.5>
            </PID>
            <MRG><MRG.1><CX.1>1234567890</CX.1></MRG.1></MRG>
        </ADT_A39>"#;
        let info = extract_msh_for_logging(xml);
        assert_eq!(info.control_id, "MSG-XML-100492");
        assert_eq!(info.message_type, "ADT^A40^ADT_A39");
        assert!(info.msh_text.contains("MSH"));
        for leaked in ["SECRET_PATIENT_NAME", "9876543210", "PID", "MRG"] {
            assert!(
                !info.msh_text.contains(leaked),
                "{leaked} leaked: {}",
                info.msh_text
            );
        }
    }

    #[test]
    fn xml_oru_and_omg_without_a_namespace() {
        for (xml, id, kind, secret) in [
            (
                "<ORU_R01><MSH><MSH.9><MSG.1>ORU</MSG.1><MSG.2>R01</MSG.2><MSG.3>ORU_R01</MSG.3></MSH.9>\
                 <MSH.10><MSH.10.1>MSG-ORU-777</MSH.10.1></MSH.10></MSH>\
                 <PATIENT_RESULT><PATIENT><PID><PID.5>CONFIDENTIAL_DIAGNOSIS</PID.5></PID></PATIENT></PATIENT_RESULT></ORU_R01>",
                "MSG-ORU-777",
                "ORU^R01^ORU_R01",
                "CONFIDENTIAL_DIAGNOSIS",
            ),
            (
                "<OMG_O19><MSH><MSH.9><MSG.1>OMG</MSG.1><MSG.2>O19</MSG.2><MSG.3>OMG_O19</MSG.3></MSH.9>\
                 <MSH.10><MSH.10.1>MSG-OMG-888</MSH.10.1></MSH.10></MSH><PID><PID.5>SENSITIVE_DATA</PID.5></PID></OMG_O19>",
                "MSG-OMG-888",
                "OMG^O19^OMG_O19",
                "SENSITIVE_DATA",
            ),
        ] {
            let info = extract_msh_for_logging(xml);
            assert_eq!(info.control_id, id);
            assert_eq!(info.message_type, kind);
            assert!(!info.msh_text.contains(secret));
            assert!(!info.msh_text.contains("PID"));
        }
    }

    #[test]
    fn er7() {
        // test_extract_msh_er7.
        let er7 = "MSH|^~\\&|349|349|WRRS|DHCW|20261022143000||ADT^A40^ADT_A39|MSG-ER7-999|P|2.5.1\r\
                   PID|1||9999999999^^^NHS||SURNAME^FORENAME||19800101|M|||123 HIGH STREET\rMRG|1111111111";
        let info = extract_msh_for_logging(er7);
        assert_eq!(info.control_id, "MSG-ER7-999");
        assert_eq!(info.message_type, "ADT^A40^ADT_A39");
        assert!(info.msh_text.starts_with("MSH|^~\\&"));
        for leaked in ["SURNAME", "9999999999", "HIGH STREET"] {
            assert!(!info.msh_text.contains(leaked));
        }
    }

    #[test]
    fn empty_and_no_msh() {
        let empty = extract_msh_for_logging("   ");
        assert_eq!(empty.msh_text, "[Empty message body]");
        assert_eq!(empty.control_id, "UNKNOWN");

        let none = extract_msh_for_logging("FOO|BAR|BAZ\rQUX|1|2|3");
        assert_eq!(none.msh_text, "[No MSH segment identified]");
        assert_eq!(none.control_id, "UNKNOWN");
    }

    #[test]
    fn a_partial_xml_document_is_salvaged_without_the_rest() {
        // Upstream's regex fallback.
        let partial =
            "<ADT_A39><MSH><MSH.10><MSH.10.1>MSG-9</MSH.10.1></MSH.10></MSH><PID><oops>SECRET";
        let info = extract_msh_for_logging(partial);
        assert_eq!(info.control_id, "MSG-9");
        assert!(!info.msh_text.contains("SECRET"));
    }

    #[test]
    fn unsalvageable_xml_is_a_placeholder() {
        let info = extract_msh_for_logging("<ADT><PID>SECRET</PID>");
        assert_eq!(
            info.msh_text,
            "[XML MSH extraction failed - structure malformed]"
        );
    }
}
