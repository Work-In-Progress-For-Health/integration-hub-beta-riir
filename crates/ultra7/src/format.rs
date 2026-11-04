//! Format detection and pretty-printing.
//!
//! Implements `spec/ultra7-test-message-sender.md` `§5`.

use std::fmt::Write as _;

use roxmltree::{Document, Node, NodeType};

use crate::model::MessageFormat;

/// Best-effort format of raw text: JSON, then XML, else HL7 (`§5.1`).
///
/// ER7 has no reliable leading marker once the field separator varies, so
/// HL7 is the fallthrough rather than a positive test. A body that starts
/// `{` or `<` but does not parse is therefore reported as HL7, not as a
/// broken JSON or XML message: the sender should still put it on the wire,
/// because sending malformed input is a legitimate test.
///
/// # Examples
/// ```
/// use ultra7::{format::detect_format, model::MessageFormat};
/// assert_eq!(detect_format(r#"{"a": 1}"#), MessageFormat::Json);
/// assert_eq!(detect_format("<a><b/></a>"), MessageFormat::Xml);
/// assert_eq!(detect_format("<a>"), MessageFormat::Hl7); // unparseable
/// ```
#[must_use]
pub fn detect_format(content: &str) -> MessageFormat {
    let stripped = content.trim();
    if stripped.starts_with(['{', '['])
        && serde_json::from_str::<serde_json::Value>(stripped).is_ok()
    {
        return MessageFormat::Json;
    }
    if stripped.starts_with('<') && Document::parse(stripped).is_ok() {
        return MessageFormat::Xml;
    }
    MessageFormat::Hl7
}

/// Re-indent `json` or `xml` content with two spaces (`§5.2`).
///
/// JSON object keys come out sorted, not in source order (`§5.3`).
///
/// # Errors
/// A message naming the format and the parser's reason (never the content),
/// or saying the format is unsupported.
pub fn pretty_print(content: &str, format: MessageFormat) -> Result<String, String> {
    match format {
        MessageFormat::Json => {
            let value: serde_json::Value =
                serde_json::from_str(content).map_err(|e| format!("Invalid JSON: {e}"))?;
            serde_json::to_string_pretty(&value).map_err(|e| format!("Invalid JSON: {e}"))
        }
        MessageFormat::Xml => pretty_xml(content),
        MessageFormat::Hl7 => Err("Formatting is not supported for 'hl7' messages".to_owned()),
    }
}

/// Parse and re-emit XML indented, dropping blank lines (`§5.2`).
fn pretty_xml(content: &str) -> Result<String, String> {
    let doc = Document::parse(content).map_err(|e| format!("Invalid XML: {e}"))?;
    let mut out = String::new();
    for child in doc.root().children() {
        write_node(child, 0, &mut out);
    }
    // Blank lines are dropped, as the Python's filter does (`§5.2`).
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Escape text, or an attribute value when `attribute` is set.
///
/// Text escapes `&`, `<` and `>`; an attribute value escapes `&`, `<` and
/// `"` and leaves `>` alone. Quote escaping matters only inside attributes,
/// which are always emitted double-quoted here.
fn escape(s: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' if !attribute => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// `prefix:local` using the prefix in scope for `uri`, else `local`.
///
/// `roxmltree` resolves prefixes away, so the original prefix is recovered
/// from the in-scope declarations. A default namespace has an empty prefix
/// and so yields the bare local name, which is what the source wrote.
fn qualified(node: Node<'_, '_>, uri: Option<&str>, local: &str) -> String {
    match uri.and_then(|u| node.lookup_prefix(u)) {
        Some(p) if !p.is_empty() => format!("{p}:{local}"),
        _ => local.to_owned(),
    }
}

/// Emit one node at `depth` (two spaces per level). Whitespace-only text is
/// dropped so the output indentation is ours, not the source's.
fn write_node(node: Node<'_, '_>, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match node.node_type() {
        NodeType::Element => write_element(node, depth, &pad, out),
        NodeType::Comment => {
            let _ = writeln!(out, "{pad}<!--{}-->", node.text().unwrap_or(""));
        }
        NodeType::PI => {
            if let Some(pi) = node.pi() {
                let _ = writeln!(out, "{pad}<?{} {}?>", pi.target, pi.value.unwrap_or(""));
            }
        }
        NodeType::Text => {
            let t = node.text().unwrap_or("").trim();
            if !t.is_empty() {
                let _ = writeln!(out, "{pad}{}", escape(t, false));
            }
        }
        NodeType::Root => {}
    }
}

/// Emit an element, inline when it holds only text.
///
/// Namespace declarations are written only where they are new relative to
/// the parent, so a prefix declared once at the root is not repeated on
/// every descendant. Attributes keep source order.
fn write_element(node: Node<'_, '_>, depth: usize, pad: &str, out: &mut String) {
    let name = qualified(node, node.tag_name().namespace(), node.tag_name().name());
    let mut open = format!("{pad}<{name}");
    let parent = node.parent_element();
    for ns in node.namespaces() {
        let inherited = parent.is_some_and(|p| {
            p.namespaces()
                .any(|q| q.name() == ns.name() && q.uri() == ns.uri())
        });
        if !inherited {
            match ns.name() {
                Some(prefix) => {
                    let _ = write!(open, " xmlns:{prefix}=\"{}\"", escape(ns.uri(), true));
                }
                None => {
                    let _ = write!(open, " xmlns=\"{}\"", escape(ns.uri(), true));
                }
            }
        }
    }
    for a in node.attributes() {
        let _ = write!(
            open,
            " {}=\"{}\"",
            qualified(node, a.namespace(), a.name()),
            escape(a.value(), true)
        );
    }
    let kids: Vec<Node<'_, '_>> = node
        .children()
        .filter(|c| !(c.is_text() && c.text().is_some_and(|t| t.trim().is_empty())))
        .collect();
    if kids.is_empty() {
        let _ = writeln!(out, "{open}/>");
    } else if kids.iter().all(Node::is_text) {
        // A text-only element stays on one line, as minidom does.
        let text: String = kids.iter().filter_map(Node::text).collect();
        let _ = writeln!(out, "{open}>{}</{name}>", escape(&text, false));
    } else {
        let _ = writeln!(out, "{open}>");
        for k in kids {
            write_node(k, depth + 1, out);
        }
        let _ = writeln!(out, "{pad}</{name}>");
    }
}
