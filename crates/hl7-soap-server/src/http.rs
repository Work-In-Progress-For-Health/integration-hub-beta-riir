//! The HTTP endpoint.
//!
//! Implements `spec/08-hl7-soap-server.md` §1, §2, §5 and §6.
//!
//! [`crate::process`] decides what a request *means*; this module decides
//! how it is admitted and how the answer is written. The split matters
//! because §2's checks happen **before the body is read** — a request that
//! declares 400 MB must be refused without allocating 400 MB — so they
//! cannot live behind a function that takes the body as a string.

use std::net::SocketAddr;
use std::sync::Arc;

use hl7_2_soap::fault::Fault;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::{Method, Request, Response, StatusCode};
use ihb_bus::sink::MessageSink;

use crate::process::{Mint, Processor};

/// Every response carries this (§1.1).
pub const XML_CONTENT_TYPE: &str = "text/xml; charset=utf-8";

/// The WSDL's target namespace (§6.4).
pub const TARGET_NAMESPACE: &str = "urn:inthub:hl7-soap-server";

/// The header a reverse proxy uses to report the original scheme (§6.2).
pub const FORWARDED_PROTO: &str = "x-forwarded-proto";

/// Answer one request.
///
/// # Errors
///
/// Never: every failure becomes a fault response, because a SOAP client
/// needs a fault envelope rather than a dropped connection. The `Result` is
/// hyper's signature, not a possibility.
pub async fn handle<E, S, M>(
    request: Request<Incoming>,
    processor: Arc<Processor<E, S, M>>,
    bound: SocketAddr,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible>
where
    E: MessageSink,
    S: MessageSink,
    M: Mint,
{
    Ok(route(request, &processor, bound).await)
}

/// §1's table, and §2's admission checks.
async fn route<E, S, M>(
    request: Request<Incoming>,
    processor: &Processor<E, S, M>,
    bound: SocketAddr,
) -> Response<Full<Bytes>>
where
    E: MessageSink,
    S: MessageSink,
    M: Mint,
{
    let config = processor.config();

    // §1.2 — the target may be absolute-form, which Azure Container Apps
    // ingress does forward. Splitting on '?' would take the path from
    // `https://host/soap?wsdl` as `https://host/soap`, so the path and
    // query come from hyper's parsed `Uri` instead.
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or_default().to_string();
    let method = request.method().clone();

    if path != config.endpoint_path {
        // §1 and §2's first row: **404** for any other path, whatever the
        // method. Built with an explicit status rather than `Fault::client`,
        // which is a 400: §2 pairs each condition with its own status, and
        // the difference is what tells a caller "wrong address" from
        // "wrong request".
        return fault_response(&Fault::new(
            "Client",
            "Unknown SOAP endpoint path.",
            StatusCode::NOT_FOUND.as_u16(),
        ));
    }

    match method {
        Method::GET if is_wsdl_query(&query) => {
            wsdl_response(&request, bound, config.tls.is_some())
        }
        Method::GET => {
            // §1 — a GET without `?wsdl` is a 405, not the 404 above: the
            // path is right, the method is not.
            method_not_allowed()
        }
        Method::POST => post(request, processor).await,
        _ => method_not_allowed(),
    }
}

/// §1's third row: the path exists but this method is not accepted.
fn method_not_allowed() -> Response<Full<Bytes>> {
    let fault = Fault::new(
        "Client",
        "SOAP endpoint accepts POST only.",
        StatusCode::METHOD_NOT_ALLOWED.as_u16(),
    );
    fault_response(&fault)
}

/// Whether the query string asks for the WSDL.
///
/// `?wsdl` is a bare key with no value, and SoapUI and .NET both send it
/// that way; `?wsdl=` and `?WSDL` also appear in the wild.
fn is_wsdl_query(query: &str) -> bool {
    query.split('&').any(|part| {
        part.split('=')
            .next()
            .is_some_and(|key| key.eq_ignore_ascii_case("wsdl"))
    })
}

/// §2's admission checks, then §3's processing.
async fn post<E, S, M>(
    request: Request<Incoming>,
    processor: &Processor<E, S, M>,
) -> Response<Full<Bytes>>
where
    E: MessageSink,
    S: MessageSink,
    M: Mint,
{
    let limit = processor.config().max_request_size;

    // §2, in order. Each row is a distinct status, so they are checked
    // separately rather than collapsed into "bad request".
    let declared = match request.headers().get(CONTENT_LENGTH) {
        None => {
            return respond(
                StatusCode::LENGTH_REQUIRED,
                Fault::client("Content-Length header is required.").to_envelope(),
            );
        }
        Some(value) => match value.to_str().ok().map(str::trim).map(str::parse::<i64>) {
            Some(Ok(declared)) => declared,
            _ => {
                return respond(
                    StatusCode::BAD_REQUEST,
                    Fault::client("Invalid Content-Length header.").to_envelope(),
                );
            }
        },
    };

    if declared <= 0 {
        return respond(
            StatusCode::BAD_REQUEST,
            Fault::client("SOAP request body is empty.").to_envelope(),
        );
    }

    // §2.1 — checked on the **declared** length, before reading, so a
    // request that announces a gigabyte costs nothing to refuse.
    if declared > i64::try_from(limit).unwrap_or(i64::MAX) {
        let fault = Fault::client(format!(
            "SOAP request body exceeds configured limit of {limit} bytes. Received: {declared} bytes."
        ));
        return respond(StatusCode::PAYLOAD_TOO_LARGE, fault.to_envelope());
    }

    // §2.1's second half: the reader is bounded too, so a *lying*
    // `Content-Length` cannot allocate without limit. `Limited` stops at
    // the cap rather than trusting the header it just checked.
    let body = http_body_util::Limited::new(request.into_body(), limit);
    let collected = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            tracing::warn!(%error, "the request body could not be read within the limit");
            let fault = Fault::client(format!(
                "SOAP request body exceeds configured limit of {limit} bytes. Received: {declared} bytes."
            ));
            return respond(StatusCode::PAYLOAD_TOO_LARGE, fault.to_envelope());
        }
    };

    let Ok(text) = std::str::from_utf8(&collected) else {
        return respond(
            StatusCode::BAD_REQUEST,
            Fault::client("SOAP request body must be UTF-8 encoded.").to_envelope(),
        );
    };

    match processor.process(text).await {
        Ok(accepted) => respond(
            StatusCode::OK,
            hl7_2_soap::response::success_in(&accepted.control_id, TARGET_NAMESPACE),
        ),
        Err(fault) => fault_response(&fault),
    }
}

/// §6 — the WSDL, addressed at the host the request arrived on.
fn wsdl_response(
    request: &Request<Incoming>,
    bound: SocketAddr,
    tls: bool,
) -> Response<Full<Bytes>> {
    // §6.2 — the first value of `X-Forwarded-Proto`, trimmed. Behind Azure
    // Container Apps ingress the connection into this process is plaintext
    // even when the client used HTTPS, so without this the WSDL would
    // advertise an address nothing can reach.
    let scheme = request
        .headers()
        .get(FORWARDED_PROTO)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || if tls { "https" } else { "http" }.to_string(),
            ToString::to_string,
        );

    // §6.3 — the `Host` header, falling back to the bound address.
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| bound.to_string(), ToString::to_string);

    let address = format!("{scheme}://{host}{}", request.uri().path());
    respond(
        StatusCode::OK,
        hl7_2_soap::wsdl::for_address_in(&address, TARGET_NAMESPACE),
    )
}

/// A fault, at the status §3.1 gives it.
fn fault_response(fault: &Fault) -> Response<Full<Bytes>> {
    let status = StatusCode::from_u16(fault.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    respond(status, fault.to_envelope())
}

/// One response, with §1.1's headers.
fn respond(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    let bytes = Bytes::from(body);
    let length = bytes.len();

    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, XML_CONTENT_TYPE)
        .header(CONTENT_LENGTH, length)
        .body(Full::new(bytes))
        // The builder only fails on an invalid header name or value, and
        // both are constants here. A 500 with no body is still better than
        // a panic on a public endpoint.
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
    fn the_wsdl_query_is_recognised_in_the_spellings_clients_send() {
        // A bare `?wsdl` is what SoapUI and .NET send; the others appear in
        // the wild and mean the same thing.
        assert!(is_wsdl_query("wsdl"));
        assert!(is_wsdl_query("wsdl="));
        assert!(is_wsdl_query("WSDL"));
        assert!(is_wsdl_query("foo=1&wsdl"));

        assert!(!is_wsdl_query(""));
        assert!(!is_wsdl_query("wsdlish"));
        assert!(!is_wsdl_query("notwsdl=1"));
    }

    #[test]
    fn a_fault_keeps_the_status_section_three_gives_it() {
        // spec/08-hl7-soap-server.md §3.1 — the status is the retry signal,
        // so it travels with the fault rather than being decided here.
        assert_eq!(fault_response(&Fault::client("x")).status(), 400);
        assert_eq!(fault_response(&Fault::validation("x")).status(), 400);
        assert_eq!(fault_response(&Fault::authorization("x")).status(), 403);
        assert_eq!(fault_response(&Fault::server("x")).status(), 500);
        assert_eq!(fault_response(&Fault::configuration("x")).status(), 500);
    }

    #[test]
    fn every_response_declares_its_type_and_length() {
        // spec/08-hl7-soap-server.md §1.1.
        let response = respond(StatusCode::OK, "<a/>".to_string());

        assert_eq!(response.headers()[CONTENT_TYPE], XML_CONTENT_TYPE);
        assert_eq!(response.headers()[CONTENT_LENGTH], "4");
    }
}
