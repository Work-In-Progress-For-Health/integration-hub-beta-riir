//! The HTTP endpoints of the HL7 pipeline.
//!
//! Implements `spec/19-rest-servers.md` §1 and §2.
//!
//! [`crate::process`] decides what a message *means*; this module decides how
//! a request is read and how the answer is written. It owns no decision about
//! status codes: a [`Failure`](crate::process::Failure) arrives with its
//! status and NACK, and is rendered.

use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Instant;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use ihb_bus::sink::MessageSink;

use crate::input::to_er7;
use crate::process::{Mint, Processor};
use crate::serve::Handler;

/// The route (`spec/19-rest-servers.md` §1).
pub const MESSAGE_PATH: &str = "/hl7MessageReceiver";

/// `{MESSAGE_PATH}/ping`.
pub const PING_PATH: &str = "/hl7MessageReceiver/ping";

/// `{MESSAGE_PATH}/status`.
pub const STATUS_PATH: &str = "/hl7MessageReceiver/status";

/// `ApiConstants.VERSION`.
const VERSION: &str = "1.0";

/// `ApiConstants.REVISION`.
const REVISION: &str = "0";

/// `ApiConstants.APPLICATION_NAME`.
const APPLICATION_NAME: &str = "HL7 Message Receiver";

/// The `/status` response time above which `timeout` is true, in ms.
const TIMEOUT_THRESHOLD_MS: u128 = 100;

/// `spec/19-rest-servers.md` §2, 400 rows.
const INVALID_BODY: &str = "Invalid or missing request body.";

/// `spec/19-rest-servers.md` §2, 400 rows.
const OVERSIZE: &str = "Message exceeds the maximum allowed size.";

/// The HL7 pipeline, answering HTTP.
pub struct Hl7Http<E, S, M> {
    /// The pipeline.
    processor: Processor<E, S, M>,
    /// `ENVIRONMENT`, for `/ping`.
    environment: String,
    /// The size limit of `spec/19-rest-servers.md` §2.1.
    max_message_size: usize,
    /// The timestamps `/status` reports: the previous request, and the
    /// previous one that did not time out.
    seen: Mutex<Seen>,
}

/// What `/status` remembers.
#[derive(Default)]
struct Seen {
    /// The previous request.
    last_requested: Option<String>,
    /// The previous request that did not time out.
    last_healthy: Option<String>,
}

impl<E: MessageSink, S: MessageSink, M: Mint> Hl7Http<E, S, M> {
    /// Wrap a processor. `max_message_size` is in bytes of the UTF-8
    /// `messageContent`, not of the whole request (`spec/19-rest-servers.md`
    /// §2.1).
    pub fn new(
        processor: Processor<E, S, M>,
        environment: String,
        max_message_size: usize,
    ) -> Self {
        Self {
            processor,
            environment,
            max_message_size,
            seen: Mutex::new(Seen::default()),
        }
    }

    /// Answer one request: route on method and path, as `FastAPI` did.
    ///
    /// A known path with the wrong method is `405`, anything else `404`,
    /// each with upstream's `{"detail":…}` body. `peer` is only for the
    /// `/ping` and `/status` replies.
    pub async fn route(
        &self,
        request: Request<Incoming>,
        peer: SocketAddr,
    ) -> Response<Full<Bytes>> {
        let path = request.uri().path().to_string();
        let method = request.method().clone();

        match (path.as_str(), &method) {
            (MESSAGE_PATH, &Method::POST) => self.post(request).await,
            (PING_PATH, &Method::GET) => self.ping(peer),
            (STATUS_PATH, &Method::GET) => self.status(peer),
            (MESSAGE_PATH | PING_PATH | STATUS_PATH, _) => {
                detail(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed")
            }
            _ => detail(StatusCode::NOT_FOUND, "Not Found"),
        }
    }

    /// `spec/19-rest-servers.md` §2.
    async fn post(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        let limit = self.max_message_size;

        // §2.1 — bounded, because JSON can expand a control character
        // sixfold and the limit applies to the decoded content.
        let cap = limit.saturating_mul(6).saturating_add(1024);
        let body = match Limited::new(request.into_body(), cap).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(error) => {
                tracing::warn!(%error, limit, "Rejected oversize message");
                return envelope(400, OVERSIZE);
            }
        };

        let Some(content) = message_content(&body) else {
            return envelope(400, INVALID_BODY);
        };

        if content.len() > limit {
            tracing::warn!(limit, "Rejected oversize message");
            return envelope(400, OVERSIZE);
        }

        // §2.2 — line endings, then v2.xml to ER7. A failure here is an
        // unparsable message, a 500, not a 400.
        let er7 = match to_er7(&content) {
            Ok(er7) => er7,
            Err(error) => {
                tracing::error!(%error, "Failed to convert inbound message to ER7");
                return envelope(500, &self.processor.generic_nack(&error.to_string()));
            }
        };

        match self.processor.process(&er7).await {
            Ok(acknowledgement) => text(StatusCode::CREATED, acknowledgement),
            Err(failure) => {
                tracing::info!(status = failure.status, "Message was not forwarded");
                envelope(failure.status, &failure.nack)
            }
        }
    }

    /// `spec/19-rest-servers.md` §1.2.
    fn ping(&self, peer: SocketAddr) -> Response<Full<Bytes>> {
        json(
            StatusCode::OK,
            &format!(
                "{{\"environment\":{},\"revision\":\"{REVISION}\",\"version\":\"{VERSION}\",\"clientIp\":{}}}",
                quote(&self.environment),
                quote(&peer.ip().to_string()),
            ),
        )
    }

    /// `spec/19-rest-servers.md` §1.3.
    fn status(&self, peer: SocketAddr) -> Response<Full<Bytes>> {
        let started = Instant::now();
        // The Python calls `ping` here, to prove the handler works.
        let _ = self.ping(peer);
        let response_ms = started.elapsed().as_millis();
        let timed_out = response_ms > TIMEOUT_THRESHOLD_MS;

        let now = ihb_telemetry::clock::Clock::now_iso8601(&ihb_telemetry::clock::SystemClock);
        let (last_requested, last_healthy) = {
            let mut seen = self
                .seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let last_requested = seen.last_requested.replace(now.clone());
            // A request that did not time out reports itself as the last
            // healthy one; one that did reports the one before.
            if !timed_out {
                seen.last_healthy = Some(now);
            }
            (last_requested, seen.last_healthy.clone())
        };

        json(
            StatusCode::OK,
            &format!(
                "{{\"application\":{},\"description\":{},\"status\":200,\"timeout\":{timed_out},\
                 \"lastRequested\":{},\"lastHealthy\":{},\"version\":\"{VERSION}\",\"revision\":\"{REVISION}\"}}",
                quote(APPLICATION_NAME),
                quote(&format!(
                    "Version {VERSION}.{REVISION} API is working, response time {response_ms} ms"
                )),
                quote(last_requested.as_deref().unwrap_or("Never")),
                quote(last_healthy.as_deref().unwrap_or("Never")),
            ),
        )
    }
}

impl<E, S, M> Handler for Hl7Http<E, S, M>
where
    E: MessageSink + 'static,
    S: MessageSink + 'static,
    M: Mint + 'static,
{
    async fn handle(&self, request: Request<Incoming>, peer: SocketAddr) -> Response<Full<Bytes>> {
        self.route(request, peer).await
    }
}

/// `messageContent` out of the body, if the body is JSON and has one that is
/// a string (`spec/19-rest-servers.md` §2).
fn message_content(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .get("messageContent")?
        .as_str()
        .map(ToString::to_string)
}

/// A JSON string literal, quotes and escapes included.
///
/// `serde_json` cannot fail on a `&str`; the fallback exists only to avoid
/// an `unwrap` in non-test code.
fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

/// The `{"StatusCode":…,"ErrorMessage":…}` envelope.
///
/// Written by hand so the key order is upstream's; `serde_json::Map` sorts.
#[must_use]
pub fn envelope(status: u16, message: &str) -> Response<Full<Bytes>> {
    let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    json(
        status_code,
        &format!(
            "{{\"StatusCode\":{status},\"ErrorMessage\":{}}}",
            quote(message)
        ),
    )
}

/// `FastAPI`'s own 404 and 405 body.
fn detail(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    json(status, &format!("{{\"detail\":{}}}", quote(message)))
}

/// A JSON response with `body` already serialised.
#[must_use]
pub fn json(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    respond(status, "application/json", body.to_string())
}

/// A `text/plain` response.
fn text(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    respond(status, "text/plain; charset=utf-8", body)
}

/// One response, with an explicit length.
fn respond(status: StatusCode, content_type: &str, body: String) -> Response<Full<Bytes>> {
    let bytes = Bytes::from(body);
    let length = bytes.len();

    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .header(CONTENT_LENGTH, length)
        .body(Full::new(bytes))
        // The builder only fails on an invalid header, and both are
        // constants. A 500 with no body beats a panic on a public endpoint.
        .unwrap_or_else(|_| {
            let mut response = Response::new(Full::new(Bytes::new()));
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_keeps_upstreams_key_order_and_escapes_the_nack() {
        // spec/19 §2 — the NACK has `\r\n` and backslashes in it.
        let response = envelope(422, "MSH|^~\\&|A\r\nMSA|AR|1|x");
        assert_eq!(response.status(), 422);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
    }

    #[test]
    fn only_a_json_object_with_a_string_content_is_a_body() {
        // test_message_route.py::test_missing_body_field_returns_400
        assert_eq!(
            message_content(br#"{"messageContent":"MSH"}"#).as_deref(),
            Some("MSH")
        );
        assert!(message_content(br#"{"wrong":"field"}"#).is_none());
        assert!(message_content(br#"{"messageContent":5}"#).is_none());
        assert!(message_content(b"not json").is_none());
        assert!(message_content(b"").is_none());
    }
}
