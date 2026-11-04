//! The tenant's token signing keys, fetched over HTTPS and cached.
//!
//! Implements `spec/20-lookup-service.md` §12.2.
//!
//! Reference implementation: `jwt.PyJWKClient` as `security.py` uses it
//! (`JWKS_URL`, `JWKS_TIMEOUT_SECONDS = 5`).
//!
//! # Why a small client here
//!
//! One `GET` of a few kilobytes, at most every half minute. The workspace
//! has no general HTTP client (`dashboard`'s is private to it, and a second
//! TLS stack is something `deny.toml` makes you justify), so this is a
//! blocking request over the `rustls` that is already linked, run by the
//! caller on a blocking thread. `pims`'s reference-data client does the same
//! for its one call; the two are separate on purpose, since neither crate
//! should depend on the other for a socket.
//!
//! # Refresh policy
//!
//! The set is cached for [`CACHE_LIFESPAN`] (`PyJWKClient`'s default, 300 s).
//! A token naming a key id that is not cached refreshes the set, as
//! `PyJWKClient` does, but **at most once per [`MIN_REFRESH_INTERVAL`]**.
//! Upstream refreshes on every unknown id, so anyone able to send a token can
//! make the service fetch from Microsoft once per request; the interval
//! bounds that (deviation, `§12.2`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::security::{KeysError, SigningKeys, rsa_public_key_der};

/// Upstream `JWKS_TIMEOUT_SECONDS`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a fetched set is trusted (`PyJWKClient`'s default lifespan).
pub const CACHE_LIFESPAN: Duration = Duration::from_secs(300);
/// The shortest gap between two fetches.
pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// The largest key document read. Entra's is about 15 KiB.
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// What has been fetched.
#[derive(Default)]
struct Cache {
    /// kid to DER `RSAPublicKey`.
    keys: HashMap<String, Vec<u8>>,
    /// When the set was last fetched successfully.
    fetched_at: Option<Instant>,
}

/// The production [`SigningKeys`]: a tenant's published key set.
pub struct JwksKeys {
    /// `https://login.microsoftonline.com/{tenant}/discovery/v2.0/keys`.
    url: String,
    /// Built on first use, so constructing the service never touches the
    /// trust store.
    tls: OnceLock<Result<Arc<ClientConfig>, ()>>,
    /// The cache.
    cache: Mutex<Cache>,
}

impl std::fmt::Debug for JwksKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwksKeys")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl JwksKeys {
    /// The key set of `tenant_id`.
    #[must_use]
    pub fn for_tenant(tenant_id: &str) -> Self {
        Self::at(format!(
            "https://login.microsoftonline.com/{tenant_id}/discovery/v2.0/keys"
        ))
    }

    /// The key set at `url` (`http` or `https`), for tests.
    #[must_use]
    pub fn at(url: String) -> Self {
        Self {
            url,
            tls: OnceLock::new(),
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Fetch and parse the set.
    fn fetch(&self) -> Result<HashMap<String, Vec<u8>>, KeysError> {
        let body = self.get().map_err(|reason| {
            // The reason is a fixed phrase: no URL, header or body.
            tracing::error!(reason, "Could not fetch the token signing keys");
            KeysError::Unavailable
        })?;
        let document: Value = serde_json::from_slice(&body).map_err(|_| KeysError::Unavailable)?;
        let mut keys = HashMap::new();
        for jwk in document
            .get("keys")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let field = |name: &str| {
                jwk.get(name)
                    .and_then(Value::as_str)
                    .and_then(|v| URL_SAFE_NO_PAD.decode(v.trim_end_matches('=')).ok())
            };
            if jwk.get("kty").and_then(Value::as_str) != Some("RSA") {
                continue;
            }
            if let (Some(kid), Some(n), Some(e)) = (
                jwk.get("kid").and_then(Value::as_str),
                field("n"),
                field("e"),
            ) {
                keys.insert(kid.to_owned(), rsa_public_key_der(&n, &e));
            }
        }
        Ok(keys)
    }

    /// One `GET`, returning the body of a `200`.
    fn get(&self) -> Result<Vec<u8>, &'static str> {
        let (https, rest) = match self.url.split_once("://") {
            Some(("https", rest)) => (true, rest),
            Some(("http", rest)) => (false, rest),
            _ => return Err("the key URL is not http or https"),
        };
        let (authority, path) = rest
            .split_once('/')
            .map_or_else(|| (rest, "/".to_owned()), |(a, p)| (a, format!("/{p}")));
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => (
                host,
                port.parse::<u16>().map_err(|_| "the port is invalid")?,
            ),
            _ => (authority, if https { 443 } else { 80 }),
        };
        let address = (host, port)
            .to_socket_addrs()
            .map_err(|_| "the key host could not be resolved")?
            .next()
            .ok_or("the key host has no address")?;
        let stream = TcpStream::connect_timeout(&address, FETCH_TIMEOUT)
            .map_err(|_| "could not connect to the key host")?;
        stream
            .set_read_timeout(Some(FETCH_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(FETCH_TIMEOUT)))
            .map_err(|_| "could not set a deadline")?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\n\
             Connection: close\r\n\r\n"
        );
        let raw = if https {
            let tls = self
                .tls
                .get_or_init(|| ihb_tls::client_config().map(Arc::new).map_err(|_| ()))
                .as_ref()
                .map_err(|()| "the trust store is unusable")?;
            let name =
                ServerName::try_from(host.to_owned()).map_err(|_| "the host is not a TLS name")?;
            let connection =
                ClientConnection::new(Arc::clone(tls), name).map_err(|_| "could not start TLS")?;
            exchange(StreamOwned::new(connection, stream), &request)?
        } else {
            exchange(stream, &request)?
        };
        split_ok_body(&raw)
    }
}

/// Send `request` and read to the close, bounded.
fn exchange<S: Read + Write>(mut stream: S, request: &str) -> Result<Vec<u8>, &'static str> {
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "the key request could not be sent")?;
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() > MAX_DOCUMENT_BYTES {
                    return Err("the key document is too large");
                }
            }
            // A TLS peer closing without `close_notify`; `Connection: close`
            // means the body is complete, and parsing it checks that.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => return Err("the key document could not be read"),
        }
    }
    Ok(raw)
}

/// The body of a `200`, de-chunked.
fn split_ok_body(raw: &[u8]) -> Result<Vec<u8>, &'static str> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("the key answer was incomplete")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines.next().and_then(|l| l.split(' ').nth(1));
    if status != Some("200") {
        return Err("the key host did not answer 200");
    }
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let body = &raw[split + 4..];
    if !chunked {
        return Ok(body.to_vec());
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("the chunked answer was malformed")?;
        let size = std::str::from_utf8(&rest[..end])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok())
            .ok_or("the chunked answer was malformed")?;
        rest = &rest[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size + 2 {
            return Err("the chunked answer was truncated");
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

impl SigningKeys for JwksKeys {
    fn signing_key(&self, kid: &str) -> Result<Vec<u8>, KeysError> {
        // A poisoned lock means a fetch panicked; the cache is plain data,
        // so carry on with what is in it.
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = cache
            .fetched_at
            .is_some_and(|at| at.elapsed() < CACHE_LIFESPAN);
        if fresh && let Some(key) = cache.keys.get(kid) {
            return Ok(key.clone());
        }
        // Not cached, or stale. Refresh unless we fetched a moment ago.
        let recently = cache
            .fetched_at
            .is_some_and(|at| at.elapsed() < MIN_REFRESH_INTERVAL);
        if recently {
            return cache.keys.get(kid).cloned().ok_or(KeysError::Unknown);
        }
        // Stamp only a success: a failed fetch must be retried by the next
        // token (and answer 503), not hidden behind the rate limit as a 401.
        cache.keys = self.fetch()?;
        cache.fetched_at = Some(Instant::now());
        cache.keys.get(kid).cloned().ok_or(KeysError::Unknown)
    }
}
