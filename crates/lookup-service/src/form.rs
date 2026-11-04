//! Parsing what arrives on the wire: query strings, urlencoded forms and
//! multipart bodies.
//!
//! Implements `spec/20-lookup-service.md` §4.3 and §8. Written here rather
//! than pulled in because the whole need is a few dozen lines and each
//! dependency in this workspace is a decision (`agents/workspace.md`).

use std::fmt::Write as _;

/// Percent-decode a query or form component, treating `+` as a space.
///
/// Invalid escapes are kept literally; invalid UTF-8 is replaced. Both are
/// the lenient behaviour of the framework the Python runs on.
#[must_use]
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len()
                && hex(bytes[i + 1]).is_some()
                && hex(bytes[i + 2]).is_some() =>
            {
                // The guard has just checked both digits.
                let high = hex(bytes[i + 1]).unwrap_or(0);
                let low = hex(bytes[i + 2]).unwrap_or(0);
                out.push(high * 16 + low);
                i += 2;
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The value of a hex digit.
const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
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
#[must_use]
pub fn parse_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(name), percent_decode(value))
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

/// Position of `needle` in `haystack` at or after `from`.
fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from > haystack.len() - needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Parse a multipart body into its parts.
///
/// # Errors
///
/// A short reason, never a piece of the body, when the framing is broken.
pub fn parse_multipart(body: &[u8], boundary: &str) -> Result<Vec<Part>, &'static str> {
    let delimiter = format!("--{boundary}").into_bytes();
    let separator = format!("\r\n--{boundary}").into_bytes();
    let mut position =
        find(body, &delimiter, 0).ok_or("malformed multipart body")? + delimiter.len();
    let mut parts = Vec::new();
    loop {
        if body[position..].starts_with(b"--") {
            return Ok(parts);
        }
        // Optional transport padding, then the line break after the boundary.
        while body
            .get(position)
            .is_some_and(|b| *b == b' ' || *b == b'\t')
        {
            position += 1;
        }
        if !body[position..].starts_with(b"\r\n") {
            return Err("malformed multipart body");
        }
        position += 2;
        let headers_end = find(body, b"\r\n\r\n", position).ok_or("malformed multipart body")?;
        let headers = std::str::from_utf8(&body[position..headers_end])
            .map_err(|_| "malformed multipart headers")?;
        let data_start = headers_end + 4;
        let data_end = find(body, &separator, data_start).ok_or("malformed multipart body")?;
        let data = body[data_start..data_end].to_vec();
        let (name, file_name) = disposition(headers);
        if let Some(name) = name {
            parts.push(Part {
                name,
                file_name,
                data,
            });
        }
        position = data_end + separator.len();
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
