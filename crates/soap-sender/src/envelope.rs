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
        let before = &rest[..open];
        let after = &rest[open + "Status".len()..];
        rest = after;

        // The character before must end a tag name's prefix, and the one
        // after must end the name.
        if !before.ends_with('<') && !before.ends_with(':') {
            continue;
        }
        if before.ends_with(':') && !before.trim_end_matches(|c: char| c != '<').ends_with('<') {
            continue;
        }
        let Some(close) = after.find('>') else {
            continue;
        };
        if !after[..close].trim().is_empty() {
            continue;
        }

        let text = &after[close + 1..];
        let end = text.find('<')?;
        return Some(text[..end].trim().to_string());
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
