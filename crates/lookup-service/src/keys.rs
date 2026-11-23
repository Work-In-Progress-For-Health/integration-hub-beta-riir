//! Composite-key handling.
//!
//! Implements `spec/20-lookup-service.md` §4.
//!
//! This is the single place that turns caller input (positional or named
//! parts) into the canonical key: the normalised parts in the table's
//! `key_columns` order. Stores, caches and Cosmos ids only ever see
//! canonical keys, so normalisation cannot drift between load and lookup.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::error::LookupError;
use crate::mapping::name_list;
use crate::model::{Case, KeyPartRule, TableDefinition};

/// What joins key parts in an id and in the display form (§4.5).
pub const KEY_PART_SEPARATOR: &str = "::";

/// Cosmos's limit on an id, in bytes (§4.5).
pub const MAX_COSMOS_ID_BYTES: usize = 255;

/// Prefix of an id that had to be hashed (§4.5).
pub const HASHED_ID_PREFIX: &str = "h:";

/// Whether a character is percent-encoded in an id.
///
/// Cosmos ids may not contain `/ \ ? #`; `:` and `%` are escaped as well so
/// that the join is reversible.
const fn id_unsafe(ch: char) -> bool {
    matches!(ch, '%' | ':' | '/' | '\\' | '?' | '#') || ch.is_ascii_control()
}

/// A readable, reversible Cosmos id for a canonical key; a SHA-256 form when
/// it would be too long (§4.5).
#[must_use]
pub fn encode_row_id(key: &[String]) -> String {
    let encoded = key
        .iter()
        .map(|part| {
            let mut out = String::with_capacity(part.len());
            for ch in part.chars() {
                if id_unsafe(ch) {
                    let mut buffer = [0_u8; 4];
                    for byte in ch.encode_utf8(&mut buffer).as_bytes() {
                        // Writing to a `String` cannot fail.
                        let _ = write!(out, "%{byte:02X}");
                    }
                } else {
                    out.push(ch);
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join(KEY_PART_SEPARATOR);
    if encoded.len() <= MAX_COSMOS_ID_BYTES {
        return encoded;
    }
    // The Python hashes `json.dumps(list(key), ensure_ascii=False)`, whose
    // element separator is ", ". Matching it byte for byte is what makes a
    // hashed id the same one the Python wrote.
    let json = format!(
        "[{}]",
        key.iter()
            .map(|part| json_string(part))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let digest = Sha256::digest(json.as_bytes());
    let mut id = String::from(HASHED_ID_PREFIX);
    for byte in digest {
        let _ = write!(id, "{byte:02x}");
    }
    id
}

/// `json.dumps(part, ensure_ascii=False)` for one string: quote, backslash and
/// control characters escaped, everything else literal. Written out so there
/// is no serialiser error to swallow (RCA-IN-6); a test pins it to
/// `serde_json`.
fn json_string(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    out.push('"');
    for ch in part.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c < ' ' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The human-readable form of a key, used for display and admin search.
#[must_use]
pub fn key_text(key: &[String]) -> String {
    key.join(&format!(" {KEY_PART_SEPARATOR} "))
}

/// Normalise one key part by a rule (§4.2).
#[must_use]
pub fn normalise_part(value: &str, rule: &KeyPartRule) -> String {
    let value = if rule.trim { value.trim() } else { value };
    match rule.case {
        Case::Upper => value.to_uppercase(),
        Case::Lower => value.to_lowercase(),
        Case::Preserve => value.to_owned(),
    }
}

/// Build the canonical key from exactly one of positional or named parts
/// (§4.3).
///
/// # Errors
///
/// `invalid_key` for a missing key, mixed styles, wrong arity, unknown or
/// missing part names, or a part that is empty after normalisation.
pub fn canonical_key(
    definition: &TableDefinition,
    positional: &[String],
    named: &BTreeMap<String, String>,
) -> Result<Vec<String>, LookupError> {
    let table = definition.name.as_str();
    if !positional.is_empty() && !named.is_empty() {
        return Err(LookupError::invalid_key(
            "Supply the key either positionally (k=) or by name (key.<part>=), not both",
            table,
        ));
    }
    let raw_parts: Vec<&str> = if !named.is_empty() {
        let unknown: Vec<&String> = named
            .keys()
            .filter(|name| !definition.key_columns.contains(name))
            .collect();
        if !unknown.is_empty() {
            return Err(LookupError::invalid_key(
                format!(
                    "Unknown key part(s) {}; expected {}",
                    name_list(&unknown),
                    describe(definition)
                ),
                table,
            ));
        }
        let missing: Vec<&String> = definition
            .key_columns
            .iter()
            .filter(|column| !named.contains_key(*column))
            .collect();
        if !missing.is_empty() {
            return Err(LookupError::invalid_key(
                format!(
                    "Missing key part(s) {}; expected {}",
                    name_list(&missing),
                    describe(definition)
                ),
                table,
            ));
        }
        definition
            .key_columns
            .iter()
            .filter_map(|column| named.get(column).map(String::as_str))
            .collect()
    } else if !positional.is_empty() {
        let expected = definition.key_columns.len();
        if positional.len() != expected {
            return Err(LookupError::invalid_key(
                format!(
                    "Expected {expected} key part(s) {}, got {}",
                    describe(definition),
                    positional.len()
                ),
                table,
            ));
        }
        positional.iter().map(String::as_str).collect()
    } else {
        return Err(LookupError::invalid_key(
            format!("No key supplied; expected parts {}", describe(definition)),
            table,
        ));
    };

    let mut parts = Vec::with_capacity(raw_parts.len());
    for (column, raw) in definition.key_columns.iter().zip(raw_parts) {
        let part = normalise_part(raw, &definition.rule_for(column));
        if part.is_empty() {
            return Err(LookupError::invalid_key(
                format!("Key part '{column}' is empty"),
                table,
            ));
        }
        parts.push(part);
    }
    Ok(parts)
}

/// `(a, b)`, for the messages.
fn describe(definition: &TableDefinition) -> String {
    format!("({})", definition.key_columns.join(", "))
}

#[cfg(test)]
mod tests {
    use super::json_string;

    /// `json_string` is `json.dumps(.., ensure_ascii=False)` for a string, so
    /// a hashed id stays the one the Python wrote (§4.4).
    #[test]
    fn json_string_matches_serde_json() {
        for text in [
            "",
            "plain",
            "quote \" and \\ slash",
            "line\nreturn\rtab\t",
            "\u{8}\u{c}\u{1}\u{1f}\u{7f}",
            "caf\u{e9} \u{1f600} \u{2028}",
        ] {
            assert_eq!(
                Ok(json_string(text)),
                serde_json::to_string(text).map_err(|e| e.to_string()),
                "{text:?}"
            );
        }
    }
}
