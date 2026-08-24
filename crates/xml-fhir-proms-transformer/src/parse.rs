//! Reading a WPAS `PromsEventRequest`.
//!
//! Implements `spec/16-proms-transformer.md` §2.5.
//!
//! # Two dialects, one index
//!
//! The specification workbook uses camelCase for the clinical event
//! messages (`nhsNumber`, `patientSurname`) and upper snake case for the
//! WPAS demographics ones (`NHS_NUMBER`, `SURNAME`). Rather than two
//! parsers, every element name is **normalised** — lowercased with
//! everything outside `[a-z0-9]` removed — so both collapse to one key
//! (§6.0).
//!
//! That is why an alias list in §6 is sometimes one lookup written twice.
//! `nhsNumber` and `NHS_NUMBER` normalise identically; `crn` and
//! `UNIT_NUMBER` do not.

use std::collections::BTreeMap;

/// Why a payload could not be read (§2.5.6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The payload was empty or whitespace.
    #[error("the PROMs payload is empty")]
    Empty,
    /// The payload was not well-formed XML.
    #[error("the PROMs payload is not well-formed XML: {0}")]
    Malformed(String),
}

/// Normalise an element name so the two dialects collapse (§6.0).
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::parse::normalise_key;
///
/// assert_eq!(normalise_key("nhsNumber"), "nhsnumber");
/// assert_eq!(normalise_key("NHS_NUMBER"), "nhsnumber");
/// assert_eq!(normalise_key("nhs-number"), "nhsnumber");
///
/// // But two different words stay different.
/// assert_ne!(normalise_key("crn"), normalise_key("UNIT_NUMBER"));
/// ```
#[must_use]
pub fn normalise_key(field_name: &str) -> String {
    field_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A read-only view over a parsed payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromsMessage {
    /// Leaf values, by normalised name.
    fields: BTreeMap<String, String>,
    /// The root element's name, with any namespace stripped.
    pub root_tag: String,
}

impl PromsMessage {
    /// The first non-empty value among these aliases (§6.0).
    ///
    /// Order matters only where the aliases normalise differently — see
    /// this module's documentation.
    #[must_use]
    pub fn get(&self, aliases: &[&str]) -> Option<&str> {
        aliases
            .iter()
            .find_map(|alias| self.fields.get(&normalise_key(alias)))
            .map(String::as_str)
    }

    /// The first non-empty value, or `""`.
    ///
    /// The Python's `get` with its empty default, for the mappers that
    /// treat absent and empty alike.
    #[must_use]
    pub fn get_or_empty(&self, aliases: &[&str]) -> &str {
        self.get(aliases).unwrap_or_default()
    }

    /// Whether any of these aliases carries a value.
    ///
    /// Note §6.0a: this cannot distinguish **absent** from **present but
    /// empty**, because §2.5.2 does not index an empty element at all.
    #[must_use]
    pub fn has(&self, aliases: &[&str]) -> bool {
        self.get(aliases).is_some()
    }

    /// How many leaves were indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Whether anything was indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// Parse a WPAS payload (§2.5).
///
/// # Errors
///
/// [`ParseError`] for an empty or malformed payload — §2.5.6 makes both a
/// failed transform rather than an empty message.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::parse::parse;
///
/// let message = parse(
///     r#"<PromsEventRequest>
///          <eventCode>REFERRAL</eventCode>
///          <nhsNumber>1234567890</nhsNumber>
///        </PromsEventRequest>"#,
/// )
/// .expect("well-formed");
///
/// assert_eq!(message.root_tag, "PromsEventRequest");
/// // Either dialect finds it.
/// assert_eq!(message.get(&["NHS_NUMBER"]), Some("1234567890"));
/// ```
pub fn parse(payload: &str) -> Result<PromsMessage, ParseError> {
    if payload.trim().is_empty() {
        return Err(ParseError::Empty);
    }

    let document =
        roxmltree::Document::parse(payload).map_err(|e| ParseError::Malformed(e.to_string()))?;

    let root = document.root_element();
    let mut fields: BTreeMap<String, String> = BTreeMap::new();

    for node in root.descendants() {
        if !node.is_element() {
            continue;
        }
        // §2.5.2 — leaves only.
        if node.children().any(|child| child.is_element()) {
            continue;
        }

        // §2.5.3 — the namespace is already stripped by `tag_name().name()`.
        let name = node.tag_name().name();
        let text = node.text().unwrap_or_default().trim();

        // §2.5.2 — a leaf with no text is skipped **entirely**, and that
        // includes its attributes (§2.5.5a). Matching the Python here
        // rather than doing the more obvious thing: it decides whether an
        // attribute on an empty element is reachable at all.
        if text.is_empty() {
            continue;
        }

        let key = normalise_key(name);

        // §2.5.4 — the first occurrence wins, and a duplicate skips the
        // whole leaf including its attributes. Overwriting silently would
        // hide a payload carrying a field twice, which the specification
        // never does and so is a problem worth keeping.
        if fields.contains_key(&key) {
            tracing::debug!(field = name, "a repeated PROMs field was ignored");
            continue;
        }
        fields.insert(key, text.to_string());

        // §2.5.5 — attributes under `{tag}_{attribute}`, first wins.
        for attribute in node.attributes() {
            fields
                .entry(normalise_key(&format!("{name}_{}", attribute.name())))
                .or_insert_with(|| attribute.value().trim().to_string());
        }
    }

    Ok(PromsMessage {
        fields,
        root_tag: root.tag_name().name().to_string(),
    })
}

/// The routing key, upper-cased (§2.5.7).
///
/// `None` when no routing field is present at all, which sends
/// `resolve_message_type` to the root-tag fallback of §2.1.1.
#[must_use]
pub fn event_code(message: &PromsMessage) -> Option<String> {
    let raw = message.get(&[
        "eventCode",
        "event_code",
        "EVENT_CODE",
        "MESSAGE_TYPE",
        "messageType",
        "MSG_TYPE",
        "msgType",
    ])?;

    let trimmed = raw.trim().to_ascii_uppercase();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_dialects_normalise_to_one_key() {
        // spec/16-proms-transformer.md §6.0 — this is why a single set of
        // mappers serves the camelCase clinical messages and the upper
        // snake case demographics ones.
        for name in ["nhsNumber", "NHS_NUMBER", "nhs-number", "Nhs Number"] {
            assert_eq!(normalise_key(name), "nhsnumber", "{name}");
        }
    }

    #[test]
    fn two_different_words_stay_different() {
        // The reason an alias list is sometimes load-bearing: `crn` and
        // `UNIT_NUMBER` are two lookups, not one written twice.
        assert_ne!(normalise_key("crn"), normalise_key("UNIT_NUMBER"));
        assert_eq!(normalise_key("UNIT_NUMBER"), "unitnumber");
        assert_eq!(normalise_key("unitNumber"), "unitnumber");
    }

    #[test]
    fn leaves_are_indexed_and_containers_are_not() {
        // §2.5.2 — a container's own text is not a field.
        let message = parse(
            r"<PromsEventRequest>
                <patient>
                  <nhsNumber>1234567890</nhsNumber>
                  <patientSurname>SURNAME</patientSurname>
                </patient>
              </PromsEventRequest>",
        )
        .unwrap();

        assert_eq!(message.get(&["nhsNumber"]), Some("1234567890"));
        assert_eq!(message.get(&["patientSurname"]), Some("SURNAME"));
        assert_eq!(message.get(&["patient"]), None, "a container is not a leaf");
        assert_eq!(message.len(), 2);
    }

    #[test]
    fn a_nested_payload_reads_the_same_as_a_flat_one() {
        // §2.5.2 indexes by name wherever the element sits, which is what
        // makes the `NESTED_REFERRAL_MESSAGE` fixture work.
        let flat = parse("<R><a>1</a><b>2</b></R>").unwrap();
        let nested = parse("<R><wrap><a>1</a></wrap><deep><er><b>2</b></er></deep></R>").unwrap();

        assert_eq!(flat.get(&["a"]), nested.get(&["a"]));
        assert_eq!(flat.get(&["b"]), nested.get(&["b"]));
    }

    #[test]
    fn an_empty_element_is_not_indexed() {
        // §2.5.2 — and §6.0a, which is the consequence: a reader cannot
        // tell an absent element from an empty one.
        let message = parse("<R><a></a><b>   </b><c>x</c></R>").unwrap();

        assert_eq!(message.get(&["a"]), None);
        assert_eq!(message.get(&["b"]), None, "whitespace only");
        assert_eq!(message.get(&["c"]), Some("x"));
        assert!(!message.has(&["a"]));
    }

    #[test]
    fn a_value_is_trimmed() {
        let message = parse("<R><a>  1234567890  </a></R>").unwrap();

        assert_eq!(message.get(&["a"]), Some("1234567890"));
    }

    #[test]
    fn the_first_occurrence_of_a_repeated_name_wins() {
        // §2.5.4 — the specification uses no repeating leaf, so a repeat
        // is a payload problem. Overwriting silently would hide it.
        let message = parse("<R><a>first</a><a>second</a></R>").unwrap();

        assert_eq!(message.get(&["a"]), Some("first"));
    }

    #[test]
    fn a_namespace_prefix_is_stripped() {
        // §2.5.3 — a namespaced payload reads the same as a bare one.
        let message = parse(
            r#"<w:PromsEventRequest xmlns:w="urn:wpas">
                 <w:nhsNumber>1234567890</w:nhsNumber>
               </w:PromsEventRequest>"#,
        )
        .unwrap();

        assert_eq!(message.root_tag, "PromsEventRequest");
        assert_eq!(message.get(&["nhsNumber"]), Some("1234567890"));
    }

    #[test]
    fn attributes_are_reachable_without_special_casing() {
        // §2.5.5.
        let message = parse(r#"<R><event code="REFERRAL">x</event></R>"#).unwrap();

        assert_eq!(message.get(&["event"]), Some("x"));
        assert_eq!(message.get(&["event_code"]), Some("REFERRAL"));
    }

    #[test]
    fn document_order_decides_between_an_element_and_an_attribute() {
        // §2.5.5 — first wins, whichever kind it is. Measured against the
        // Python rather than assumed: an earlier draft of §2.5.5 said an
        // element always beat an attribute, and it does not.
        let attribute_first = parse(
            r#"<R><event code="FROM_ATTRIBUTE">x</event><event_code>FROM_ELEMENT</event_code></R>"#,
        )
        .unwrap();
        assert_eq!(attribute_first.get(&["event_code"]), Some("FROM_ATTRIBUTE"));

        let element_first = parse(
            r#"<R><event_code>FROM_ELEMENT</event_code><event code="FROM_ATTRIBUTE">x</event></R>"#,
        )
        .unwrap();
        assert_eq!(element_first.get(&["event_code"]), Some("FROM_ELEMENT"));
    }

    #[test]
    fn an_attribute_on_a_textless_element_is_unreachable() {
        // §2.5.5a — THE subtle one, and it is the Python's behaviour: a
        // leaf with no text is skipped before its attributes are looked
        // at, so `<event code="REFERRAL"/>` indexes nothing at all.
        //
        // It matters because `resolve_message_type` describes the root-tag
        // fallback as being "for future attribute-based format" — a format
        // this parser could not read. See spec/16 §9.7.
        let message = parse(r#"<R><event code="REFERRAL"/></R>"#).unwrap();

        assert!(message.is_empty(), "{message:?}");
        assert_eq!(message.get(&["event_code"]), None);
        assert_eq!(event_code(&message), None);

        // The same element with any text at all makes it reachable.
        let with_text = parse(r#"<R><event code="REFERRAL">x</event></R>"#).unwrap();
        assert_eq!(with_text.get(&["event_code"]), Some("REFERRAL"));
    }

    #[test]
    fn a_repeated_name_also_drops_the_second_elements_attributes() {
        // §2.5.4 — the duplicate check skips the whole leaf, not just its
        // text. Matching the Python.
        let message = parse(r#"<R><a>first</a><a b="attr">second</a></R>"#).unwrap();

        assert_eq!(message.get(&["a"]), Some("first"));
        assert_eq!(
            message.get(&["a_b"]),
            None,
            "the second leaf was skipped whole"
        );
    }

    #[test]
    fn an_alias_list_returns_the_first_that_is_present() {
        // §6.0 — and the order is what the §6 tables record.
        let message = parse("<R><unitNumber>U1</unitNumber></R>").unwrap();

        assert_eq!(
            message.get(&["crn", "UNIT_NUMBER", "unitNumber"]),
            Some("U1")
        );

        let both = parse("<R><crn>C1</crn><unitNumber>U1</unitNumber></R>").unwrap();
        assert_eq!(
            both.get(&["crn", "UNIT_NUMBER", "unitNumber"]),
            Some("C1"),
            "the first alias wins when a payload carries both"
        );
    }

    #[test]
    fn an_empty_or_malformed_payload_is_an_error() {
        // §2.5.6 — a failed transform, not an empty message. An empty
        // message would route to nothing and look like an unknown event
        // code, which is a different failure to diagnose.
        assert_eq!(parse(""), Err(ParseError::Empty));
        assert_eq!(parse("   \n  "), Err(ParseError::Empty));

        assert!(matches!(
            parse("<R><unclosed>"),
            Err(ParseError::Malformed(_))
        ));
        assert!(matches!(
            parse("not xml at all"),
            Err(ParseError::Malformed(_))
        ));
    }

    #[test]
    fn the_routing_key_is_upper_cased_and_ordered() {
        // §2.5.7.
        let modern = parse("<R><eventCode>referral</eventCode></R>").unwrap();
        assert_eq!(event_code(&modern).as_deref(), Some("REFERRAL"));

        // The legacy alias, for a payload shape that predates `eventCode`.
        let legacy = parse("<R><MESSAGE_TYPE>surgery</MESSAGE_TYPE></R>").unwrap();
        assert_eq!(event_code(&legacy).as_deref(), Some("SURGERY"));

        // `eventCode` wins when both are present.
        let both =
            parse("<R><eventCode>REFERRAL</eventCode><MESSAGE_TYPE>SURGERY</MESSAGE_TYPE></R>")
                .unwrap();
        assert_eq!(event_code(&both).as_deref(), Some("REFERRAL"));
    }

    #[test]
    fn no_routing_field_at_all_is_none() {
        // §2.1.1 — which sends the caller to the root-tag fallback.
        let message = parse("<PromsEventRequest><a>1</a></PromsEventRequest>").unwrap();

        assert_eq!(event_code(&message), None);
        assert_eq!(message.root_tag, "PromsEventRequest");
    }

    #[test]
    fn a_payload_with_no_leaves_parses_to_an_empty_message() {
        // Not an error: §2.5.6 covers malformed, not vacuous. Routing then
        // fails per §2.3, which is the right diagnosis.
        let message = parse("<PromsEventRequest/>").unwrap();

        assert!(message.is_empty());
        assert_eq!(message.root_tag, "PromsEventRequest");
    }

    #[test]
    fn arbitrary_input_never_panics() {
        for payload in [
            "<R>&lt;</R>",
            "<R><a>🏥</a></R>",
            "<!-- comment only -->",
            "<?xml version=\"1.0\"?><R/>",
            &format!("<R><a>{}</a></R>", "x".repeat(100_000)),
        ] {
            let _ = parse(payload);
        }
    }
}
