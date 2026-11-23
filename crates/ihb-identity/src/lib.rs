//! A managed-identity access token for Azure SQL.
//!
//! Implements `spec/12-message-store.md` §5.4.1 and §5.4.2, shared by
//! `ihb-sql` and `db-sender` so the two cannot disagree.
//!
//! # What this does and does not do
//!
//! The request shapes, the scope-to-resource conversion, the two spellings of
//! `expires_in` and the per-scope cache all live in [`azure_token_lite`]. This
//! crate adds the one thing that crate leaves out on purpose, the transport,
//! and keeps it as small as the job allows.
//!
//! The identity endpoint (`IDENTITY_ENDPOINT`) is a plain-HTTP server on the
//! container's own loopback interface: App Service and Container Apps both
//! publish `http://localhost:<port>/...`. So the transport is a single
//! `HTTP/1.0` `GET` over a [`TcpStream`], and it
//! **refuses any host that is not loopback** (§5.4.3): the request carries
//! `IDENTITY_HEADER`, a secret, and the response carries a bearer token, and
//! neither belongs on a network in the clear. `HTTP/1.0` rather than 1.1 so the
//! server closes the connection and never answers in chunks.
//!
//! IMDS (`169.254.169.254`) is not used, for the reason `azure-token-lite`
//! gives: off Azure it hangs rather than fails.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable
    )
)]

use std::time::Duration;

use azure_token_lite::{Credential, Method, TokenError, TokenRequest, TokenSource};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The scope for Azure SQL, Azure Synapse and Fabric databases.
pub const SQL_SCOPE: &str = "https://database.windows.net/.default";

/// How long the identity endpoint has to answer. It is on loopback, so
/// anything slower than this is a fault, not latency.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest response read. A token is a few kilobytes.
const MAX_RESPONSE: usize = 64 * 1024;

/// Why a token could not be obtained. Never carries a token or the
/// identity-endpoint secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    /// The environment provides no managed identity, or its answer was not a
    /// token.
    #[error("{0}")]
    Credential(String),

    /// The identity endpoint could not be reached or answered with an error.
    #[error("the identity endpoint failed: {0}")]
    Endpoint(String),
}

/// A managed identity, and the tokens it has fetched.
///
/// One per process is enough: the cache is keyed by scope and shared by
/// every connection (§5.4.2).
#[derive(Debug)]
pub struct ManagedIdentity {
    /// The credential and its cache.
    source: TokenSource,
}

impl ManagedIdentity {
    /// The system-assigned identity, or the user-assigned one named by
    /// `client_id` (`MANAGED_IDENTITY_CLIENT_ID`).
    #[must_use]
    pub fn new(client_id: Option<String>) -> Self {
        Self {
            source: TokenSource::new(Credential::ManagedIdentity { client_id }),
        }
    }

    /// A usable access token for [`SQL_SCOPE`], from the cache when there is
    /// one that is not about to expire.
    ///
    /// # Errors
    ///
    /// [`IdentityError`]. Without `IDENTITY_ENDPOINT` this fails at once,
    /// naming the variable, rather than waiting on IMDS.
    pub async fn sql_token(&self) -> Result<String, IdentityError> {
        self.token(SQL_SCOPE).await
    }

    /// A usable access token for `scope`.
    ///
    /// # Errors
    ///
    /// As [`Self::sql_token`].
    pub async fn token(&self, scope: &str) -> Result<String, IdentityError> {
        self.source
            .token(scope, fetch)
            .await
            .map_err(|error| match error {
                TokenError::Token(error) => IdentityError::Credential(error.to_string()),
                TokenError::Transport(error) => error,
            })
    }
}

/// Perform one token request against the identity endpoint.
async fn fetch(request: TokenRequest) -> Result<String, IdentityError> {
    if !matches!(request.method, Method::Get) {
        return Err(IdentityError::Endpoint(
            "only the identity endpoint's GET is supported".to_string(),
        ));
    }
    let target = Target::parse(&request.url)?;

    tokio::time::timeout(REQUEST_TIMEOUT, exchange(&target, &request.headers))
        .await
        .map_err(|_| {
            IdentityError::Endpoint(format!(
                "no answer from {} within {} s",
                target.host,
                REQUEST_TIMEOUT.as_secs()
            ))
        })?
}

/// Where to send the request.
#[derive(Debug, PartialEq, Eq)]
struct Target {
    /// The host, loopback only.
    host: String,
    /// The port.
    port: u16,
    /// The path and query.
    path: String,
}

impl Target {
    /// Split `http://host[:port]/path?query`, refusing anything that is not
    /// plain HTTP to a loopback host (§5.4.3).
    fn parse(url: &str) -> Result<Self, IdentityError> {
        let rest = url.strip_prefix("http://").ok_or_else(|| {
            IdentityError::Endpoint(
                "the identity endpoint must be plain http on loopback (spec/12 §5.4.3)".to_string(),
            )
        })?;
        let (authority, path) = rest.find('/').map_or((rest, "/"), |at| rest.split_at(at));
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') || authority.contains(']') => {
                let port = port.parse::<u16>().map_err(|_| {
                    IdentityError::Endpoint(
                        "the identity endpoint's port is not a number".to_string(),
                    )
                })?;
                (host, port)
            }
            _ => (authority, 80),
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');

        if !matches!(host, "localhost" | "127.0.0.1" | "::1") {
            return Err(IdentityError::Endpoint(format!(
                "{host} is not a loopback address; the identity endpoint carries a secret and \
                 a bearer token and is never reached over a network (spec/12 §5.4.3)"
            )));
        }

        Ok(Self {
            host: host.to_string(),
            port,
            path: path.to_string(),
        })
    }
}

/// Send the request and return the body of a `200`.
async fn exchange(target: &Target, headers: &[(String, String)]) -> Result<String, IdentityError> {
    let mut stream = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .map_err(|error| IdentityError::Endpoint(error.to_string()))?;

    let mut request = format!(
        "GET {} HTTP/1.0\r\nHost: {}:{}\r\nAccept: application/json\r\n",
        target.path, target.host, target.port
    );
    for (name, value) in headers {
        // A header value with a line break would be a second header.
        if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
            return Err(IdentityError::Endpoint(
                "a header contained a line break".to_string(),
            ));
        }
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| IdentityError::Endpoint(error.to_string()))?;

    let mut raw = Vec::new();
    let limit =
        u64::try_from(MAX_RESPONSE).map_err(|error| IdentityError::Endpoint(error.to_string()))?;
    stream
        .take(limit)
        .read_to_end(&mut raw)
        .await
        .map_err(|error| IdentityError::Endpoint(error.to_string()))?;

    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| IdentityError::Endpoint("the response had no header block".to_string()))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("?");

    if status != "200" {
        // The body of an error can echo the request; report the status only.
        return Err(IdentityError::Endpoint(format!(
            "the identity endpoint answered {status}"
        )));
    }

    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::*;

    /// Serve one canned response and report the request that arrived.
    async fn serve(response: &'static str) -> (u16, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&buffer[..read]).to_string()
        });
        (port, handle)
    }

    fn get(url: String) -> TokenRequest {
        TokenRequest {
            method: Method::Get,
            url,
            headers: vec![("X-IDENTITY-HEADER".to_string(), "s3cret".to_string())],
            body: None,
        }
    }

    #[test]
    fn only_loopback_http_is_accepted() {
        // spec/12 §5.4.3.
        for ok in [
            "http://localhost:42356/msi/token?x=1",
            "http://127.0.0.1:8081/",
            "http://[::1]:9/t",
        ] {
            assert!(Target::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "https://localhost/t",
            "http://169.254.169.254/metadata/identity/oauth2/token",
            "http://example.com/t",
            "http://localhost.evil.example/t",
        ] {
            assert!(Target::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_port_and_path_are_split_out() {
        let target =
            Target::parse("http://localhost:42356/msi/token?api-version=2019-08-01").unwrap();
        assert_eq!(target.port, 42356);
        assert_eq!(target.path, "/msi/token?api-version=2019-08-01");
    }

    #[tokio::test]
    async fn a_token_is_read_and_the_secret_header_is_sent() {
        let (port, seen) = serve(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"access_token\":\"abc\",\"expires_in\":\"3600\"}",
        )
        .await;

        let body = fetch(get(format!("http://127.0.0.1:{port}/msi/token")))
            .await
            .unwrap();

        assert!(body.contains("abc"));
        let request = seen.await.unwrap();
        assert!(
            request.starts_with("GET /msi/token HTTP/1.0\r\n"),
            "{request}"
        );
        assert!(request.contains("X-IDENTITY-HEADER: s3cret"), "{request}");
    }

    #[tokio::test]
    async fn an_error_status_is_reported_without_its_body() {
        let (port, _) = serve("HTTP/1.0 400 Bad Request\r\n\r\n{\"echo\":\"s3cret\"}").await;

        let error = fetch(get(format!("http://127.0.0.1:{port}/t")))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("400"), "{error}");
        assert!(!error.to_string().contains("s3cret"), "{error}");
    }

    #[tokio::test]
    async fn the_cache_serves_the_second_call() {
        // §5.4.2: a connection rebuilt per §5.2 must not hit the endpoint
        // every time. A client-secret credential builds its request without
        // the identity environment, which is all this needs.
        let source = TokenSource::new(Credential::client_secret("t", "c", "s"));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let body = || async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, IdentityError>("{\"access_token\":\"abc\",\"expires_in\":3600}".to_string())
        };

        let first = source.token(SQL_SCOPE, |_| body()).await.unwrap();
        let second = source.token(SQL_SCOPE, |_| body()).await.unwrap();

        assert_eq!((first.as_str(), second.as_str()), ("abc", "abc"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn without_an_identity_endpoint_it_fails_at_once_naming_the_variable() {
        // Not asserted when the variable is set: removing it is unsafe under
        // parallel tests, and a laptop and CI both lack it.
        if std::env::var("IDENTITY_ENDPOINT").is_ok() {
            return;
        }
        let started = std::time::Instant::now();

        let error = ManagedIdentity::new(None).sql_token().await.unwrap_err();

        assert!(error.to_string().contains("IDENTITY_ENDPOINT"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
