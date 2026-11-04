//! The HTTP client that posts an envelope.
//!
//! Implements `spec/10-senders.md` §4.1 and §4.3.

use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use ihb_config::Secret;
use ihb_sender::{Delivery, DeliveryError, Verdict};
use tokio::net::TcpStream;

use crate::envelope::{Format, UNPARSEABLE_LOG_LIMIT, evaluate};

/// The content type §4.1 requires.
pub const CONTENT_TYPE_VALUE: &str = "text/xml; charset=utf-8";

/// Where to post, and how.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// `SOAP_ENDPOINT_URL`.
    pub url: String,
    /// `SOAP_TIMEOUT_SECONDS`.
    pub timeout: Duration,
    /// `SOAP_API_KEY`, sent as `Authorization: ApiKey {key}`.
    pub api_key: Option<Secret>,
}

/// A SOAP client.
///
/// # No connection reuse, unlike MLLP
///
/// `spec/10-senders.md` §4.3 retries with a **fresh client**, and the
/// Python opened one per request. HTTP endpoints behind a load balancer
/// expect that; the long-lived connection of §3.1 is an MLLP convention,
/// not an HTTP one.
#[derive(Debug)]
pub struct SoapClient {
    /// Where it posts.
    endpoint: Endpoint,
    /// Which envelope (`spec/10-senders.md` §4.6).
    format: Format,
}

impl SoapClient {
    /// A client for `endpoint`.
    #[must_use]
    pub const fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            format: Format::Default,
        }
    }

    /// Use another envelope (`spec/10-senders.md` §4.6).
    #[must_use]
    pub const fn with_format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    /// Where this posts.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Post one envelope and read the response.
    async fn post(&self, body: String) -> Result<(u16, String), Attempt> {
        let uri: Uri = self
            .endpoint
            .url
            .parse()
            .map_err(|error| Attempt::Connection(format!("{error}")))?;

        let host = uri
            .host()
            .ok_or_else(|| Attempt::Connection("the endpoint URL names no host".to_string()))?
            .to_string();
        let port = uri.port_u16().unwrap_or(80);

        if uri.scheme_str() == Some("https") {
            // TLS is not wired up here yet: `SOAP_CLIENT_CERT_PATH` is
            // `spec/02-configuration.md` §6.3's mTLS option and needs the
            // certificate loading of `spec/08-hl7-soap-server.md` §7 in the
            // other direction. Failing loudly beats posting a patient
            // record in plaintext to a URL that asked for TLS.
            return Err(Attempt::Connection(
                "https endpoints are not yet supported by this sender".to_string(),
            ));
        }

        let stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| Attempt::Connection(error.to_string()))?;

        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|error| Attempt::Connection(error.to_string()))?;

        // The connection drives itself on its own task; dropping the join
        // handle is fine because the response is awaited below.
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let mut builder = Request::builder()
            .method("POST")
            .uri(&self.endpoint.url)
            .header(CONTENT_TYPE, CONTENT_TYPE_VALUE)
            .header(hyper::header::HOST, host);

        // §4.1 — the key is a secret and never reaches a log line; only its
        // presence does.
        if let Some(key) = &self.endpoint.api_key {
            builder = builder.header(AUTHORIZATION, format!("ApiKey {}", key.expose()));
        }

        let request = builder
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| Attempt::Connection(error.to_string()))?;

        let response =
            match tokio::time::timeout(self.endpoint.timeout, sender.send_request(request)).await {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => return Err(Attempt::Connection(error.to_string())),
                Err(_) => return Err(Attempt::Timeout),
            };

        let status = response.status().as_u16();
        let collected =
            match tokio::time::timeout(self.endpoint.timeout, response.into_body().collect()).await
            {
                Ok(Ok(collected)) => collected.to_bytes(),
                Ok(Err(error)) => return Err(Attempt::Connection(error.to_string())),
                Err(_) => return Err(Attempt::Timeout),
            };

        Ok((status, String::from_utf8_lossy(&collected).into_owned()))
    }
}

/// What one attempt produced.
#[derive(Debug)]
enum Attempt {
    /// No response within the timeout. Retryable once (§4.3).
    Timeout,
    /// A connection failure. Not retried (§4.3).
    Connection(String),
}

impl Delivery for SoapClient {
    /// §4.2, §4.3 and §4.4.
    async fn deliver(&mut self, message: &str) -> Result<Verdict, DeliveryError> {
        // §4.6.3 — a message that cannot be wrapped is not posted.
        let body = self
            .format
            .wrap(message)
            .map_err(DeliveryError::Unexpected)?;

        match self.post(body.clone()).await {
            Ok((status, response)) => return Ok(read(status, &response)),

            Err(Attempt::Connection(reason)) => {
                // §4.3 — a connection error fails immediately, without a
                // retry. The endpoint is not there; trying again in the
                // same millisecond will not change that, and Service Bus
                // will redeliver with backoff.
                return Err(DeliveryError::Connection(format!(
                    "SOAP connection error: {reason}"
                )));
            }

            Err(Attempt::Timeout) => {
                tracing::warn!(
                    seconds = self.endpoint.timeout.as_secs(),
                    "no SOAP response within the timeout; retrying once with a fresh client"
                );
            }
        }

        match self.post(body).await {
            Ok((status, response)) => Ok(read(status, &response)),
            Err(Attempt::Timeout) => Err(DeliveryError::Connection(format!(
                // §4.3's exact wording.
                "No SOAP response within {} seconds",
                self.endpoint.timeout.as_secs()
            ))),
            Err(Attempt::Connection(reason)) => Err(DeliveryError::Connection(format!(
                "SOAP connection error: {reason}"
            ))),
        }
    }
}

/// Evaluate a response, logging what §4.4.2 allows.
fn read(status: u16, body: &str) -> Verdict {
    let verdict = evaluate(status, body);

    if !verdict.is_accepted() {
        // §4.4.2 — bounded, and the event logger redacts it. A response
        // body can echo the message that produced it.
        tracing::warn!(
            status,
            body = %body.chars().take(UNPARSEABLE_LOG_LIMIT).collect::<String>(),
            "the SOAP endpoint did not accept the message"
        );
    }

    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_carries_its_key_as_a_secret() {
        // `spec/04-observability.md` §2 — an API key must not reach a log
        // line, and `Secret` is what stops it.
        let endpoint = Endpoint {
            url: "http://mpi.example/soap".to_string(),
            timeout: Duration::from_secs(30),
            api_key: Some(Secret::new("super-secret")),
        };

        let rendered = format!("{endpoint:?}");
        assert!(!rendered.contains("super-secret"), "{rendered}");
    }
}
