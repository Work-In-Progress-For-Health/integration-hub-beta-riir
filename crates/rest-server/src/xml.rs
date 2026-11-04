//! Small, namespace-agnostic XML helpers shared by the content adapters.
//!
//! Implements `spec/19-rest-servers.md` §9.4.
//!
//! Only trusted, already-parsed elements are inspected here; untrusted text
//! is parsed once, by `roxmltree`, which resolves neither DTDs nor external
//! entities (§9.4.2).

use roxmltree::Node;

/// The local part of an element's name; the namespace prefix is ignored.
#[must_use]
pub fn local_name<'a>(node: Node<'a, '_>) -> &'a str {
    node.tag_name().name()
}

/// Walk a path of local element names from `parent` and return the first
/// non-blank text.
///
/// Namespace prefixes are ignored, so the same path works whichever XML
/// namespace a sender uses.
///
/// # Examples
///
/// ```
/// use rest_server::xml::find_first_text;
///
/// let document = roxmltree::Document::parse(
///     "<a:Doc xmlns:a='urn:x'><a:Header><a:Source>  252 </a:Source></a:Header></a:Doc>",
/// )?;
///
/// assert_eq!(
///     find_first_text(document.root_element(), &["Header", "Source"]).as_deref(),
///     Some("252")
/// );
/// assert_eq!(find_first_text(document.root_element(), &["Header", "Nope"]), None);
/// # Ok::<(), roxmltree::Error>(())
/// ```
#[must_use]
pub fn find_first_text<S: AsRef<str>>(parent: Node<'_, '_>, path: &[S]) -> Option<String> {
    let mut current = vec![parent];

    for segment in path {
        let next: Vec<Node<'_, '_>> = current
            .iter()
            .flat_map(roxmltree::Node::children)
            .filter(|child| child.is_element() && local_name(*child) == segment.as_ref())
            .collect();
        if next.is_empty() {
            return None;
        }
        current = next;
    }

    current
        .iter()
        .filter_map(roxmltree::Node::text)
        .map(str::trim)
        .find(|text| !text.is_empty())
        .map(ToString::to_string)
}

/// `& < > " '` escaped, for interpolation into a response.
///
/// `spec/19-rest-servers.md` §9.4.1.
///
/// # Examples
///
/// ```
/// use rest_server::xml::escape;
///
/// assert_eq!(escape(r#"<a href="x">&'"#), "&lt;a href=&quot;x&quot;&gt;&amp;&apos;");
/// ```
#[must_use]
pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// An element as standalone XML text, namespaces included.
///
/// The slice of `source` the element occupies, so the bytes are the sender's
/// own (the same reason `hl7-soap-server` slices rather than re-renders; see
/// its `payload` module) — with the namespace declarations that were in
/// scope **from an ancestor** added to the opening tag. Without them an
/// element taken out of a `<soap:Envelope xmlns:ns0=…>` would have an unbound
/// prefix, and `roxmltree` rightly refuses a document with one. Python's
/// `ElementTree.tostring` does the same hoisting.
///
/// # Examples
///
/// ```
/// use rest_server::xml::standalone;
///
/// let source = r#"<e:Envelope xmlns:e="urn:env" xmlns:ns0="urn:hl7-org:v2xml"><e:Body><ns0:ADT_A05><ns0:MSH/></ns0:ADT_A05></e:Body></e:Envelope>"#;
/// let document = roxmltree::Document::parse(source)?;
/// let payload = document.descendants().find(|n| n.has_tag_name("ADT_A05")).unwrap();
///
/// let text = standalone(source, payload);
///
/// assert!(text.starts_with("<ns0:ADT_A05"));
/// assert!(roxmltree::Document::parse(&text).is_ok(), "parses on its own: {text}");
/// # Ok::<(), roxmltree::Error>(())
/// ```
#[must_use]
pub fn standalone(source: &str, node: Node<'_, '_>) -> String {
    let slice = source.get(node.range()).unwrap_or_default();

    let inherited: Vec<(Option<&str>, &str)> = node
        .parent_element()
        .map(|parent| {
            parent
                .namespaces()
                .filter(|ns| ns.name() != Some("xml"))
                .map(|ns| (ns.name(), ns.uri()))
                .collect()
        })
        .unwrap_or_default();

    // Declared on the element itself (or overriding an ancestor's): it
    // already carries those in its own opening tag.
    let own: Vec<Option<&str>> = node
        .namespaces()
        .filter(|ns| !inherited.contains(&(ns.name(), ns.uri())))
        .map(roxmltree::Namespace::name)
        .collect();

    let declarations: String = inherited
        .iter()
        .filter(|(prefix, _)| !own.contains(prefix))
        .map(|(prefix, uri)| {
            let uri = escape_attribute(uri);
            prefix.map_or_else(
                || format!(" xmlns=\"{uri}\""),
                |prefix| format!(" xmlns:{prefix}=\"{uri}\""),
            )
        })
        .collect();

    if declarations.is_empty() {
        return slice.to_string();
    }

    // After the qualified name: `<ns0:ADT_A05` | `<ADT_A05` + declarations.
    let name_end = slice[1..]
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .map_or(slice.len(), |at| at + 1);
    format!("{}{declarations}{}", &slice[..name_end], &slice[name_end..])
}

/// A namespace URI as an attribute value.
fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_first_text_skips_blank_values_and_is_prefix_blind() {
        // test_content_adapters.py — the locator behaviour; spec/19 §9.4.
        let document = roxmltree::Document::parse(
            "<Doc><Header><Source>   </Source><Source> real </Source></Header></Doc>",
        )
        .unwrap();
        assert_eq!(
            find_first_text(document.root_element(), &["Header", "Source"]).as_deref(),
            Some("real")
        );
    }

    #[test]
    fn a_default_namespace_is_hoisted_too() {
        let source =
            r#"<Envelope xmlns="urn:default"><Body><Payload><Inner/></Payload></Body></Envelope>"#;
        let document = roxmltree::Document::parse(source).unwrap();
        let payload = document
            .descendants()
            .find(|n| n.has_tag_name("Payload"))
            .unwrap();
        let text = standalone(source, payload);
        assert_eq!(text, r#"<Payload xmlns="urn:default"><Inner/></Payload>"#);
    }

    #[test]
    fn a_prefix_the_element_declares_itself_is_not_declared_twice() {
        let source = r#"<e:Envelope xmlns:e="urn:env"><e:Body><ns0:P xmlns:ns0="urn:p"/></e:Body></e:Envelope>"#;
        let document = roxmltree::Document::parse(source).unwrap();
        let payload = document
            .descendants()
            .find(|n| n.has_tag_name("P"))
            .unwrap();
        let text = standalone(source, payload);
        assert!(roxmltree::Document::parse(&text).is_ok(), "{text}");
        assert_eq!(text.matches("xmlns:ns0").count(), 1, "{text}");
    }
}
