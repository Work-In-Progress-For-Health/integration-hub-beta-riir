//! The reference-data client: PIMS codes to eMPI codes.
//!
//! Implements `spec/09-transformers.md` §5.10.
//!
//! Reference implementation:
//! `transformers/hl7_pims_transformer/hl7_pims_transformer/clients/reference_data_client.py`
//! (upstream `c1d2a74`, retargeted at `lookup_service` by `04b30e7`, #427).
//!
//! # Why the client is synchronous
//!
//! [`ihb_transformer::Transformer::transform`] is a plain function and the
//! receive loop processes one message of a session at a time (`§1.7`), so
//! there is nothing to overlap a lookup with. A blocking `std` socket with a
//! deadline is the smallest thing that is correct; [`LookupClient`] wraps the
//! call in `block_in_place` when it is on a multi-threaded runtime so the
//! session-lock renewal task keeps a worker (`§5.10.6`).
//!
//! # What an error contains
//!
//! `spec/index.md` §0.8 and `spec/09-transformers.md` §5.10.3: a
//! [`LookupError`] names the **dataset** and the failure, never the source
//! code or the target code. Upstream interpolates both; the source code
//! comes from a free-text field (`PID-16`, `PID-22`) in a clinical message,
//! so this port does not.
//!
//! # Examples
//!
//! The datasets, and the format a target code must have:
//!
//! ```
//! use hl7_pims_transformer::reference_data::Dataset;
//!
//! assert_eq!(Dataset::NhsStatus.code_type(), "nhs-status");
//! assert!(Dataset::NhsStatus.accepts("3"));
//! assert!(!Dataset::NhsStatus.accepts("NN"), "digits only, at most two");
//! assert!(Dataset::Gender.accepts("U"));
//! ```

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ihb_config::{ConfigErrors, Reader, Secret};
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, ClientConnection, StreamOwned};

/// The lookup table that holds every translation (`§5.10.4`).
pub const LOOKUP_TABLE: &str = "pims_to_mpi_codes";

/// `urllib3`'s `Retry(status_forcelist=...)` in upstream (`§5.10.5`).
const RETRYABLE_STATUSES: [u16; 4] = [429, 502, 503, 504];

/// The most response body read. A lookup answer is a few hundred bytes; a
/// misrouted request can return a whole HTML page, and reading it all helps
/// nobody.
const MAX_BODY: u64 = 64 * 1024;

/// Which code system a lookup is in (`§5.10`).
///
/// Each variant names the `PID` field it translates; the same variant picks
/// the URL path segment ([`Dataset::code_type`]) and the shape the returned
/// code must have before it is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dataset {
    /// `PID-8`.
    Gender,
    /// `PID-16.1`.
    MaritalStatus,
    /// `PID-22.1`.
    EthnicGroup,
    /// `PID-32`, from `PID-3`'s `NI` repetition.
    NhsStatus,
}

impl Dataset {
    /// The `code_type` key part (`§5.10`'s table): the dataset's name as it
    /// appears in the lookup URL, e.g. `marital-status`.
    #[must_use]
    pub const fn code_type(self) -> &'static str {
        match self {
            Self::Gender => "gender",
            Self::MaritalStatus => "marital-status",
            Self::EthnicGroup => "ethnic-group",
            Self::NhsStatus => "nhs-status",
        }
    }

    /// Whether a **non-blank** target code has the format the eMPI accepts.
    ///
    /// The eMPI rejects malformed codes, so one that does not match is
    /// treated as "returned in the incorrect format" (upstream's user-story
    /// Scenario 2) rather than forwarded. The patterns are short,
    /// delimiter-free tokens, which keeps HL7 separators out of the message.
    #[must_use]
    pub fn accepts(self, code: &str) -> bool {
        let (max, digits_only) = match self {
            Self::Gender => (3, false),
            Self::MaritalStatus | Self::EthnicGroup => (4, false),
            Self::NhsStatus => (2, true),
        };
        let length = code.chars().count();
        (1..=max).contains(&length)
            && code.chars().all(|c| {
                if digits_only {
                    c.is_ascii_digit()
                } else {
                    c.is_ascii_alphanumeric()
                }
            })
    }
}

/// Why a code could not be resolved (`§5.10.3`).
///
/// The text names a dataset and a failure and carries no code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LookupError(pub String);

/// Anything that can resolve a PIMS code.
///
/// A trait rather than the concrete client so the mapping is tested without
/// a socket, as upstream's `ReferenceDataLookup` protocol does.
pub trait ReferenceData: Send + Sync {
    /// Resolve `source_code` (already trimmed, never blank) in `dataset`.
    ///
    /// Returns `""` when the mapping deliberately has no target (`§5.10.2`).
    ///
    /// # Errors
    ///
    /// [`LookupError`], one of `§5.10.3`'s six.
    fn lookup(&self, dataset: Dataset, source_code: &str) -> Result<String, LookupError>;
}

/// The settings of `§5.10.4`/`§5.10.5`.
///
/// [`Default`] is the upstream defaults (5 s, 3 retries, 0.5 s backoff) with
/// no URL and no key; the binary overlays environment variables on it.
#[derive(Debug, Clone)]
pub struct ReferenceDataConfig {
    /// `REFERENCE_DATA_API_BASE_URL`, without a trailing `/`. `None` is
    /// tolerated at start-up and fails on the first lookup, as upstream does,
    /// so a flow whose messages never carry these fields still starts.
    pub base_url: Option<String>,
    /// `REFERENCE_DATA_API_TIMEOUT_SECONDS`: the connect timeout, each socket
    /// read and write timeout, and (as a deadline checked between reads) the
    /// bound on one whole attempt. Retries each get a fresh one.
    pub timeout: Duration,
    /// `REFERENCE_DATA_API_MAX_RETRIES`: retries *after* the first attempt,
    /// so the number of requests is `max_retries + 1` at most.
    pub max_retries: u32,
    /// `REFERENCE_DATA_API_RETRY_BACKOFF_SECONDS`: base of the exponential
    /// delay in [`backoff_before`]; seconds, may be fractional.
    pub backoff_seconds: f64,
    /// `REFERENCE_DATA_API_KEY`, sent as `X-API-Key`.
    pub api_key: Option<Secret>,
}

impl Default for ReferenceDataConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            timeout: Duration::from_secs(5),
            max_retries: 3,
            backoff_seconds: 0.5,
            api_key: None,
        }
    }
}

impl ReferenceDataConfig {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every unparseable or out-of-range setting, together (`02` §1.4).
    pub fn from_env() -> Result<Self, ConfigErrors> {
        let mut reader = Reader::from_env();
        let config = Self::read(&mut reader);
        reader.finish()?;
        Ok(config)
    }

    /// Read from explicit pairs, for tests.
    ///
    /// # Errors
    ///
    /// As [`ReferenceDataConfig::from_env`].
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, ConfigErrors>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut reader = Reader::from_pairs(pairs);
        let config = Self::read(&mut reader);
        reader.finish()?;
        Ok(config)
    }

    /// Read from a reader the caller owns; problems accumulate in it.
    pub fn read(reader: &mut Reader) -> Self {
        let defaults = Self::default();
        let base_url = reader
            .optional_string("REFERENCE_DATA_API_BASE_URL")
            .map(|url| url.trim_end_matches('/').to_owned());

        let timeout: f64 = reader.parse_or(
            "REFERENCE_DATA_API_TIMEOUT_SECONDS",
            "a positive number",
            defaults.timeout.as_secs_f64(),
        );
        // Upstream accepts anything `float()` does and lets urllib3 fail
        // later; a zero or negative deadline here would make every lookup
        // fail instantly or panic in `Duration::from_secs_f64`.
        let timeout = if timeout.is_finite() && timeout > 0.0 {
            Duration::from_secs_f64(timeout)
        } else {
            reader.invalid(
                "REFERENCE_DATA_API_TIMEOUT_SECONDS",
                "a positive number",
                timeout.to_string(),
            );
            defaults.timeout
        };

        let max_retries = reader.integer_or("REFERENCE_DATA_API_MAX_RETRIES", defaults.max_retries);
        let backoff_seconds: f64 = reader.parse_or(
            "REFERENCE_DATA_API_RETRY_BACKOFF_SECONDS",
            "a number >= 0",
            defaults.backoff_seconds,
        );
        let backoff_seconds = if backoff_seconds.is_finite() && backoff_seconds >= 0.0 {
            backoff_seconds
        } else {
            reader.invalid(
                "REFERENCE_DATA_API_RETRY_BACKOFF_SECONDS",
                "a number >= 0",
                backoff_seconds.to_string(),
            );
            defaults.backoff_seconds
        };

        Self {
            base_url,
            timeout,
            max_retries,
            backoff_seconds,
            api_key: reader.optional_secret("REFERENCE_DATA_API_KEY"),
        }
    }
}

/// Percent-encode a query component.
///
/// Everything except the RFC 3986 unreserved set is encoded. Python's `urlencode` (`quote_plus`) leaves `-_.~` and
/// alphanumerics alone too, and turns a space into `+`; a space is encoded
/// as `%20` here, which the lookup service decodes identically.
///
/// # Examples
///
/// ```
/// use hl7_pims_transformer::reference_data::encode_component;
///
/// assert_eq!(encode_component("nhs-status"), "nhs-status");
/// assert_eq!(encode_component("a/b"), "a%2Fb");
/// ```
#[must_use]
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The request path and query of `§5.10.4`.
///
/// # Examples
///
/// ```
/// use hl7_pims_transformer::reference_data::{request_target, Dataset};
///
/// assert_eq!(
///     request_target(Dataset::Gender, "F"),
///     "/api/v1/lookup/pims_to_mpi_codes?key.code_type=gender&key.pims_code=F"
/// );
/// ```
#[must_use]
pub fn request_target(dataset: Dataset, source_code: &str) -> String {
    format!(
        "/api/v1/lookup/{LOOKUP_TABLE}?key.code_type={}&key.pims_code={}",
        encode_component(dataset.code_type()),
        encode_component(source_code)
    )
}

/// The real client: `http` and `https`, with `urllib3`'s retry rules.
pub struct LookupClient {
    /// The settings.
    config: ReferenceDataConfig,
    /// TLS settings, built once: assembling the root store per request is
    /// measurable (`dashboard`'s `HttpsClient` does the same).
    tls: Arc<ClientConfig>,
}

impl std::fmt::Debug for LookupClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No `config`: it holds the API key.
        f.debug_struct("LookupClient").finish_non_exhaustive()
    }
}

impl LookupClient {
    /// A client for `config`.
    ///
    /// # Errors
    ///
    /// [`ihb_tls::TrustError`] when `SSL_CERT_FILE` names an unusable bundle
    /// (`spec/18-deployment.md` §4.3: fatal, not a fallback).
    pub fn new(config: ReferenceDataConfig) -> Result<Self, ihb_tls::TrustError> {
        Ok(Self {
            config,
            tls: Arc::new(ihb_tls::client_config()?),
        })
    }

    /// One attempt: `Ok((status, body))` or a transport failure.
    fn attempt(&self, base: &str, target: &str) -> Result<(u16, Vec<u8>), String> {
        let (https, rest) = match base.split_once("://") {
            Some(("http", rest)) => (false, rest),
            Some(("https", rest)) => (true, rest),
            _ => return Err("the base URL is not http or https".to_owned()),
        };
        let (authority, prefix) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
        let host = authority
            .rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
            .map_or(authority, |(host, _)| host);
        let port = authority
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .unwrap_or(if https { 443 } else { 80 });
        let path = if prefix.is_empty() {
            target.to_owned()
        } else {
            format!("/{prefix}{target}")
        };

        let deadline = Instant::now() + self.config.timeout;
        let address = (host, port)
            .to_socket_addrs()
            .map_err(|_| "the host could not be resolved".to_owned())?
            .next()
            .ok_or_else(|| "the host has no address".to_owned())?;
        let stream = TcpStream::connect_timeout(&address, self.config.timeout)
            .map_err(|_| "could not connect".to_owned())?;
        // A deadline per read and write, not for the whole exchange: the
        // exchange is one small request and one small response, and the
        // `Instant` above bounds the total below.
        stream
            .set_read_timeout(Some(self.config.timeout))
            .and_then(|()| stream.set_write_timeout(Some(self.config.timeout)))
            .map_err(|_| "could not set a deadline".to_owned())?;

        let mut request = format!(
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\n\
             Connection: close\r\n"
        );
        if let Some(key) = &self.config.api_key {
            request.push_str("X-API-Key: ");
            request.push_str(key.expose());
            request.push_str("\r\n");
        }
        request.push_str("\r\n");

        let raw = if https {
            let name = ServerName::try_from(host.to_owned())
                .map_err(|_| "the host is not a valid TLS name".to_owned())?;
            let connection = ClientConnection::new(Arc::clone(&self.tls), name)
                .map_err(|_| "could not start TLS".to_owned())?;
            exchange(StreamOwned::new(connection, stream), &request, deadline)?
        } else {
            exchange(stream, &request, deadline)?
        };
        parse_response(&raw)
    }
}

/// Send `request`, read everything up to the close, giving up at `deadline`
/// (checked between reads) and stopping once more than `MAX_BODY` plus a
/// header allowance has arrived.
///
/// Errors are fixed strings that name the step that failed, never the
/// request or response, so a lookup failure cannot leak a code or a key into
/// a log line.
fn exchange<S: Read + Write>(
    mut stream: S,
    request: &str,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "the request could not be sent".to_owned())?;
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if Instant::now() > deadline {
            return Err("no answer within the deadline".to_owned());
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() as u64 > MAX_BODY + 8192 {
                    break;
                }
            }
            // A TLS peer that closes without `close_notify` surfaces as
            // `UnexpectedEof`; with `Connection: close` the body is complete
            // by then, and `parse_response` checks it parses.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => return Err("the answer could not be read".to_owned()),
        }
    }
    Ok(raw)
}

/// Split a raw HTTP/1.1 response into status and (de-chunked) body.
///
/// # Errors
///
/// A description with no values in it.
fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "the answer was incomplete".to_owned())?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "the answer had no status".to_owned())?;
    let chunked = lines.any(|line| {
        let line = line.to_ascii_lowercase();
        line.starts_with("transfer-encoding:") && line.contains("chunked")
    });
    let body = &raw[split + 4..];
    if !chunked {
        return Ok((status, body.to_vec()));
    }

    // uvicorn answers `Connection: close` with a content-length, but a
    // proxy in front of it may chunk; handle it rather than assume.
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| "the chunked answer was malformed".to_owned())?;
        let size = std::str::from_utf8(&rest[..end])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok())
            .ok_or_else(|| "the chunked answer was malformed".to_owned())?;
        rest = &rest[end + 2..];
        if size == 0 {
            return Ok((status, out));
        }
        if rest.len() < size + 2 {
            return Err("the chunked answer was truncated".to_owned());
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// Seconds to sleep before retry number `retry` (1-based): none before the
/// first, `backoff * 2^(retry-1)` after (`urllib3` 2.x
/// `Retry.get_backoff_time`, `§5.10.5`).
///
/// # Examples
///
/// ```
/// use hl7_pims_transformer::reference_data::backoff_before;
///
/// assert_eq!(backoff_before(1, 0.5), 0.0);
/// assert_eq!(backoff_before(2, 0.5), 1.0);
/// assert_eq!(backoff_before(3, 0.5), 2.0);
/// ```
#[must_use]
pub fn backoff_before(retry: u32, backoff_seconds: f64) -> f64 {
    if retry <= 1 {
        0.0
    } else {
        backoff_seconds * f64::from(2_u32.saturating_pow(retry - 1))
    }
}

impl LookupClient {
    /// The lookup proper, on the calling thread: build the target, then try up
    /// to `max_retries + 1` times with [`backoff_before`] sleeps between.
    ///
    /// Blocking: callers on an async runtime go through
    /// [`ReferenceData::lookup`], which moves it off the worker.
    fn lookup_blocking(&self, dataset: Dataset, source_code: &str) -> Result<String, LookupError> {
        let name = dataset.code_type();
        let Some(base) = self.config.base_url.as_deref() else {
            return Err(LookupError(format!(
                "Reference-data API is not configured (REFERENCE_DATA_API_BASE_URL is unset); \
                 cannot resolve a code for dataset '{name}'."
            )));
        };
        let target = request_target(dataset, source_code);

        let mut retry = 0_u32;
        let (status, body) = loop {
            let outcome = self.attempt(base, &target);
            let retryable = match &outcome {
                Ok((status, _)) => RETRYABLE_STATUSES.contains(status),
                Err(_) => true,
            };
            if !retryable || retry >= self.config.max_retries {
                break outcome.map_err(|reason| {
                    LookupError(format!(
                        "Reference-data API request failed for dataset '{name}': {reason}"
                    ))
                })?;
            }
            retry += 1;
            std::thread::sleep(Duration::from_secs_f64(backoff_before(
                retry,
                self.config.backoff_seconds,
            )));
        };
        interpret(dataset, status, &body)
    }
}

/// Turn a final response into a target code (`§5.10.3` items 3 to 6).
///
/// Public so the rules are testable without a socket.
///
/// # Errors
///
/// [`LookupError`], one of `§5.10.3`'s.
///
/// # Examples
///
/// ```
/// use hl7_pims_transformer::reference_data::{interpret, Dataset};
///
/// let body = br#"{"table":"pims_to_mpi_codes","key":["gender","F"],"value":" F "}"#;
/// assert_eq!(interpret(Dataset::Gender, 200, body)?, "F");
///
/// // A blank target is a deliberate "leave unset" (`§5.10.2`).
/// assert_eq!(interpret(Dataset::MaritalStatus, 200, br#"{"value":""}"#)?, "");
///
/// assert!(interpret(Dataset::Gender, 404, b"").is_err());
/// assert!(interpret(Dataset::Gender, 200, br#"{"value":"A|B"}"#).is_err());
/// # Ok::<(), hl7_pims_transformer::reference_data::LookupError>(())
/// ```
pub fn interpret(dataset: Dataset, status: u16, body: &[u8]) -> Result<String, LookupError> {
    let name = dataset.code_type();
    match status {
        200 => {}
        404 => {
            return Err(LookupError(format!(
                "No reference-data mapping found for dataset '{name}'."
            )));
        }
        other => {
            return Err(LookupError(format!(
                "Reference-data API returned HTTP {other} for dataset '{name}'."
            )));
        }
    }
    let payload: serde_json::Value = serde_json::from_slice(body).map_err(|_| {
        LookupError(format!(
            "Reference-data API returned an unparseable body for dataset '{name}'."
        ))
    })?;
    let Some(value) = payload.get("value").and_then(serde_json::Value::as_str) else {
        return Err(LookupError(format!(
            "Reference-data API returned no target code for dataset '{name}'."
        )));
    };
    let value = value.trim();
    if !value.is_empty() && !dataset.accepts(value) {
        return Err(LookupError(format!(
            "Reference-data API returned a target code for dataset '{name}' \
             in an unexpected format."
        )));
    }
    Ok(value.to_owned())
}

impl ReferenceData for LookupClient {
    /// `§5.10.6`: runs the blocking lookup without stalling the runtime.
    fn lookup(&self, dataset: Dataset, source_code: &str) -> Result<String, LookupError> {
        // `§5.10.6`. `block_in_place` panics on a current-thread runtime and
        // is meaningless outside one, so it is used only where it is valid.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| self.lookup_blocking(dataset, source_code))
            }
            _ => self.lookup_blocking(dataset, source_code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Serve `answers` (raw HTTP responses) one per connection; returns the
    /// base URL, the requests seen and a hit counter.
    fn serve(
        answers: Vec<String>,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hits = Arc::new(AtomicUsize::new(0));
        let (seen2, hits2) = (Arc::clone(&seen), Arc::clone(&hits));
        std::thread::spawn(move || {
            for answer in answers {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0_u8; 4096];
                let n = socket.read(&mut buffer).unwrap_or(0);
                seen2
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buffer[..n]).into_owned());
                hits2.fetch_add(1, Ordering::SeqCst);
                let _ = socket.write_all(answer.as_bytes());
            }
        });
        (base, seen, hits)
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn status(code: u16) -> String {
        format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }

    fn client(base: Option<String>, retries: u32) -> LookupClient {
        LookupClient::new(ReferenceDataConfig {
            base_url: base,
            timeout: Duration::from_secs(2),
            max_retries: retries,
            backoff_seconds: 0.0,
            api_key: None,
        })
        .unwrap()
    }

    #[test]
    fn the_request_names_the_table_and_both_key_parts() {
        // spec/09-transformers.md §5.10.4.
        let (base, seen, _) = serve(vec![ok(r#"{"value":"F"}"#)]);
        let value = client(Some(base), 0).lookup(Dataset::Gender, "F").unwrap();

        assert_eq!(value, "F");
        let request = seen.lock().unwrap()[0].clone();
        assert!(
            request.starts_with(
                "GET /api/v1/lookup/pims_to_mpi_codes?key.code_type=gender&key.pims_code=F HTTP/1.1"
            ),
            "{request}"
        );
        assert!(request.contains("Accept: application/json"));
        assert!(!request.contains("X-API-Key"));
    }

    #[test]
    fn the_source_code_is_percent_encoded() {
        // spec/09-transformers.md §5.10.4; upstream test_source_code_is_url_encoded.
        let (base, seen, _) = serve(vec![ok(r#"{"value":"F"}"#)]);
        client(Some(base), 0)
            .lookup(Dataset::Gender, "a/b")
            .unwrap();

        assert!(seen.lock().unwrap()[0].contains("key.pims_code=a%2Fb "));
    }

    #[test]
    fn the_api_key_is_sent_when_set() {
        // spec/09-transformers.md §5.10.4.
        let (base, seen, _) = serve(vec![ok(r#"{"value":"F"}"#)]);
        let client = LookupClient::new(ReferenceDataConfig {
            base_url: Some(base),
            api_key: Some(Secret::new("k3y")),
            max_retries: 0,
            ..ReferenceDataConfig::default()
        })
        .unwrap();
        client.lookup(Dataset::Gender, "F").unwrap();

        assert!(seen.lock().unwrap()[0].contains("X-API-Key: k3y"));
        assert!(!format!("{client:?}").contains("k3y"), "never printed");
    }

    #[test]
    fn a_transient_status_is_retried_and_then_succeeds() {
        // spec/09-transformers.md §5.10.5.
        let (base, _, hits) = serve(vec![status(503), status(429), ok(r#"{"value":"2"}"#)]);
        let value = client(Some(base), 3)
            .lookup(Dataset::NhsStatus, "NN")
            .unwrap();

        assert_eq!(value, "2");
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn retries_are_bounded_and_the_last_status_is_reported() {
        // spec/09-transformers.md §5.10.3 item 4 and §5.10.5.
        let (base, _, hits) = serve(vec![status(503), status(503), status(503)]);
        let error = client(Some(base), 2)
            .lookup(Dataset::Gender, "F")
            .unwrap_err();

        assert_eq!(hits.load(Ordering::SeqCst), 3, "one try and two retries");
        assert!(error.0.contains("HTTP 503"), "{error}");
    }

    #[test]
    fn a_404_is_not_retried() {
        // spec/09-transformers.md §5.10.3 item 3.
        let (base, _, hits) = serve(vec![status(404), ok(r#"{"value":"F"}"#)]);
        let error = client(Some(base), 3)
            .lookup(Dataset::Gender, "F")
            .unwrap_err();

        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(error.0.contains("No reference-data mapping"), "{error}");
    }

    #[test]
    fn an_unreachable_service_fails_after_its_retries() {
        // spec/09-transformers.md §5.10.3 item 2: a closed port.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let error = client(Some(base), 1)
            .lookup(Dataset::Gender, "F")
            .unwrap_err();
        assert!(error.0.contains("request failed"), "{error}");
    }

    #[test]
    fn an_unset_base_url_fails_the_lookup_not_the_start() {
        // spec/09-transformers.md §5.10.3 item 1.
        let error = client(None, 0).lookup(Dataset::Gender, "F").unwrap_err();
        assert!(error.0.contains("REFERENCE_DATA_API_BASE_URL"), "{error}");
    }

    #[test]
    fn a_chunked_answer_is_read() {
        let body = "{\"value\":\"F\"}";
        let answer = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
            body.len()
        );
        let (base, _, _) = serve(vec![answer]);
        assert_eq!(
            client(Some(base), 0).lookup(Dataset::Gender, "F").unwrap(),
            "F"
        );
    }

    #[test]
    fn interpret_covers_every_failure_and_the_blank_target() {
        // spec/09-transformers.md §5.10.2 and §5.10.3 items 4 to 6.
        assert_eq!(
            interpret(Dataset::MaritalStatus, 200, br#"{"value":""}"#).unwrap(),
            ""
        );
        assert_eq!(
            interpret(Dataset::MaritalStatus, 200, br#"{"value":"  "}"#).unwrap(),
            ""
        );
        for (status, body) in [
            (500_u16, &b"{}"[..]),
            (200, b"not json"),
            (200, br#"{"table":"pims_to_mpi_codes"}"#),
            (200, br#"{"value":3}"#),
            (200, br#"{"value":"ABCD"}"#),
            (200, br#"{"value":"a^b"}"#),
        ] {
            assert!(
                interpret(Dataset::Gender, status, body).is_err(),
                "{status} {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn an_error_carries_no_code() {
        // spec/index.md §0.8 and spec/09 §5.10.3.
        let error = interpret(Dataset::Gender, 200, br#"{"value":"SECRET9"}"#).unwrap_err();
        assert!(!error.0.contains("SECRET9"), "{error}");
        let (base, _, _) = serve(vec![status(404)]);
        let error = client(Some(base), 0)
            .lookup(Dataset::Gender, "DISTINCTIVE")
            .unwrap_err();
        assert!(!error.0.contains("DISTINCTIVE"), "{error}");
    }

    #[test]
    fn the_patterns_are_the_documented_ones() {
        // spec/09-transformers.md §5.10's table.
        assert!(Dataset::Gender.accepts("ABC"));
        assert!(!Dataset::Gender.accepts("ABCD"));
        assert!(Dataset::MaritalStatus.accepts("AB12"));
        assert!(!Dataset::EthnicGroup.accepts("A B"));
        assert!(Dataset::NhsStatus.accepts("12"));
        assert!(!Dataset::NhsStatus.accepts("1A"));
        assert!(!Dataset::NhsStatus.accepts("123"));
        assert!(!Dataset::Gender.accepts(""), "blank is handled before this");
    }

    #[test]
    fn configuration_defaults_and_errors() {
        // spec/09-transformers.md §5.10.5.
        let config = ReferenceDataConfig::from_pairs([(
            "REFERENCE_DATA_API_BASE_URL",
            "http://lookup-service:8080/",
        )])
        .unwrap();
        assert_eq!(
            config.base_url.as_deref(),
            Some("http://lookup-service:8080")
        );
        assert_eq!(config.timeout, Duration::from_secs(5));
        assert_eq!(config.max_retries, 3);

        let bad = ReferenceDataConfig::from_pairs([
            ("REFERENCE_DATA_API_TIMEOUT_SECONDS", "0"),
            ("REFERENCE_DATA_API_MAX_RETRIES", "x"),
            ("REFERENCE_DATA_API_RETRY_BACKOFF_SECONDS", "-1"),
        ])
        .unwrap_err();
        assert_eq!(bad.len(), 3, "{bad}");
    }

    #[test]
    fn the_backoff_schedule_matches_urllib3() {
        // spec/09-transformers.md §5.10.5.
        assert_eq!(backoff_before(1, 0.5), 0.0);
        assert!((backoff_before(4, 0.5) - 4.0).abs() < f64::EPSILON);
    }
}
