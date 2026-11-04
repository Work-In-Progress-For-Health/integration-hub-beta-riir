//! Ported from `test_senders.py`, plus the run loop of `test_send_controls.py`.
//! `spec/ultra7-test-message-sender.md` `§7`, `§8`.
#![allow(missing_docs, clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use ultra7::model::{
    Endpoint, EndpointKind, IterationMode, IterationSpec, Message, MessageFormat, Project,
};
use ultra7::run::{Event, describe_destination, send_enabled, send_messages, send_selected_once};
use ultra7::sender::{SendResult, send, send_mllp, send_rest, send_soap, wrap_in_envelope};

const MSG: &str = "MSH|^~\\&|A|B|C|D|20250101||ADT^A01|1|P|2.5";

/// One-shot MLLP server: reads to the end block, records it, replies `ack`
/// (nothing when `None`, then closes).
fn mllp_server(ack: Option<&'static [u8]>) -> (u16, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&received);
    thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        while !buf.contains(&0x1c) {
            let n = conn.read(&mut chunk).unwrap();
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        *rec.lock().unwrap() = buf;
        if let Some(a) = ack {
            conn.write_all(a).unwrap();
        }
    });
    (port, received)
}

fn mllp_endpoint(port: u16) -> Endpoint {
    Endpoint {
        host: "127.0.0.1".into(),
        port: Some(port),
        timeout_seconds: 2.0,
        ..Endpoint::default()
    }
}

fn hl7(content: &str) -> Message {
    Message::new("A01", MessageFormat::Hl7, content)
}

#[test]
fn mllp_send_receives_ack() {
    let (port, _) = mllp_server(Some(b"\x0bMSA|AA|MSG1\x1c\r"));
    let r = send_mllp(&mllp_endpoint(port), &hl7(MSG));
    assert!(r.ok, "{r:?}");
    assert!(r.response_summary.contains("MSA|AA|MSG1"));
}

#[test]
fn mllp_normalizes_lf_segment_separators_to_cr() {
    let (port, received) = mllp_server(Some(b"\x0bMSA|AA|1\x1c\r"));
    let r = send_mllp(
        &mllp_endpoint(port),
        &hl7(&format!("{MSG}\nPID|1\r\nPV1|1")),
    );
    assert!(r.ok);
    let sent = received.lock().unwrap().clone();
    assert!(!sent.contains(&b'\n'));
    assert_eq!(sent.first(), Some(&0x0b));
    assert!(String::from_utf8_lossy(&sent).contains(&format!("{MSG}\rPID|1\rPV1|1")));
}

#[test]
fn mllp_nak_is_reported_as_failure() {
    let (port, _) = mllp_server(Some(b"\x0bMSA|AE|1\x1c\r"));
    let r = send_mllp(&mllp_endpoint(port), &hl7(MSG));
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some("NAK: AE"));
}

#[test]
fn mllp_commit_accept_is_success() {
    let (port, _) = mllp_server(Some(b"\x0bMSA|CA|1\x1c\r"));
    assert!(send_mllp(&mllp_endpoint(port), &hl7(MSG)).ok);
}

#[test]
fn mllp_no_ack_is_reported_as_failure() {
    let (port, _) = mllp_server(None);
    let r = send_mllp(&mllp_endpoint(port), &hl7(MSG));
    assert!(!r.ok);
    assert_eq!(
        r.error.as_deref(),
        Some("No ACK received (connection closed)")
    );
}

#[test]
fn mllp_missing_host_port_is_an_error() {
    let r = send_mllp(&Endpoint::default(), &hl7("MSH|..."));
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some("Host and port are required"));
}

#[test]
fn mllp_connection_refused_is_reported_as_error() {
    let mut e = mllp_endpoint(1);
    e.timeout_seconds = 1.0;
    assert!(!send_mllp(&e, &hl7("MSH|...")).ok);
}

/// The request head and body a test server saw.
type Request = (String, Vec<u8>);

/// One-shot HTTP server recording `(request head, body)` and answering 200 `ack`
/// or `status`.
fn http_server(status: &'static str) -> (u16, Arc<Mutex<Request>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let got = Arc::new(Mutex::new((String::new(), Vec::new())));
    let rec = Arc::clone(&got);
    thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        let (head_end, len) = loop {
            let n = conn.read(&mut chunk).unwrap();
            buf.extend_from_slice(&chunk[..n]);
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                break (p + 4, len);
            }
        };
        while buf.len() < head_end + len {
            let n = conn.read(&mut chunk).unwrap();
            buf.extend_from_slice(&chunk[..n]);
        }
        *rec.lock().unwrap() = (
            String::from_utf8_lossy(&buf[..head_end]).into_owned(),
            buf[head_end..].to_vec(),
        );
        write!(conn, "HTTP/1.1 {status}\r\nContent-Length: 3\r\n\r\nack").unwrap();
    });
    (port, got)
}

fn http_endpoint(kind: EndpointKind, port: u16, path: &str) -> Endpoint {
    Endpoint {
        kind,
        url: format!("http://127.0.0.1:{port}{path}"),
        timeout_seconds: 2.0,
        ..Endpoint::default()
    }
}

#[test]
fn rest_rejects_non_http_scheme() {
    let e = Endpoint {
        kind: EndpointKind::Rest,
        url: "file:///etc/passwd".into(),
        ..Endpoint::default()
    };
    let r = send_rest(&e, &Message::new("m", MessageFormat::Json, "{}"));
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("scheme"));
}

#[test]
fn rest_missing_url_is_an_error() {
    let r = send_rest(
        &Endpoint::default(),
        &Message::new("m", MessageFormat::Json, "{}"),
    );
    assert_eq!(r.error.as_deref(), Some("URL is required"));
}

#[test]
fn https_is_refused_with_the_documented_reason() {
    // `§7.5`
    let e = Endpoint {
        url: "https://example.test/x".into(),
        ..Endpoint::default()
    };
    let r = send_rest(&e, &Message::new("m", MessageFormat::Json, "{}"));
    assert!(r.error.unwrap().starts_with("https is not supported"));
}

#[test]
fn rest_posts_body_and_returns_response() {
    let (port, got) = http_server("200 OK");
    let e = http_endpoint(EndpointKind::Rest, port, "/ingest?x=1");
    let r = send_rest(&e, &Message::new("m", MessageFormat::Json, r#"{"a": 1}"#));
    assert!(r.ok, "{r:?}");
    assert!(r.response_summary.contains("HTTP 200"));
    let (head, body) = got.lock().unwrap().clone();
    assert!(head.starts_with("POST /ingest?x=1 HTTP/1.1"));
    assert!(head.contains("Content-Type: application/json"));
    assert_eq!(body, br#"{"a": 1}"#);
}

#[test]
fn rest_endpoint_content_type_wins() {
    let (port, got) = http_server("200 OK");
    let mut e = http_endpoint(EndpointKind::Rest, port, "/");
    e.headers.insert("content-type".into(), "text/plain".into());
    assert!(send_rest(&e, &Message::new("m", MessageFormat::Json, "{}")).ok);
    let head = got.lock().unwrap().0.clone();
    assert!(head.contains("content-type: text/plain"));
    assert!(!head.contains("application/json"));
}

#[test]
fn http_error_status_is_a_failure_with_the_body() {
    let (port, _) = http_server("500 Internal Server Error");
    let r = send_rest(
        &http_endpoint(EndpointKind::Rest, port, "/"),
        &Message::new("m", MessageFormat::Json, "{}"),
    );
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some("HTTP 500"));
    assert_eq!(r.response_summary, "ack");
}

#[test]
fn header_injection_is_refused() {
    let mut e = http_endpoint(EndpointKind::Rest, 1, "/");
    e.headers.insert("X-A".into(), "v\r\nX-B: 1".into());
    let r = send_rest(&e, &Message::new("m", MessageFormat::Json, "{}"));
    assert!(r.error.unwrap().starts_with("Invalid header"));
}

#[test]
fn soap_wraps_body_in_envelope_and_sets_soap_action() {
    let (port, got) = http_server("200 OK");
    let mut e = http_endpoint(EndpointKind::Soap, port, "/soap");
    e.soap_action = "urn:submit".into();
    let r = send_soap(&e, &Message::new("m", MessageFormat::Xml, "<Payload/>"));
    assert!(r.ok, "{r:?}");
    let (head, body) = got.lock().unwrap().clone();
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains("Envelope") && body.contains("<Payload/>"));
    assert!(head.contains("SOAPAction: urn:submit"));
    assert!(head.contains("Content-Type: text/xml; charset=utf-8"));
}

#[test]
fn an_existing_envelope_is_not_wrapped_again() {
    let env = "<soap:Envelope xmlns:soap=\"x\"><soap:Body/></soap:Envelope>";
    assert_eq!(wrap_in_envelope(&format!("  {env}\n")), env);
}

#[test]
fn dispatch_follows_the_endpoint_kind() {
    let r = send(
        &Endpoint {
            kind: EndpointKind::Soap,
            ..Endpoint::default()
        },
        &hl7("x"),
    );
    assert_eq!(r.error.as_deref(), Some("URL is required"));
}

// --- the send loop (`§8`) ---------------------------------------------------

const fn ok_result() -> SendResult {
    SendResult {
        ok: true,
        latency_ms: 1.0,
        response_summary: String::new(),
        error: None,
    }
}

fn project_with_iteration() -> Project {
    let mut p = Project::new("P");
    p.endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port: Some(2575),
        ..Endpoint::default()
    };
    let mut m = Message::new("A01", MessageFormat::Hl7, "MSH|1|000001");
    m.iteration = Some(IterationSpec::new(6, 12, IterationMode::Increment));
    p.messages.push(m);
    let mut off = Message::new("off", MessageFormat::Hl7, "MSH|2");
    off.enabled = false;
    p.messages.push(off);
    p
}

#[test]
fn repeat_applies_iteration_per_repeat_and_labels_the_sends() {
    let p = project_with_iteration();
    let bodies = Mutex::new(Vec::new());
    let mut lines = Vec::new();
    let msgs: Vec<&Message> = p.messages.iter().filter(|m| m.enabled).collect();
    let summary = send_messages(
        &p.endpoint,
        &msgs,
        3,
        0,
        &AtomicBool::new(false),
        &|_, m| {
            bodies.lock().unwrap().push(m.content.clone());
            ok_result()
        },
        &mut |e| match e {
            Event::Info(l) => lines.push(l),
            Event::Result(l, _) => lines.push(format!("result {l}")),
        },
    );
    assert_eq!((summary.sent, summary.failed), (3, 0));
    assert_eq!(
        *bodies.lock().unwrap(),
        ["MSH|1|000001", "MSH|1|000002", "MSH|1|000003"]
    );
    assert_eq!(lines[0], "Sending A01 [1/3] to 127.0.0.1 on port 2575");
    assert_eq!(lines[1], "result A01 [1/3]");
}

#[test]
fn zero_repeat_count_is_clamped_to_one() {
    let p = project_with_iteration();
    let msgs: Vec<&Message> = p.messages.iter().take(1).collect();
    let s = send_messages(
        &p.endpoint,
        &msgs,
        0,
        0,
        &AtomicBool::new(false),
        &|_, _| ok_result(),
        &mut |_| {},
    );
    assert_eq!(s.sent, 1);
}

#[test]
fn cancel_stops_before_the_next_send() {
    let p = project_with_iteration();
    let msgs: Vec<&Message> = p.messages.iter().collect();
    let cancel = AtomicBool::new(false);
    let s = send_messages(
        &p.endpoint,
        &msgs,
        5,
        0,
        &cancel,
        &|_, _| ok_result(),
        &mut |e| {
            if matches!(e, Event::Result(..)) {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(s.sent, 1);
    assert!(s.cancelled);
}

#[test]
fn failures_are_counted() {
    let p = project_with_iteration();
    let msgs: Vec<&Message> = p.messages.iter().collect();
    let fail = |_: &Endpoint, _: &Message| SendResult {
        ok: false,
        latency_ms: 0.0,
        response_summary: String::new(),
        error: Some("x".into()),
    };
    let s = send_messages(
        &p.endpoint,
        &msgs,
        1,
        0,
        &AtomicBool::new(false),
        &fail,
        &mut |_| {},
    );
    assert_eq!((s.sent, s.failed), (2, 2));
}

#[test]
fn no_enabled_messages_sends_nothing() {
    let mut p = project_with_iteration();
    p.messages.clear();
    let err = send_enabled(&p, 1, 0, &AtomicBool::new(false), &mut |_| {}).unwrap_err();
    assert_eq!(err, "No enabled messages to send");
}

#[test]
fn selected_once_ignores_enabled_flag_and_validates_names() {
    let p = project_with_iteration();
    let none = send_selected_once(&p, &[], &AtomicBool::new(false), &mut |_| {}).unwrap_err();
    assert_eq!(none, "No message selected");
    let missing =
        send_selected_once(&p, &["nope".into()], &AtomicBool::new(false), &mut |_| {}).unwrap_err();
    assert!(missing.contains("nope"));
}

#[test]
fn destination_text() {
    let mllp = Endpoint {
        host: "127.0.0.1".into(),
        port: Some(2575),
        ..Endpoint::default()
    };
    assert_eq!(describe_destination(&mllp), "127.0.0.1 on port 2575");
    let rest = |u: &str| Endpoint {
        kind: EndpointKind::Rest,
        url: u.into(),
        ..Endpoint::default()
    };
    assert_eq!(
        describe_destination(&rest("https://example.test:443/x")),
        "example.test on port 443"
    );
    assert_eq!(
        describe_destination(&rest("https://example.test/x")),
        "example.test"
    );
    assert_eq!(describe_destination(&rest("not a url")), "not a url");
}
