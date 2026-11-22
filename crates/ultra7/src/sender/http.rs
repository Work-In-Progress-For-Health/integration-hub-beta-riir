//! REST and SOAP over HTTP/1.1 on a plain socket.
//! `spec/ultra7-test-message-sender.md` `§7.3`-`§7.5`.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Instant;

use super::{SendResult, timeout_of};
use crate::model::{Endpoint, Message, MessageFormat};

/// Wrap in a SOAP 1.1 envelope unless the first 200 characters already say
/// `Envelope` (`§7.4`).
#[must_use]
pub fn wrap_in_envelope(content: &str) -> String {
    let stripped = content.trim();
    let head: String = stripped.chars().take(200).collect();
    if head.contains("Envelope") {
        return stripped.to_owned();
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><soap:Envelope \
         xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\"><soap:Body>{stripped}\
         </soap:Body></soap:Envelope>"
    )
}

/// A parsed `http://host[:port]/path` target.
///
/// Parsed by hand, in the same spirit as the plain-socket client of `§7.5`:
/// a developer tool should not grow its build graph for a URL parser.
struct Target {
    /// Host to connect to, without brackets.
    host: String,
    /// TCP port; 80 unless the URL names one.
    port: u16,
    /// Request target: path and query, never empty.
    path: String,
    /// The authority as written, for the `Host` header.
    host_header: String,
}

/// Check the scheme and split the URL. `Err` carries the `§7.3` / `§7.5`
/// error text, which names the problem but never echoes the URL, so any
/// userinfo in it cannot reach the terminal.
fn validate(endpoint: &Endpoint) -> Result<Target, String> {
    if endpoint.url.is_empty() {
        return Err("URL is required".to_owned());
    }
    let (scheme, rest) = endpoint
        .url
        .split_once("://")
        .unwrap_or((endpoint.url.as_str(), ""));
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" => {}
        "https" => {
            return Err(
                "https is not supported by this build; use an http:// endpoint or a local proxy"
                    .to_owned(),
            );
        }
        other => return Err(format!("Unsupported URL scheme: {other}")),
    }
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(split);
    let tail = tail.split('#').next().unwrap_or("");
    let path = if tail.starts_with('/') {
        tail.to_owned()
    } else {
        format!("/{tail}")
    };
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (
            h,
            p.parse::<u16>()
                .map_err(|_| "Invalid port in URL".to_owned())?,
        ),
        _ => (authority, 80),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err("URL has no host".to_owned());
    }
    Ok(Target {
        host: host.to_owned(),
        port,
        path,
        host_header: authority.to_owned(),
    })
}

/// POST over a socket and judge the status (`§7.3`).
///
/// Sends `Connection: close` and reads to EOF so no response framing beyond
/// chunking has to be implemented. Redirects are not followed (`§7.5`).
fn post(endpoint: &Endpoint, body: &[u8], headers: &[(String, String)]) -> SendResult {
    let target = match validate(endpoint) {
        Ok(t) => t,
        Err(e) => return SendResult::failure(0.0, String::new(), e),
    };
    // A header name or value with a line break would let a project file
    // inject headers or a second request.
    if headers
        .iter()
        .any(|(k, v)| k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']))
    {
        return SendResult::failure(
            0.0,
            String::new(),
            "Invalid header (line break or colon in name)",
        );
    }
    let mut req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        target.path,
        target.host_header,
        body.len()
    );
    for (k, v) in headers {
        let _ = write!(req, "{k}: {v}\r\n");
    }
    req.push_str("\r\n");

    let timeout = timeout_of(endpoint);
    let start = Instant::now();
    let ms = || start.elapsed().as_secs_f64() * 1000.0;
    let exchange = || -> std::io::Result<Vec<u8>> {
        let addr = (target.host.as_str(), target.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "host did not resolve")
            })?;
        let mut sock = TcpStream::connect_timeout(&addr, timeout)?;
        sock.set_read_timeout(Some(timeout))?;
        sock.set_write_timeout(Some(timeout))?;
        sock.write_all(req.as_bytes())?;
        sock.write_all(body)?;
        let mut raw = Vec::new();
        sock.read_to_end(&mut raw)?;
        Ok(raw)
    };
    let raw = match exchange() {
        Ok(r) => r,
        Err(e) => return SendResult::failure(ms(), String::new(), e.to_string()),
    };
    let latency = ms();
    let Some((status, response_body)) = parse_response(&raw) else {
        return SendResult::failure(latency, String::new(), "Malformed HTTP response");
    };
    if (200..300).contains(&status) {
        SendResult {
            ok: true,
            latency_ms: latency,
            response_summary: format!("HTTP {status}: {response_body}"),
            error: None,
        }
    } else {
        SendResult::failure(latency, response_body, format!("HTTP {status}"))
    }
}

/// What comes before and after the first `needle` in `haystack`.
fn split_at_needle<'a>(haystack: &'a [u8], needle: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let at = haystack
        .windows(needle.len())
        .position(|window| window == needle)?;
    let (before, from_needle) = haystack.split_at_checked(at)?;
    Some((before, from_needle.get(needle.len()..)?))
}

/// Status code and decoded body of a `Connection: close` response.
fn parse_response(raw: &[u8]) -> Option<(u16, String)> {
    let (head_bytes, body) = split_at_needle(raw, b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(head_bytes);
    let mut lines = head.lines();
    let status = lines
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let bytes = if chunked {
        dechunk(body)
    } else {
        body.to_vec()
    };
    Some((status, String::from_utf8_lossy(&bytes).into_owned()))
}

/// Decode a `Transfer-Encoding: chunked` body; stops quietly on damage.
///
/// Lenient on purpose: a truncated final chunk yields what arrived, since
/// the tool exists to show a misbehaving receiver's reply, not reject it.
fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some((size_line, after_line)) = split_at_needle(body, b"\r\n") {
        let size_text = String::from_utf8_lossy(size_line);
        let Ok(size) = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
        else {
            break;
        };
        body = after_line;
        if size == 0 || body.len() < size {
            if size != 0 {
                out.extend_from_slice(body);
            }
            break;
        }
        let Some((chunk, tail)) = body.split_at_checked(size) else {
            break;
        };
        out.extend_from_slice(chunk);
        body = tail.get(2..).unwrap_or(&[]);
    }
    out
}

/// The endpoint's headers plus a default that the endpoint may override.
fn headers_with_default(endpoint: &Endpoint, name: &str, value: &str) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = endpoint
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // Header names are case-insensitive, so the endpoint's wins however it
    // spells it (`§7.4`).
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name)) {
        headers.push((name.to_owned(), value.to_owned()));
    }
    headers
}

/// POST the body as is (`§7.3`).
#[must_use]
pub fn send_rest(endpoint: &Endpoint, message: &Message) -> SendResult {
    if let Err(e) = validate(endpoint) {
        return SendResult::failure(0.0, String::new(), e);
    }
    let content_type = match message.format {
        MessageFormat::Hl7 => "application/hl7-v2",
        MessageFormat::Xml => "application/xml",
        MessageFormat::Json => "application/json",
    };
    let headers = headers_with_default(endpoint, "Content-Type", content_type);
    post(endpoint, message.content.as_bytes(), &headers)
}

/// POST the body in a SOAP envelope (`§7.4`).
#[must_use]
pub fn send_soap(endpoint: &Endpoint, message: &Message) -> SendResult {
    if let Err(e) = validate(endpoint) {
        return SendResult::failure(0.0, String::new(), e);
    }
    let mut headers = headers_with_default(endpoint, "Content-Type", "text/xml; charset=utf-8");
    if !endpoint.soap_action.is_empty()
        && !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("SOAPAction"))
    {
        headers.push(("SOAPAction".to_owned(), endpoint.soap_action.clone()));
    }
    post(
        endpoint,
        wrap_in_envelope(&message.content).as_bytes(),
        &headers,
    )
}

/// Host and explicit port of a URL, for the announce line (`§8.2`). Falls
/// back to the whole URL as the host when there is none, as the Python's
/// `urlparse(...).hostname or url`.
#[must_use]
pub fn url_host_port(url: &str) -> (String, Option<u16>) {
    let Some((_, rest)) = url.split_once("://") else {
        return (url.to_owned(), None);
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (h, p.parse::<u16>().ok()),
        _ => (authority, None),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        (url.to_owned(), None)
    } else {
        (host.to_owned(), port)
    }
}
