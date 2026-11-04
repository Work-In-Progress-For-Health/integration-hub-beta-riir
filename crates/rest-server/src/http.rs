//! The HTTP endpoints of the `generic` pipeline, and the dispatch between the
//! two pipelines.
//!
//! Implements `spec/19-rest-servers.md` §9.1.
//!
//! [`crate::process`] decides what a request *means*; this module decides how
//! it is admitted and how the answer is written. The size check streams the
//! body and stops as soon as it passes the limit, so a request that declares
//! nothing, or lies, or is chunked, still cannot allocate without limit.

use std::net::SocketAddr;

use hl7_rest_server::http::{Hl7Http, json};
use hl7_rest_server::process::Mint;
use hl7_rest_server::serve::Handler;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use ihb_bus::sink::MessageSink;

use crate::adapter::ContentAdapter;
use crate::process::GenericProcessor;

/// The liveness route (`spec/19-rest-servers.md` §9).
pub const HEALTH_PATH: &str = "/health";

/// The `generic` pipeline, answering HTTP.
pub struct GenericHttp<E, S, M> {
    /// The pipeline.
    processor: GenericProcessor<E, S, M>,
    /// `MAX_REQUEST_SIZE_BYTES`.
    max_request_size: usize,
}

impl<E: MessageSink, S: MessageSink, M: Mint> GenericHttp<E, S, M> {
    /// Wrap a processor.
    pub const fn new(processor: GenericProcessor<E, S, M>, max_request_size: usize) -> Self {
        Self {
            processor,
            max_request_size,
        }
    }

    /// Answer one request.
    pub async fn route(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        let path = request.uri().path().to_string();
        let method = request.method().clone();

        if path == HEALTH_PATH && method == Method::GET {
            return json(StatusCode::OK, "{\"status\":\"ok\"}");
        }
        if path != self.processor.endpoint_path() {
            return detail(StatusCode::NOT_FOUND, "Not Found");
        }
        if method != Method::POST {
            return detail(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
        }
        self.post(request).await
    }

    /// `spec/19-rest-servers.md` §9.1.
    async fn post(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        let adapter = self.processor.adapter();
        let limit = self.max_request_size;

        let body = match Limited::new(request.into_body(), limit).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(error)
                if error
                    .downcast_ref::<http_body_util::LengthLimitError>()
                    .is_some() =>
            {
                return fault(
                    adapter,
                    413,
                    &format!("Request body exceeds configured limit of {limit} bytes."),
                );
            }
            Err(error) => {
                tracing::warn!(%error, "the request body could not be read");
                return fault(adapter, 400, "Request body could not be read.");
            }
        };

        if body.is_empty() {
            return fault(adapter, 400, "Request body is empty.");
        }
        let Ok(text) = std::str::from_utf8(&body) else {
            return fault(adapter, 400, "Request body must be UTF-8 encoded.");
        };

        let reply = self.processor.process(text).await;
        respond(
            StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            adapter.content_type(),
            reply.body,
        )
    }
}

impl<E, S, M> Handler for GenericHttp<E, S, M>
where
    E: MessageSink + 'static,
    S: MessageSink + 'static,
    M: Mint + 'static,
{
    async fn handle(&self, request: Request<Incoming>, _peer: SocketAddr) -> Response<Full<Bytes>> {
        self.route(request).await
    }
}

/// Whichever pipeline `PIPELINE` selected.
pub enum RestHandler<E, S, M> {
    /// `generic`.
    Generic(GenericHttp<E, S, M>),
    /// `hl7`.
    Hl7(Hl7Http<E, S, M>),
}

impl<E, S, M> Handler for RestHandler<E, S, M>
where
    E: MessageSink + 'static,
    S: MessageSink + 'static,
    M: Mint + 'static,
{
    async fn handle(&self, request: Request<Incoming>, peer: SocketAddr) -> Response<Full<Bytes>> {
        match self {
            Self::Generic(generic) => generic.route(request).await,
            Self::Hl7(hl7) => hl7.route(request, peer).await,
        }
    }
}

/// A client fault in the adapter's own format.
fn fault(adapter: &ContentAdapter, status: u16, message: &str) -> Response<Full<Bytes>> {
    respond(
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        adapter.content_type(),
        adapter.error_response("Client", message),
    )
}

/// `FastAPI`'s own 404 and 405 body, which both pipelines answer with.
fn detail(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    json(status, &format!("{{\"detail\":\"{message}\"}}"))
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
        .unwrap_or_else(|_| {
            let mut response = Response::new(Full::new(Bytes::new()));
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        })
}
