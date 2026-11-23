//! Parsing what arrives on the wire: query strings, urlencoded forms and
//! multipart bodies.
//!
//! Implements `spec/20-lookup-service.md` §4.3 and §8. Written here rather
//! than pulled in because the whole need is a few dozen lines and each
//! dependency in this workspace is a decision (`agents/workspace.md`).

use std::fmt::Write as _;

/// A query or form component that is not valid percent-encoding.
///
/// Carries nothing from the input: a malformed query can hold anything, and
/// the message that names it reaches the client and a log (`§12.7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MalformedEncoding;

impl std::fmt::Display for MalformedEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the query string or form is not valid percent-encoding")
    }
}

impl std::error::Error for MalformedEncoding {}

/// Percent-decode a query or form component, treating `+` as a space.
///
/// # Errors
///
/// [`MalformedEncoding`] for a `%` that is not followed by two hex digits, or
/// for bytes that are not UTF-8 once decoded. Neither is kept literally or
/// replaced with U+FFFD: malformed input is refused, not repaired
/// (`rust-checked-arithmetic` RCA-IN-2; `spec/20-lookup-service.md` §4.3).
///
/// # Examples
///
/// ```
/// use lookup_service::form::percent_decode;
///
/// assert_eq!(percent_decode("a%20b+c").as_deref(), Ok("a b c"));
/// assert!(percent_decode("100%").is_err());
/// assert!(percent_decode("%zz").is_err());
/// assert!(percent_decode("%FF").is_err(), "not UTF-8");
/// ```
pub fn percent_decode(text: &str) -> Result<String, MalformedEncoding> {
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        match byte {
            b'+' => out.push(b' '),
            b'%' => match tail {
                [high, low, after @ ..] if let Some(decoded) = escaped(*high, *low) => {
                    out.push(decoded);
                    rest = after;
                }
                _ => return Err(MalformedEncoding),
            },
            byte => out.push(byte),
        }
    }
    String::from_utf8(out).map_err(|_| MalformedEncoding)
}

/// The byte `%<high><low>` stands for, when both are hex digits.
fn escaped(high: u8, low: u8) -> Option<u8> {
    hex(high)?.checked_mul(16)?.checked_add(hex(low)?)
}

/// The value of a hex digit.
const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0' => Some(0),
        b'1' => Some(1),
        b'2' => Some(2),
        b'3' => Some(3),
        b'4' => Some(4),
        b'5' => Some(5),
        b'6' => Some(6),
        b'7' => Some(7),
        b'8' => Some(8),
        b'9' => Some(9),
        b'a' | b'A' => Some(10),
        b'b' | b'B' => Some(11),
        b'c' | b'C' => Some(12),
        b'd' | b'D' => Some(13),
        b'e' | b'E' => Some(14),
        b'f' | b'F' => Some(15),
        _ => None,
    }
}

/// Percent-encode a component for use in a URL I build myself.
#[must_use]
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// `a=1&b=2` as ordered pairs, repeats kept.
///
/// # Errors
///
/// [`MalformedEncoding`] if any name or value is not valid percent-encoding.
pub fn parse_pairs(query: &str) -> Result<Vec<(String, String)>, MalformedEncoding> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            Ok((percent_decode(name)?, percent_decode(value)?))
        })
        .collect()
}

/// One part of a multipart body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The form field name.
    pub name: String,
    /// The file name, for a file part.
    pub file_name: Option<String>,
    /// The part's bytes.
    pub data: Vec<u8>,
}

/// The boundary named by a `multipart/form-data` content type.
#[must_use]
pub fn boundary_of(content_type: &str) -> Option<String> {
    let (kind, params) = content_type.split_once(';')?;
    if !kind.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    params.split(';').find_map(|param| {
        let (name, value) = param.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("boundary")
            .then(|| value.trim().trim_matches('"').to_owned())
            .filter(|boundary| !boundary.is_empty())
    })
}

/// What comes before and after the first `needle` in `haystack`.
pub(crate) fn split_at_needle<'a>(
    haystack: &'a [u8],
    needle: &[u8],
) -> Option<(&'a [u8], &'a [u8])> {
    if needle.is_empty() {
        return None;
    }
    let at = haystack
        .windows(needle.len())
        .position(|window| window == needle)?;
    let (before, from_needle) = haystack.split_at_checked(at)?;
    Some((before, from_needle.get(needle.len()..)?))
}

/// The reason for every framing failure: never a piece of the body.
const MALFORMED: &str = "malformed multipart body";

/// Parse a multipart body into its parts.
///
/// # Errors
///
/// A short reason, never a piece of the body, when the framing is broken.
pub fn parse_multipart(body: &[u8], boundary: &str) -> Result<Vec<Part>, &'static str> {
    let delimiter = format!("--{boundary}").into_bytes();
    let separator = format!("\r\n--{boundary}").into_bytes();
    let mut rest = split_at_needle(body, &delimiter).ok_or(MALFORMED)?.1;
    let mut parts = Vec::new();
    loop {
        if rest.starts_with(b"--") {
            return Ok(parts);
        }
        // Optional transport padding, then the line break after the boundary.
        let padding = rest
            .iter()
            .take_while(|byte| matches!(**byte, b' ' | b'\t'))
            .count();
        let after_padding = rest.get(padding..).ok_or(MALFORMED)?;
        let line = after_padding.strip_prefix(b"\r\n").ok_or(MALFORMED)?;
        let (headers, after_headers) = split_at_needle(line, b"\r\n\r\n").ok_or(MALFORMED)?;
        let headers = std::str::from_utf8(headers).map_err(|_| "malformed multipart headers")?;
        let (data, after_data) = split_at_needle(after_headers, &separator).ok_or(MALFORMED)?;
        let (name, file_name) = disposition(headers);
        if let Some(name) = name {
            parts.push(Part {
                name,
                file_name,
                data: data.to_vec(),
            });
        }
        rest = after_data;
        if parts.len() > 64 {
            return Err("too many form fields");
        }
    }
}

/// `name` and `filename` from a part's headers.
fn disposition(headers: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut file_name = None;
    for line in headers.split("\r\n") {
        let Some((header, value)) = line.split_once(':') else {
            continue;
        };
        if !header.trim().eq_ignore_ascii_case("content-disposition") {
            continue;
        }
        for param in value.split(';').skip(1) {
            let Some((key, raw)) = param.split_once('=') else {
                continue;
            };
            let raw = raw.trim().trim_matches('"').to_owned();
            match key.trim().to_ascii_lowercase().as_str() {
                "name" => name = Some(raw),
                "filename" => file_name = Some(raw),
                _ => {}
            }
        }
    }
    (name, file_name)
}
