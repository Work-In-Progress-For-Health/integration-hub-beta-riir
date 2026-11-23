//! Wrapping a message, and reading what came back.
//!
//! Implements `spec/10-senders.md` §4.2 and §4.4.

use ihb_sender::Verdict;

/// The `Status` values that mean the receiver took the message.
///
/// The same two as MLLP's `MSA-1` (`spec/10-senders.md` §3.7), because it
/// is the same acknowledgement carried over a different transport.
pub const ACCEPTED: [&str; 2] = ["AA", "CA"];

/// HTTP statuses that are not, in themselves, a failure.
pub const OK_STATUSES: [u16; 2] = [200, 202];

/// Wrap an ER7 message in the SOAP envelope §4.2 specifies.
///
/// # The escaping is exactly three characters
///
/// `&`, `<` and `>` — and **not** `"` or `'`. That is unusual: most XML
/// escapers do all five, and doing so here would be *more* correct XML and
/// the wrong answer. The receiving endpoint is the Python's counterpart,
/// which unescapes the same three; a `"` arriving as `&quot;` would reach
/// MPI as the literal six characters.
///
/// HL7 payloads carry apostrophes constantly — `O'BRIEN` — so this is not a
/// theoretical concern.
///
/// # Examples
///
/// ```
/// use soap_sender::envelope::wrap;
///
/// let envelope = wrap("MSH|^~\\&|252&x|<test>");
///
/// assert!(envelope.contains("&amp;x"), "{envelope}");
/// assert!(envelope.contains("&lt;test&gt;"), "{envelope}");
/// ```
///
/// Quotes and apostrophes pass through as themselves:
///
/// ```
/// use soap_sender::envelope::wrap;
///
/// let envelope = wrap("PID|||1||O'BRIEN^\"J\"");
///
/// assert!(envelope.contains("O'BRIEN"), "{envelope}");
/// assert!(envelope.contains("\"J\""), "{envelope}");
/// assert!(!envelope.contains("&apos;"), "{envelope}");
/// assert!(!envelope.contains("&quot;"), "{envelope}");
/// ```
#[must_use]
pub fn wrap(er7: &str) -> String {
    let escaped = escape(er7);

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\">\n  \
         <soapenv:Header/>\n  \
         <soapenv:Body>\n    \
         <SendHL7Message>\n      \
         <hl7Message>{escaped}</hl7Message>\n    \
         </SendHL7Message>\n  \
         </soapenv:Body>\n\
         </soapenv:Envelope>"
    )
}

/// Which envelope a message is wrapped in (`spec/10-senders.md` §4.6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// §4.2: the ER7 as text in `SendHL7Message`.
    #[default]
    Default,
    /// §4.6.2: HL7 v2 XML in WIS's `CaptureFromFiorona`.
    Wis,
}

impl Format {
    /// Read `SOAP_ENVELOPE_FORMAT`.
    ///
    /// `wis` after trim and lower-casing; **anything else is the default**
    /// (§4.6.1), because that is what the Python does and a deployment's
    /// existing value must keep meaning what it meant.
    ///
    /// # Examples
    ///
    /// ```
    /// use soap_sender::envelope::Format;
    ///
    /// assert_eq!(Format::parse(" WIS "), Format::Wis);
    /// assert_eq!(Format::parse("default"), Format::Default);
    /// assert_eq!(Format::parse("anything else"), Format::Default);
    /// ```
    #[must_use]
    pub fn parse(value: &str) -> Self {
        if value.trim().eq_ignore_ascii_case("wis") {
            Self::Wis
        } else {
            Self::Default
        }
    }

    /// Wrap `er7` in this format's envelope.
    ///
    /// # Errors
    ///
    /// The structural reason, for [`Format::Wis`] when the message cannot be
    /// converted to v2 XML (§4.6.3).
    pub fn wrap(self, er7: &str) -> Result<String, String> {
        match self {
            Self::Default => Ok(wrap(er7)),
            Self::Wis => wrap_wis(er7),
        }
    }
}

/// The WIS namespaces (`spec/10-senders.md` §4.6.2).
const WIS_ENVELOPE_NS: &str = "http://Cypris.Nhs.Wales.Uk/CaptureFromFiorona/Input";
/// The namespace of the `CaptureFromFiorona` operation.
const WIS_SERVICE_NS: &str = "http://Cypris.Nhs.Wales.Uk/";

/// Wrap an ER7 message, converted to v2 XML, in the WIS envelope.
///
/// # Errors
///
/// A structural reason when the conversion fails (§4.6.3).
///
/// # Examples
///
/// ```
/// use soap_sender::envelope::wrap_wis;
///
/// let envelope = wrap_wis("MSH|^~\\&|328||100||20260729095037||ADT^A28^ADT_A05|M1|P|2.5\rPID|1")?;
///
/// assert!(envelope.contains("<ns3:inputString>"));
/// assert!(envelope.contains("urn:hl7-org:v2xml"));
/// # Ok::<(), String>(())
/// ```
pub fn wrap_wis(er7: &str) -> Result<String, String> {
    let xml = ihb_validation::convert::er7_to_xml(er7)
        .map_err(|error| format!("could not convert the message to HL7 v2 XML for WIS: {error}"))?;

    // §4.6.2 — four characters here, three in §4.2.
    let escaped = escape(&xml).replace('"', "&quot;");

    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <ns1:Envelope xmlns:ns1=\"{WIS_ENVELOPE_NS}\">\n  \
         <ns1:Body>\n    \
         <ns3:CaptureFromFiorona xmlns:ns3=\"{WIS_SERVICE_NS}\">\n      \
         <ns3:inputString>{escaped}</ns3:inputString>\n    \
         </ns3:CaptureFromFiorona>\n  \
         </ns1:Body>\n\
         </ns1:Envelope>"
    ))
}

/// The three characters §4.2 escapes, and only those.
///
/// `&` first, or the ampersands introduced by the other two would be
/// escaped again.
#[must_use]
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Evaluate a SOAP response.
///
/// Implements every row of `spec/10-senders.md` §4.4's table, in order.
///
/// # Examples
///
/// ```
/// use soap_sender::envelope::evaluate;
///
/// // A status element saying yes.
/// assert!(evaluate(200, "<Envelope><Status>AA</Status></Envelope>").is_accepted());
///
/// // A minimal 200 with nothing in it — §4.4.1, and deliberate.
/// assert!(evaluate(200, "").is_accepted());
///
/// // A fault, whatever the status.
/// assert!(!evaluate(200, "<Envelope><Fault/></Envelope>").is_accepted());
///
/// // Any other status.
/// assert!(!evaluate(500, "<Status>AA</Status>").is_accepted());
/// ```
#[must_use]
pub fn evaluate(status: u16, body: &str) -> Verdict {
    // Row 1.
    if !OK_STATUSES.contains(&status) {
        return Verdict::Rejected {
            reason: format!("the endpoint answered HTTP {status}"),
            acknowledgement: truncated(body, FAULT_LOG_LIMIT),
        };
    }

    // Row 2 — checked **before** parsing, and independent of status. §4.4.1
    // explains why it has to exist: without it, a fault body with no
    // `Status` element would fall through to the last row and be read as
    // success.
    if body.contains("Fault") {
        return Verdict::Rejected {
            reason: "the response carries a SOAP fault".to_string(),
            acknowledgement: truncated(body, FAULT_LOG_LIMIT),
        };
    }

    // Rows 3, 4 and 5.
    match status_element(body) {
        Some(value) if ACCEPTED.contains(&value.as_str()) => Verdict::Accepted {
            acknowledgement: value,
        },
        Some(value) => Verdict::Rejected {
            reason: format!("the endpoint answered Status {value}"),
            acknowledgement: value,
        },
        // Row 6 — a minimal 200 with an empty body is a success. §4.4.1
        // records that as deliberate: some endpoints answer this way, and
        // the fault check above is what stops it swallowing a real failure.
        None => Verdict::Accepted {
            acknowledgement: format!("HTTP {status}"),
        },
    }
}

/// How much of a faulted body is logged (§4.4.2).
pub const FAULT_LOG_LIMIT: usize = 500;

/// How much of an unparseable body is logged (§4.4.2).
pub const UNPARSEABLE_LOG_LIMIT: usize = 200;

/// The text of the first `Status` element, by local name.
///
/// Matched on local name so a prefixed `<tns:Status>` reads alike, as
/// everywhere else in this workspace
/// (`spec/08-hl7-soap-server.md` §3.2).
fn status_element(body: &str) -> Option<String> {
    let mut rest = body;

    while let Some(open) = rest.find("Status") {
        let Some((before, from_name)) = rest.split_at_checked(open) else {
            break;
        };
        let Some(after) = from_name.strip_prefix("Status") else {
            break;
        };
        rest = after;

        // The character before must end a tag name's prefix, and the one
        // after must end the name.
        if !before.ends_with('<') && !before.ends_with(':') {
            continue;
        }
        if before.ends_with(':') && !before.trim_end_matches(|c: char| c != '<').ends_with('<') {
            continue;
        }
        let Some((attributes, text)) = after.split_once('>') else {
            continue;
        };
        if !attributes.trim().is_empty() {
            continue;
        }

        return text
            .split_once('<')
            .map(|(text, _)| text.trim().to_string());
    }

    None
}

/// At most `limit` characters, on a character boundary.
///
/// §4.4.2 — a response body can echo message content, so what is logged is
/// bounded and passes through redaction at the event logger.
fn truncated(body: &str, limit: usize) -> String {
    body.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Python's `_WDS_A28_ER7` fixture shape.
    const WDS_A28: &str = "MSH|^~\\&|328||100||20260729095037||ADT^A28^ADT_A05|6778031837018553261|P|2.5|||AL|NE||UTF-8\rEVN|A28|20260729095037\rPID|1||B0000010612^^^328^PI|||Testpatient^Test^^^||20010909|M\rPD1|||^UNK|UNK\rPV1||N";

    #[test]
    fn the_wis_envelope_names_the_operation_and_its_input() {
        // spec/10-senders.md §4.6.2.
        let envelope = wrap_wis(WDS_A28).unwrap();

        assert!(envelope.contains("CaptureFromFiorona"));
        assert!(envelope.contains("<ns3:inputString>"));
        assert!(envelope.contains("http://Cypris.Nhs.Wales.Uk/CaptureFromFiorona/Input"));
        assert!(!envelope.contains("SendHL7Message"));
    }

    #[test]
    fn the_wis_payload_is_escaped_v2_xml() {
        // §4.6.2 — the namespace and the structure, escaped, not raw.
        let envelope = wrap_wis(WDS_A28).unwrap();

        assert!(envelope.contains("urn:hl7-org:v2xml"));
        assert!(envelope.contains("ADT_A05"));
        assert!(
            envelope.contains("&lt;ADT_A05"),
            "the XML is text, not markup"
        );
        assert!(envelope.contains("&quot;"), "attribute quotes are escaped");
        assert!(!envelope.contains("<ADT_A05"));
    }

    #[test]
    fn the_wis_envelope_is_well_formed_xml() {
        // §4.6.4.
        let envelope = wrap_wis(WDS_A28).unwrap();
        assert!(roxmltree::Document::parse(&envelope).is_ok(), "{envelope}");
    }

    #[test]
    fn the_wis_envelope_escapes_quotes_unlike_the_default() {
        // §4.6.2 against §4.2.
        assert!(!wrap("PID|\"x\"").contains("&quot;"));
        assert!(wrap_wis(WDS_A28).unwrap().contains("&quot;"));
    }

    #[test]
    fn an_unconvertible_message_is_an_error_not_an_envelope() {
        // §4.6.3.
        let error = wrap_wis("this is not HL7").unwrap_err();
        assert!(error.contains("could not convert"), "{error}");
        assert!(
            !error.contains("this is not HL7"),
            "no content in an error: {error}"
        );
    }

    #[test]
    fn the_format_is_wis_only_for_wis() {
        // §4.6.1.
        assert_eq!(Format::parse("wis"), Format::Wis);
        assert_eq!(Format::parse("  WIS\n"), Format::Wis);
        assert_eq!(Format::parse(""), Format::Default);
        assert_eq!(Format::parse("wiss"), Format::Default);
        assert_eq!(Format::default(), Format::Default);
    }

    #[test]
    fn the_default_format_is_unchanged() {
        // §4.6.4.
        assert_eq!(Format::Default.wrap("MSH|x").unwrap(), wrap("MSH|x"));
        assert_eq!(
            Format::Wis.wrap(WDS_A28).unwrap(),
            wrap_wis(WDS_A28).unwrap()
        );
    }

    #[test]
    fn the_envelope_is_the_one_section_four_two_specifies() {
        let envelope = wrap("MSH|^~\\&|252");

        assert!(envelope.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(envelope.contains(
            "<soapenv:Envelope xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\">"
        ));
        assert!(envelope.contains("<soapenv:Header/>"));
        assert!(envelope.contains("<SendHL7Message>"));
        assert!(envelope.contains("<hl7Message>MSH|^~\\&amp;|252</hl7Message>"));
        assert!(envelope.ends_with("</soapenv:Envelope>"));
    }

    #[test]
    fn exactly_three_characters_are_escaped() {
        // spec/10-senders.md §4.2 and §6.5 — and the "crucially" in §6.5 is
        // about the two that are not.
        assert_eq!(escape("a&b"), "a&amp;b");
        assert_eq!(escape("a<b"), "a&lt;b");
        assert_eq!(escape("a>b"), "a&gt;b");

        assert_eq!(escape("O'BRIEN"), "O'BRIEN", "apostrophes pass through");
        assert_eq!(escape("\"J\""), "\"J\"", "so do quotes");
    }

    #[test]
    fn the_ampersand_is_escaped_first() {
        // Otherwise `<` becomes `&lt;` and its ampersand is escaped again,
        // yielding `&amp;lt;` — and the receiver sees the literal text.
        assert_eq!(escape("<&>"), "&lt;&amp;&gt;");
        assert_eq!(escape("&lt;"), "&amp;lt;");
    }

    #[test]
    fn every_row_of_the_response_table() {
        // spec/10-senders.md §4.4 and §6.4.

        // Row 1: not 200 or 202.
        for status in [400, 401, 404, 500, 503] {
            assert!(
                !evaluate(status, "<Status>AA</Status>").is_accepted(),
                "{status}"
            );
        }
        for status in OK_STATUSES {
            assert!(
                evaluate(status, "<Status>AA</Status>").is_accepted(),
                "{status}"
            );
        }

        // Row 2: a fault, before parsing and whatever the status.
        assert!(
            !evaluate(
                200,
                "<soapenv:Fault><faultcode>Server</faultcode></soapenv:Fault>"
            )
            .is_accepted()
        );
        assert!(!evaluate(202, "anything mentioning Fault").is_accepted());

        // Rows 3 and 4: the status element.
        for code in ACCEPTED {
            assert!(
                evaluate(200, &format!("<Status>{code}</Status>")).is_accepted(),
                "{code}"
            );
        }
        for code in ["AE", "AR", "XX", ""] {
            assert!(
                !evaluate(200, &format!("<Status>{code}</Status>")).is_accepted(),
                "{code:?}"
            );
        }

        // Row 6: no fault, no status element.
        assert!(evaluate(200, "").is_accepted());
        assert!(evaluate(202, "<Envelope><Body/></Envelope>").is_accepted());
    }

    #[test]
    fn the_fault_check_precedes_the_status_element() {
        // §4.4.1 — the reason row 2 exists at all. A fault body with no
        // `Status` element would otherwise fall to row 6 and be read as
        // success, completing a message the endpoint refused.
        let fault =
            "<Envelope><Body><Fault><faultstring>no</faultstring></Fault></Body></Envelope>";

        assert!(!evaluate(200, fault).is_accepted());
    }

    #[test]
    fn a_prefixed_status_element_is_read_alike() {
        // As everywhere else in this workspace, matching is on local name.
        assert!(evaluate(200, "<tns:Status>AA</tns:Status>").is_accepted());
        assert!(!evaluate(200, "<tns:Status>AE</tns:Status>").is_accepted());
    }

    #[test]
    fn a_name_that_merely_contains_status_is_not_one() {
        // `<OverallStatusCode>` is not `<Status>`, and reading it as one
        // would take a value from the wrong element.
        assert!(
            evaluate(200, "<MessageStatusCode>AE</MessageStatusCode>").is_accepted(),
            "no Status element, so §4.4's last row applies"
        );
    }

    #[test]
    fn a_logged_body_is_bounded() {
        // §4.4.2 — a response can echo message content.
        let long = "x".repeat(10_000);
        let verdict = evaluate(500, &long);

        assert_eq!(verdict.acknowledgement().len(), FAULT_LOG_LIMIT);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // The shape that broke `ihb_telemetry::redact`: a fixed offset into
        // text that is not ASCII.
        let body = "🏥".repeat(1000);
        let verdict = evaluate(500, &body);

        assert_eq!(verdict.acknowledgement().chars().count(), FAULT_LOG_LIMIT);
    }
}
