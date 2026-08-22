//! Reading a request and building a reply.
//!
//! Implements `spec/11-mock-receivers.md` §3.3 to §3.7.
//!
//! # Forgiving on purpose
//!
//! §3.2: a body that is not well-formed XML is **not** an error here. A
//! mock that refuses malformed input tells you your test harness is
//! malformed, which you knew; a mock that answers anyway tells you what
//! your sender does with a reply. §1.2 is the general form of this — a mock
//! that goes silent looks like a hung endpoint.

/// SOAP 1.1's envelope namespace.
pub const SOAP_11: &str = "http://schemas.xmlsoap.org/soap/envelope/";

/// SOAP 1.2's.
pub const SOAP_12: &str = "http://www.w3.org/2003/05/soap-envelope";

/// The element names §3.4 looks for first.
pub const PAYLOAD_ELEMENTS: [&str; 6] = [
    "hl7Message",
    "HL7Message",
    "message",
    "Message",
    "payload",
    "Payload",
];

/// When no control id can be read (§3.5).
pub const UNKNOWN_CONTROL_ID: &str = "UNKNOWN";

/// Which SOAP version a request is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// SOAP 1.1.
    OneOne,
    /// SOAP 1.2.
    OneTwo,
}

impl Version {
    /// The envelope namespace to answer in (§3.3).
    #[must_use]
    pub const fn namespace(self) -> &'static str {
        match self {
            Self::OneOne => SOAP_11,
            Self::OneTwo => SOAP_12,
        }
    }

    /// The content type to answer with (§3.3).
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::OneOne => "text/xml; charset=utf-8",
            Self::OneTwo => "application/soap+xml; charset=utf-8",
        }
    }
}

/// Detect the SOAP version (§3.3).
///
/// By namespace substring in the raw body, not by parsing — which is the
/// point: a body that does not parse still gets the right reply shape.
///
/// # Examples
///
/// ```
/// use http_mock_receiver::soap::{detect_version, Version};
///
/// assert_eq!(
///     detect_version(r#"<e xmlns="http://www.w3.org/2003/05/soap-envelope"/>"#),
///     Version::OneTwo
/// );
///
/// // Anything else, including nonsense, is 1.1.
/// assert_eq!(detect_version("<Envelope/>"), Version::OneOne);
/// assert_eq!(detect_version("not xml at all"), Version::OneOne);
/// ```
#[must_use]
pub fn detect_version(body: &str) -> Version {
    if body.contains(SOAP_12) {
        Version::OneTwo
    } else {
        Version::OneOne
    }
}

/// Take the HL7 payload out of a request (§3.4).
///
/// Three steps, in order: a named element, then any descendant whose text
/// looks like an HL7 message, then nothing.
///
/// # Examples
///
/// ```
/// use http_mock_receiver::soap::extract_payload;
///
/// // Step 1 — a named element.
/// let named = "<Envelope><Body><SendHL7Message><hl7Message>MSH|^~\\&amp;|A</hl7Message>\
///              </SendHL7Message></Body></Envelope>";
/// assert_eq!(extract_payload(named).as_deref(), Some("MSH|^~\\&|A"));
///
/// // Step 2 — anything whose text starts with MSH.
/// let unnamed = "<Envelope><Body><Anything>MSH|^~\\&amp;|A</Anything></Body></Envelope>";
/// assert_eq!(extract_payload(unnamed).as_deref(), Some("MSH|^~\\&|A"));
///
/// // Step 3 — nothing to find.
/// assert_eq!(extract_payload("<Envelope><Body/></Envelope>"), None);
/// ```
#[must_use]
pub fn extract_payload(body: &str) -> Option<String> {
    // Step 1: a named element, in the order §3.4 lists them.
    for name in PAYLOAD_ELEMENTS {
        if let Some(text) = element_text(body, name)
            && !text.trim().is_empty()
        {
            return Some(unescape(text.trim()));
        }
    }

    // Step 2: any element whose trimmed text starts with `MSH`.
    let mut rest = body;
    while let Some(close) = rest.find('>') {
        let after = &rest[close + 1..];
        let end = after.find('<').unwrap_or(after.len());
        let text = after[..end].trim();
        if text.starts_with("MSH") {
            return Some(unescape(text));
        }
        rest = after;
    }

    // Step 3.
    None
}

/// The text of the first element with this local name.
fn element_text<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = body;

    while let Some(at) = rest.find(name) {
        let before = &rest[..at];
        let after = &rest[at + name.len()..];
        rest = after;

        // The name must be a whole element name: `<name>` or `<p:name>`,
        // and not `<nameOther>`.
        if !before.ends_with('<') && !before.ends_with(':') {
            continue;
        }
        let Some(close) = after.find('>') else {
            continue;
        };
        if !after[..close].trim().is_empty() && !after[..close].starts_with(' ') {
            continue;
        }

        let text = &after[close + 1..];
        let end = text.find('<')?;
        return Some(&text[..end]);
    }

    None
}

/// The five XML entities, in reverse.
///
/// `&amp;` last, so an escaped ampersand does not become the start of
/// another entity.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `MSH-10`, by splitting rather than parsing (§3.5).
///
/// Deliberately not a full parse: a mock has to answer a malformed message
/// too, and `MSH-10` is what the caller's test is asserting on.
///
/// # Examples
///
/// ```
/// use http_mock_receiver::soap::control_id;
///
/// let message = "MSH|^~\\&|252|252|100|100|20250505||ADT^A31|MSGID1|P|2.5\rPID|||1";
/// assert_eq!(control_id(message), "MSGID1");
///
/// // Too few fields, or no MSH at all.
/// assert_eq!(control_id("MSH|^~\\&|252"), "UNKNOWN");
/// assert_eq!(control_id("nonsense"), "UNKNOWN");
/// ```
#[must_use]
pub fn control_id(er7: &str) -> String {
    // §3.5 — line endings normalised first, because a sender that used
    // `\n` still has an `MSH` segment.
    let normalised = er7.replace("\r\n", "\r").replace('\n', "\r");

    normalised
        .split('\r')
        .find(|segment| segment.starts_with("MSH"))
        .and_then(|msh| msh.split('|').nth(9))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| UNKNOWN_CONTROL_ID.to_string(), ToString::to_string)
}

/// The success envelope (§3.6).
#[must_use]
pub fn success(version: Version, control_id: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <soapenv:Envelope xmlns:soapenv=\"{}\">\n  \
         <soapenv:Header/>\n  \
         <soapenv:Body>\n    \
         <AcknowledgementResponse>\n      \
         <Status>AA</Status>\n      \
         <MessageControlID>{}</MessageControlID>\n    \
         </AcknowledgementResponse>\n  \
         </soapenv:Body>\n\
         </soapenv:Envelope>",
        version.namespace(),
        escape(control_id)
    )
}

/// The fault envelope (§3.7).
///
/// In the detected version's namespace, so `soap-sender`'s evaluation
/// (`spec/10-senders.md` §4.4, row 2) rejects it — which is what a test
/// driving the unhappy path is asking for.
#[must_use]
pub fn fault(version: Version, reason: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <soapenv:Envelope xmlns:soapenv=\"{}\">\n  \
         <soapenv:Body>\n    \
         <soapenv:Fault>\n      \
         <faultcode>soapenv:Server</faultcode>\n      \
         <faultstring>{}</faultstring>\n    \
         </soapenv:Fault>\n  \
         </soapenv:Body>\n\
         </soapenv:Envelope>",
        version.namespace(),
        escape(reason)
    )
}

/// All five XML entities, for text this mock writes.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_detection_and_its_two_reply_shapes() {
        // spec/11-mock-receivers.md §3.3 and §4.5.
        assert_eq!(
            detect_version(&format!("<e xmlns=\"{SOAP_12}\"/>")),
            Version::OneTwo
        );
        assert_eq!(
            detect_version(&format!("<e xmlns=\"{SOAP_11}\"/>")),
            Version::OneOne
        );
        assert_eq!(detect_version(""), Version::OneOne, "the default");

        assert_eq!(Version::OneOne.content_type(), "text/xml; charset=utf-8");
        assert_eq!(
            Version::OneTwo.content_type(),
            "application/soap+xml; charset=utf-8"
        );
    }

    #[test]
    fn every_named_payload_element_is_found() {
        // §3.4's first step, all six names.
        for name in PAYLOAD_ELEMENTS {
            let body =
                format!("<Envelope><Body><{name}>MSH|^~\\&amp;|A</{name}></Body></Envelope>");
            assert_eq!(
                extract_payload(&body).as_deref(),
                Some("MSH|^~\\&|A"),
                "{name}"
            );
        }
    }

    #[test]
    fn the_fallback_finds_anything_that_looks_like_a_message() {
        // §3.4's second step — a sender using an element name nobody
        // anticipated still gets its message read.
        let body =
            "<Envelope><Body><SomethingElse>MSH|^~\\&amp;|A|B</SomethingElse></Body></Envelope>";
        assert_eq!(extract_payload(body).as_deref(), Some("MSH|^~\\&|A|B"));
    }

    #[test]
    fn a_named_element_that_is_blank_falls_through() {
        // §3.4 says "with non-blank text", so an empty `hl7Message` does
        // not stop the search.
        let body = "<Envelope><Body><hl7Message>   </hl7Message>\
                    <other>MSH|^~\\&amp;|A</other></Body></Envelope>";
        assert_eq!(extract_payload(body).as_deref(), Some("MSH|^~\\&|A"));
    }

    #[test]
    fn no_payload_is_none_rather_than_a_guess() {
        assert_eq!(extract_payload("<Envelope><Body/></Envelope>"), None);
        assert_eq!(extract_payload(""), None);
        assert_eq!(extract_payload("not xml"), None);
    }

    #[test]
    fn the_control_id_is_read_without_parsing() {
        // §3.5 and §4.4 — including a message with fewer than ten fields.
        let full = "MSH|^~\\&|252|252|100|100|20250505||ADT^A31|MSGID1|P|2.5\rPID|||1";
        assert_eq!(control_id(full), "MSGID1");

        assert_eq!(control_id("MSH|^~\\&|252"), UNKNOWN_CONTROL_ID);
        assert_eq!(
            control_id("MSH|^~\\&|252|252|100|100|20250505||ADT^A31||P"),
            UNKNOWN_CONTROL_ID
        );
        assert_eq!(control_id("no msh here"), UNKNOWN_CONTROL_ID);
        assert_eq!(control_id(""), UNKNOWN_CONTROL_ID);
    }

    #[test]
    fn line_endings_are_normalised_before_the_split() {
        // §3.5 — a sender using `\n` still has an `MSH` segment.
        let with_lf = "MSH|^~\\&|252|252|100|100|20250505||ADT^A31|MSGID1|P|2.5\nPID|||1";
        assert_eq!(control_id(with_lf), "MSGID1");

        let with_crlf = with_lf.replace('\n', "\r\n");
        assert_eq!(control_id(&with_crlf), "MSGID1");
    }

    #[test]
    fn the_success_envelope_carries_the_id_escaped() {
        // §3.6 — and escaped, so a control id a caller controls cannot
        // inject elements into the reply.
        let envelope = success(Version::OneOne, "M<1>&\"'");

        assert!(envelope.contains("<Status>AA</Status>"));
        assert!(
            envelope.contains("M&lt;1&gt;&amp;&quot;&apos;"),
            "{envelope}"
        );
        assert!(envelope.contains(SOAP_11));
    }

    #[test]
    fn a_fault_is_in_the_detected_namespace() {
        // §3.7 — so `soap-sender`'s §4.4 row 2 rejects it, which is what a
        // test driving the unhappy path wants.
        let eleven = fault(Version::OneOne, "no");
        assert!(eleven.contains(SOAP_11));
        assert!(eleven.contains("Fault"), "soap-sender matches on this");

        let twelve = fault(Version::OneTwo, "no");
        assert!(twelve.contains(SOAP_12));
    }

    #[test]
    fn arbitrary_input_never_panics() {
        // §3.2 — this endpoint takes whatever a test harness sends.
        for input in [
            "",
            "<",
            "<>",
            "<a",
            "&amp;",
            "🏥",
            "<Body>🏥MSH|x</Body>",
            "\u{0}",
        ] {
            let _ = detect_version(input);
            let _ = extract_payload(input);
            let _ = control_id(input);
        }
    }
}
