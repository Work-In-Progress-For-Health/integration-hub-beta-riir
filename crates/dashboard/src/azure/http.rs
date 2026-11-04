//! A small HTTPS client for the Azure REST APIs.
//!
//! Implements `spec/15-dashboard.md` §4.6 and §4.7.
//!
//! # Why this exists rather than a client library
//!
//! Three requests, all of them "GET or POST JSON with a bearer token and a
//! deadline". A general-purpose HTTP client would bring its own TLS stack
//! alongside the `tokio-rustls` one this workspace already has, and
//! `deny.toml` counts duplicate stacks as something to justify rather than
//! collect.
//!
//! # Everything has a deadline
//!
//! §2.2.2 gives every route a timeout even when Azure is unreachable. A
//! request with no deadline is how a page hangs, and a hung operations
//! dashboard is worse than one reporting unknown — nobody can tell it is
//! broken.

use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST};
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::pki_types::ServerName;

/// How long any single request may take.
///
/// Chosen against §2.2.2: a page fans out to several of these
/// concurrently, so this is roughly the worst case for a cold render, and
/// a whole minute of it would be a blank page for a minute.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a request did not produce a body.
///
/// Every variant ends the same way — §4.2 turns it into an empty result —
/// but they are distinguished because the **log line** is the only place an
/// operator finds out why a panel is blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// The URL could not be parsed, or names no host.
    Url(String),
    /// The connection or the TLS handshake failed.
    Connect(String),
    /// The request was sent and no answer arrived in time.
    Timeout,
    /// The server answered with a status outside 2xx.
    Status {
        /// The status.
        code: u16,
        /// As much of the body as is worth logging.
        body: String,
    },
    /// The answer arrived and was not what was expected.
    Body(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(reason) => write!(f, "the URL is not usable: {reason}"),
            Self::Connect(reason) => write!(f, "could not connect: {reason}"),
            Self::Timeout => write!(f, "no answer within {REQUEST_TIMEOUT:?}"),
            Self::Status { code, body } => write!(f, "the service answered {code}: {body}"),
            Self::Body(reason) => write!(f, "the answer could not be read: {reason}"),
        }
    }
}

/// How much of an error body reaches a log line.
///
/// An Azure error body is a short JSON document, but a misrouted request
/// can return an HTML error page, and a whole one in a log line helps
/// nobody.
const ERROR_BODY_LIMIT: usize = 512;

/// A client that speaks HTTPS to one set of hosts.
///
/// The TLS configuration is built once and shared: assembling the root
/// store per request is measurable, and every request here goes to Azure.
#[derive(Clone)]
pub struct HttpsClient {
    /// The shared TLS configuration (§4.7).
    tls: Arc<ClientConfig>,
}

impl std::fmt::Debug for HttpsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsClient").finish_non_exhaustive()
    }
}

impl HttpsClient {
    /// A client trusting the Mozilla root store, plus the corporate CA
    /// bundle if one is configured (§4.7, `spec/18-deployment.md` §4.1).
    ///
    /// Certificate verification is not configurable here, deliberately.
    /// The one place this specification allows it to be turned off is the
    /// local Cosmos emulator (§5.6.1), and that is a separate decision made
    /// in the configuration rather than a knob on the transport.
    ///
    /// # Errors
    ///
    /// [`ihb_tls::TrustError`] if `SSL_CERT_FILE` names a bundle that
    /// cannot be read or contains no usable certificate. This is fallible
    /// rather than falling back to the default roots, because
    /// `spec/18-deployment.md` §4.3 says exactly that: continuing with the
    /// defaults turns a certificate problem into a handshake failure
    /// somewhere else entirely.
    ///
    /// With no bundle configured — the laptop case of §1.2 — this cannot
    /// fail.
    pub fn new() -> Result<Self, ihb_tls::TrustError> {
        let tls = ClientConfig::builder()
            .with_root_certificates(ihb_tls::root_store()?)
            .with_no_client_auth();

        Ok(Self { tls: Arc::new(tls) })
    }

    /// `GET url` with a bearer token, parsed as JSON.
    ///
    /// # Errors
    ///
    /// Any [`HttpError`]. The caller turns it into an empty result (§4.2)
    /// after logging it.
    pub async fn get_json(
        &self,
        url: &str,
        token: Option<&str>,
    ) -> Result<serde_json::Value, HttpError> {
        self.send(Method::Get, url, token, None, &[]).await
    }

    /// `POST url` with a JSON body and a bearer token, parsed as JSON.
    ///
    /// # Errors
    ///
    /// As [`HttpsClient::get_json`].
    pub async fn post_json(
        &self,
        url: &str,
        token: Option<&str>,
        body: String,
    ) -> Result<serde_json::Value, HttpError> {
        self.send(
            Method::Post,
            url,
            token,
            Some(("application/json", body)),
            &[],
        )
        .await
    }

    /// `POST url` with a form body — the AAD token endpoint's shape.
    ///
    /// # Errors
    ///
    /// As [`HttpsClient::get_json`].
    pub async fn post_form(&self, url: &str, body: String) -> Result<serde_json::Value, HttpError> {
        self.send(
            Method::Post,
            url,
            None,
            Some(("application/x-www-form-urlencoded", body)),
            &[],
        )
        .await
    }

    /// `POST url` with a JSON body and extra headers — Cosmos' shape,
    /// which authenticates with its own signed header rather than a bearer
    /// token (`spec/15-dashboard.md` §5.7.1).
    ///
    /// # Errors
    ///
    /// As [`HttpsClient::get_json`].
    pub async fn post_json_with(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: String,
    ) -> Result<serde_json::Value, HttpError> {
        self.send(
            Method::Post,
            url,
            None,
            Some(("application/json", body)),
            headers,
        )
        .await
    }

    /// `POST url` as a Cosmos **query** (`spec/15-dashboard.md` §10.1).
    ///
    /// Cosmos distinguishes a query from a create by the content type
    /// alone: the same URL and verb with `application/json` is an attempt to
    /// create a document named by the body.
    ///
    /// # Errors
    ///
    /// As [`HttpsClient::get_json`].
    pub async fn post_query_with(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: String,
    ) -> Result<serde_json::Value, HttpError> {
        self.send(
            Method::Post,
            url,
            None,
            Some(("application/query+json", body)),
            headers,
        )
        .await
    }

    /// `DELETE url` with extra headers (`spec/15-dashboard.md` §10.1).
    ///
    /// A successful delete answers `204` with no body, which is
    /// [`serde_json::Value::Null`] here rather than a parse error.
    ///
    /// # Errors
    ///
    /// As [`HttpsClient::get_json`].
    pub async fn delete_with(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<serde_json::Value, HttpError> {
        self.send(Method::Delete, url, None, None, headers).await
    }

    /// `GET url` with extra headers — the managed-identity endpoint's shape
    /// (§4.6.2), which authenticates with a header rather than a token.
    ///
    /// # Errors
    ///
    /// As [`HttpsClient::get_json`].
    pub async fn get_json_with(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<serde_json::Value, HttpError> {
        self.send(Method::Get, url, None, None, headers).await
    }

    /// One request, with §2.2.2's deadline over the whole of it.
    async fn send(
        &self,
        method: Method,
        url: &str,
        token: Option<&str>,
        body: Option<(&str, String)>,
        headers: &[(&str, &str)],
    ) -> Result<serde_json::Value, HttpError> {
        // The timeout wraps connect, send and read together. Three separate
        // deadlines would let a slow server take three times as long as any
        // one of them allows.
        match tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.send_inner(method, url, token, body, headers),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(HttpError::Timeout),
        }
    }

    async fn send_inner(
        &self,
        method: Method,
        url: &str,
        token: Option<&str>,
        body: Option<(&str, String)>,
        headers: &[(&str, &str)],
    ) -> Result<serde_json::Value, HttpError> {
        let uri: Uri = url.parse().map_err(|e| HttpError::Url(format!("{e}")))?;
        let host = uri
            .host()
            .ok_or_else(|| HttpError::Url("no host".to_string()))?
            .to_string();

        // §4.7 — HTTPS only. A plaintext URL here would send a bearer token
        // for the whole subscription over the network in the clear, so this
        // is a refusal rather than a downgrade.
        if uri.scheme_str() != Some("https") {
            return Err(HttpError::Url(format!(
                "{host} was reached over {}; Azure endpoints must be https \
                 (spec/15-dashboard.md §4.7)",
                uri.scheme_str().unwrap_or("no scheme")
            )));
        }

        let port = uri.port_u16().unwrap_or(443);

        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| HttpError::Connect(e.to_string()))?;

        let server_name = ServerName::try_from(host.clone())
            .map_err(|e| HttpError::Url(format!("{host} is not a valid server name: {e}")))?;

        let stream = TlsConnector::from(Arc::clone(&self.tls))
            .connect(server_name, tcp)
            .await
            .map_err(|e| HttpError::Connect(format!("TLS: {e}")))?;

        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| HttpError::Connect(e.to_string()))?;

        tokio::spawn(async move {
            let _ = connection.await;
        });

        let mut builder = Request::builder()
            .method(method.as_str())
            .uri(url)
            .header(HOST, host)
            .header(ACCEPT, "application/json");

        if let Some(token) = token {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }

        let request = match body {
            Some((content_type, body)) => builder
                .header(CONTENT_TYPE, content_type)
                .body(Full::new(Bytes::from(body))),
            None => builder.body(Full::new(Bytes::new())),
        }
        .map_err(|e| HttpError::Url(e.to_string()))?;

        let response = sender
            .send_request(request)
            .await
            .map_err(|e| HttpError::Connect(e.to_string()))?;

        let status = response.status().as_u16();
        let collected = response
            .into_body()
            .collect()
            .await
            .map_err(|e| HttpError::Body(e.to_string()))?
            .to_bytes();

        if !(200..300).contains(&status) {
            let body = String::from_utf8_lossy(&collected);
            return Err(HttpError::Status {
                code: status,
                // Truncated on a character boundary: a body that is not
                // UTF-8 must not panic the slice.
                body: body.chars().take(ERROR_BODY_LIMIT).collect(),
            });
        }

        // `204 No Content` is a success with nothing to parse.
        if collected.is_empty() {
            return Ok(serde_json::Value::Null);
        }

        serde_json::from_slice(&collected).map_err(|e| HttpError::Body(e.to_string()))
    }
}

/// The verbs used here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `DELETE`.
    Delete,
}

impl Method {
    /// The verb as HTTP spells it.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_plaintext_url_is_refused_rather_than_downgraded() {
        // spec/15-dashboard.md §4.7 — THE test of this file.
        //
        // These requests carry a bearer token good for the whole
        // subscription. Sending one over plaintext because a URL said
        // `http://` would put it on the network in the clear, so a
        // misconfiguration has to fail rather than proceed.
        let client = HttpsClient::new().expect("no CA bundle is configured in tests");

        let error = client
            .get_json("http://management.azure.com/x", Some("a-token"))
            .await
            .expect_err("plaintext must be refused");

        assert!(matches!(error, HttpError::Url(_)), "{error:?}");
        assert!(error.to_string().contains("must be https"), "{error}");
    }

    #[tokio::test]
    async fn a_url_with_no_host_is_refused() {
        let client = HttpsClient::new().expect("no CA bundle is configured in tests");

        let error = client.get_json("not a url", None).await.unwrap_err();

        assert!(matches!(error, HttpError::Url(_)), "{error:?}");
    }

    #[tokio::test]
    async fn an_unreachable_host_is_a_connect_error_not_a_hang() {
        // §2.2.2 — a page that hangs is worse than one reporting unknown,
        // because nobody can tell it is broken. `.invalid` is reserved by
        // RFC 2606 and never resolves.
        let client = HttpsClient::new().expect("no CA bundle is configured in tests");

        let error = client
            .get_json("https://dashboard-test.invalid/x", None)
            .await
            .unwrap_err();

        assert!(
            matches!(error, HttpError::Connect(_) | HttpError::Timeout),
            "{error:?}"
        );
    }

    #[test]
    fn every_error_says_something_an_operator_can_act_on() {
        // These are the only explanation of why a panel is empty (§4.2
        // swallows the error itself), so each has to name what happened.
        let errors = [
            HttpError::Url("no host".to_string()),
            HttpError::Connect("refused".to_string()),
            HttpError::Timeout,
            HttpError::Status {
                code: 403,
                body: "Forbidden".to_string(),
            },
            HttpError::Body("expected object".to_string()),
        ];

        for error in errors {
            let rendered = error.to_string();
            assert!(!rendered.is_empty(), "{error:?}");
            assert!(
                rendered.len() > "error".len(),
                "{error:?} renders as {rendered:?}, which explains nothing"
            );
        }

        assert!(
            HttpError::Status {
                code: 403,
                body: "Forbidden".to_string()
            }
            .to_string()
            .contains("403"),
            "the status is the first thing to look at"
        );
    }

    #[test]
    fn the_verbs_are_spelled_the_way_http_spells_them() {
        assert_eq!(Method::Get.as_str(), "GET");
        assert_eq!(Method::Post.as_str(), "POST");
        assert_eq!(Method::Delete.as_str(), "DELETE");
    }
}
