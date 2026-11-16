//! One HTTP request, over plain TCP or TLS, behind a trait.
//!
//! The trait exists so the retry and polling policy of [`crate::client`] is
//! tested without a socket (`spec/10-senders.md` §8.8). [`HyperTransport`] is
//! the real one: a fresh connection per request, no redirects followed, and a
//! cap on how much of a response is read.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::header::HOST;
use hyper::{Request as HyperRequest, Uri};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::pki_types::ServerName;

/// The most of a response that is read. An `OperationOutcome` is small; a
/// peer that sends more is not one.
pub const MAX_RESPONSE_BYTES: usize = 1_048_576;

/// The request methods this service uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The status poll.
    Get,
    /// The Bundle and the token request.
    Post,
}

impl Method {
    /// The wire name, also used in log lines.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// A request to send.
#[derive(Debug, Clone)]
pub struct Request {
    /// The method.
    pub method: Method,
    /// An absolute `http` or `https` URL.
    pub url: String,
    /// Header names and values, in order.
    pub headers: Vec<(String, String)>,
    /// The body, for a `POST`.
    pub body: Option<Vec<u8>>,
    /// Bounds the whole exchange: connect, TLS, request and response.
    pub timeout: Duration,
}

/// A response, fully read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// Header names (lower-cased) and values.
    pub headers: Vec<(String, String)>,
    /// The body, at most [`MAX_RESPONSE_BYTES`].
    pub body: Vec<u8>,
}

impl Response {
    /// A header by case-insensitive name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body as text, lossily.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as JSON, or `None` when it is not.
    #[must_use]
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// Anything that stopped a request producing a response.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(pub String);

/// Something that can send a [`Request`].
pub trait Transport: Send {
    /// Send it and read the whole response.
    ///
    /// # Errors
    ///
    /// [`TransportError`] for a bad URL, a refused connection, TLS or HTTP
    /// failure, an oversized response or the timeout.
    fn send(
        &mut self,
        request: Request,
    ) -> impl Future<Output = Result<Response, TransportError>> + Send;
}

/// The real transport.
#[derive(Clone)]
pub struct HyperTransport {
    /// Verifies `https` peers against the workspace roots.
    tls: TlsConnector,
}

impl std::fmt::Debug for HyperTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HyperTransport").finish_non_exhaustive()
    }
}

impl HyperTransport {
    /// A transport that verifies `https` peers against the workspace roots.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when the trust roots cannot be built.
    pub fn new() -> Result<Self, TransportError> {
        let roots = ihb_tls::root_store().map_err(|error| TransportError(error.to_string()))?;
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            tls: TlsConnector::from(Arc::new(config)),
        })
    }
}

impl Transport for HyperTransport {
    async fn send(&mut self, request: Request) -> Result<Response, TransportError> {
        let timeout = request.timeout;
        tokio::time::timeout(timeout, self.exchange(request))
            .await
            .map_err(|_| TransportError(format!("timed out after {:.1}s", timeout.as_secs_f64())))?
    }
}

impl HyperTransport {
    /// Connect, send and read, with no timeout of its own.
    async fn exchange(&self, request: Request) -> Result<Response, TransportError> {
        let fail = |error: &dyn std::fmt::Display| TransportError(error.to_string());

        let uri: Uri = request
            .url
            .parse()
            .map_err(|error| TransportError(format!("the URL is invalid: {error}")))?;
        let host = uri
            .host()
            .ok_or_else(|| TransportError("the URL names no host".to_string()))?
            .to_string();
        let secure = match uri.scheme_str() {
            Some("https") => true,
            Some("http") => false,
            _ => return Err(TransportError("the URL is not http or https".to_string())),
        };
        let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });

        let mut builder = HyperRequest::builder()
            .method(request.method.as_str())
            .uri(uri.path_and_query().map_or("/", |p| p.as_str()))
            .header(HOST, authority_header(&host, uri.port_u16()));
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let hyper_request = builder
            .body(Full::new(Bytes::from(request.body.unwrap_or_default())))
            .map_err(|error| fail(&error))?;

        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| fail(&error))?;
        if secure {
            let name = ServerName::try_from(host)
                .map_err(|error| TransportError(format!("invalid server name: {error}")))?;
            let stream = self
                .tls
                .connect(name, tcp)
                .await
                .map_err(|error| fail(&error))?;
            exchange(stream, hyper_request).await
        } else {
            exchange(tcp, hyper_request).await
        }
    }
}

/// `Host` carries the port only when the URL did.
fn authority_header(host: &str, port: Option<u16>) -> String {
    port.map_or_else(|| host.to_string(), |port| format!("{host}:{port}"))
}

/// Send `request` over `io` and read the whole response.
async fn exchange<I>(io: I, request: HyperRequest<Full<Bytes>>) -> Result<Response, TransportError>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let fail = |error: &dyn std::fmt::Display| TransportError(error.to_string());
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|error| fail(&error))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let response = sender
        .send_request(request)
        .await
        .map_err(|error| fail(&error))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect();
    let body = Limited::new(response.into_body(), MAX_RESPONSE_BYTES)
        .collect()
        .await
        .map_err(|error| TransportError(format!("could not read the response: {error}")))?
        .to_bytes()
        .to_vec();

    Ok(Response {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_is_found_whatever_its_case() {
        let response = Response {
            status: 202,
            headers: vec![("content-location".into(), "/status/1".into())],
            body: vec![],
        };
        assert_eq!(response.header("Content-Location"), Some("/status/1"));
        assert_eq!(response.header("Retry-After"), None);
    }

    #[test]
    fn the_host_header_carries_the_port_only_when_the_url_did() {
        assert_eq!(authority_header("example.org", None), "example.org");
        assert_eq!(authority_header("localhost", Some(8080)), "localhost:8080");
    }

    #[test]
    fn a_body_that_is_not_json_is_none() {
        let response = Response {
            status: 200,
            headers: vec![],
            body: b"not json".to_vec(),
        };
        assert!(response.json().is_none());
        assert_eq!(response.text(), "not json");
    }
}
