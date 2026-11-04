//! The HTTP endpoint.
//!
//! Implements `spec/11-mock-receivers.md` §3.1, §3.2 and §3.9.

use std::net::SocketAddr;
use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::CONTENT_TYPE;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use ihb_bus::message::Outgoing;
use ihb_bus::sink::MessageSink;
use ihb_runtime::shutdown::Watcher;
use tokio::net::TcpListener;

use crate::soap::{control_id, detect_version, extract_payload, fault, success};

/// What the receiver forwards to, and how it is configured.
///
/// Shared by every connection behind an `Arc`; it is read-only after
/// startup, so no lock is needed.
pub struct Receiver<S> {
    /// Where a received message goes, when one is configured (§3.9).
    pub sink: Option<S>,
    /// `EGRESS_SESSION_ID`, stamped on every forwarded message; a
    /// session-enabled queue refuses a message that has none.
    pub session_id: Option<String>,
}

/// Serve until the watcher says stop.
///
/// On stop the listener closes first and open connections are then awaited
/// rather than dropped, so a request already being forwarded still gets its
/// reply. Connections are HTTP/1.1 only; a SOAP sender here needs no more.
///
/// # Errors
///
/// [`std::io::Error`] if the address cannot be bound.
pub async fn serve<S>(
    address: SocketAddr,
    receiver: Arc<Receiver<S>>,
    watcher: Watcher,
) -> std::io::Result<()>
where
    S: MessageSink + 'static,
{
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr().unwrap_or(address);
    tracing::info!(%bound, forwarding = receiver.sink.is_some(), "http mock receiver listening");

    let mut connections = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            biased;

            () = watcher.wait() => break,

            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let receiver = Arc::clone(&receiver);
                    connections.spawn(async move {
                        let service = service_fn(move |request| {
                            let receiver = Arc::clone(&receiver);
                            async move { handle(request, receiver).await }
                        });
                        if let Err(error) = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .await
                        {
                            tracing::debug!(%peer, %error, "connection ended");
                        }
                    });
                }
                Err(error) => tracing::warn!(%error, "could not accept a connection"),
            },
        }
    }

    while connections.join_next().await.is_some() {}
    tracing::info!("http mock receiver stopped");

    Ok(())
}

/// Answer one request.
///
/// # Errors
///
/// Never: §1.2 requires a mock to always answer, so every path here
/// produces a response.
async fn handle<S: MessageSink>(
    request: Request<Incoming>,
    receiver: Arc<Receiver<S>>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let collected = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            tracing::warn!(%error, "could not read the request body");
            Bytes::new()
        }
    };
    let body = String::from_utf8_lossy(&collected).into_owned();

    // §3.3 — detected from the raw text, so a body that does not parse
    // still gets the right reply shape.
    let version = detect_version(&body);

    // §3.4. Note this finds the payload by text scan anywhere in the body,
    // not only as an immediate child of `Body`; broader than §3.4's wording
    // and deliberately forgiving (§3.2).
    let Some(payload) = extract_payload(&body) else {
        // §3.2 — logged at warn, and still answered.
        tracing::warn!("no HL7 payload found in the request");
        return Ok(respond(
            version,
            StatusCode::INTERNAL_SERVER_ERROR,
            fault(version, "No HL7 payload found in the request"),
        ));
    };

    let id = control_id(&payload);

    // §1.1 — the mock convention, honoured here as in `hl7-mock-receiver`.
    if payload.to_ascii_lowercase().contains("fail") {
        tracing::info!(control_id = %id, "rejecting on request");
        return Ok(respond(
            version,
            StatusCode::INTERNAL_SERVER_ERROR,
            fault(version, "Rejected on request"),
        ));
    }

    // §3.9 — forwarding is optional.
    if let Some(sink) = receiver.sink.as_ref() {
        let outgoing = Outgoing::new(payload.clone())
            .with_message_id(id.clone())
            .with_optional_session(receiver.session_id.as_deref());

        if let Err(error) = sink.send(outgoing).await {
            // As `hl7-mock-receiver` §2.3.1: an acknowledgement that says
            // yes when nothing was forwarded is a lie, and a test waiting
            // on the queue would hang rather than fail.
            tracing::error!(%error, control_id = %id, "could not forward; rejecting");
            return Ok(respond(
                version,
                StatusCode::INTERNAL_SERVER_ERROR,
                fault(version, "Could not forward the message"),
            ));
        }
    } else {
        tracing::info!(control_id = %id, bytes = payload.len(), "not forwarding: no queue configured");
    }

    tracing::info!(control_id = %id, "acknowledged");
    Ok(respond(version, StatusCode::OK, success(version, &id)))
}

/// One response, in the detected version's content type (§3.3).
///
/// `Content-Length` is set explicitly so a sender that does not handle
/// chunked replies still reads the whole body. The builder cannot fail for
/// these inputs; the fallback exists so that `handle` stays infallible
/// (§1.2) without an `expect`.
fn respond(
    version: crate::soap::Version,
    status: StatusCode,
    body: String,
) -> Response<Full<Bytes>> {
    let bytes = Bytes::from(body);
    let length = bytes.len();

    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, version.content_type())
        .header(hyper::header::CONTENT_LENGTH, length)
        .body(Full::new(bytes))
        .unwrap_or_else(|_| {
            let mut response = Response::new(Full::new(Bytes::new()));
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        })
}
