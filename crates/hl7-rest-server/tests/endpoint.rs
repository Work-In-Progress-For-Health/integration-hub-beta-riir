//! The endpoints, over real HTTP.
//!
//! Implements `spec/19-rest-servers.md` §1, §2 and §12: the Python's
//! `test_message_route.py` and `test_health_routes.py`.
//!
//! Driven with a hand-written HTTP/1.1 client so the tests see exactly what
//! a source system sees: the status line, the content type and the body.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hl7_rest_server::config::{Hl7Settings, Identity};
use hl7_rest_server::http::Hl7Http;
use hl7_rest_server::process::{Mint, Processor};
use hl7_rest_server::serve::serve;
use ihb_bus::error::BusError;
use ihb_bus::fake::{FakeBus, FakeSender};
use ihb_bus::retry::Disposition;
use ihb_config::Reader;
use ihb_runtime::shutdown::Shutdown;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const VALID: &str = "MSH|^~\\&|252|252|MPI|MPI|20240101120000||ADT^A28^ADT_A05|MSGID12345|P|2.5\r\
                     EVN|A28|20240101120000\r\
                     PID|1||123456^^^NHS||SMITH^JOHN||19800101|M";

const RISP_A40: &str = "MSH|^~\\&|349|349|MPI|MPI|20240101120000||ADT^A40^ADT_A39|RISPMSG002|P|2.5.1\r\
                        EVN|A40|20240101120000\r\
                        PID|1||654321^^^NHS||DOE^JANE||19900101|F\r\
                        MRG|123456^^^NHS";

/// The Python test's `_er7_to_min_xml`.
const MIN_XML: &str = concat!(
    r#"<ADT_A05 xmlns="urn:hl7-org:v2xml"><MSH><MSH.1>|</MSH.1><MSH.2>^~\&amp;</MSH.2>"#,
    "<MSH.3><HD.1>252</HD.1></MSH.3><MSH.4><HD.1>252</HD.1></MSH.4>",
    "<MSH.5><HD.1>MPI</HD.1></MSH.5><MSH.6><HD.1>MPI</HD.1></MSH.6>",
    "<MSH.7><TS.1>20240101120000</TS.1></MSH.7>",
    "<MSH.9><MSG.1>ADT</MSG.1><MSG.2>A28</MSG.2><MSG.3>ADT_A05</MSG.3></MSH.9>",
    "<MSH.10>MSGID12345</MSH.10><MSH.11><PT.1>P</PT.1></MSH.11>",
    "<MSH.12><VID.1>2.5</VID.1></MSH.12></MSH>",
    "<EVN><EVN.1>A28</EVN.1><EVN.2><TS.1>20240101120000</TS.1></EVN.2></EVN>",
    "<PID><PID.1>1</PID.1><PID.3><CX.1>123456</CX.1></PID.3>",
    "<PID.5><XPN.1><FN.1>SMITH</FN.1></XPN.1><XPN.2>JOHN</XPN.2></PID.5></PID></ADT_A05>",
);

struct Fixed;
impl Mint for Fixed {
    fn correlation_id(&self) -> String {
        "fixed-correlation-id".into()
    }
    fn received_at(&self) -> String {
        "2026-08-20T09:15:00+00:00".into()
    }
    fn ack_timestamp(&self) -> String {
        "20260820091500".into()
    }
    fn control_id(&self) -> String {
        "GENERATED".into()
    }
}

struct Server {
    address: SocketAddr,
    bus: FakeBus,
    shutdown: Shutdown,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Server {
    async fn stop(self) {
        self.shutdown.stop();
        self.task.await.unwrap().unwrap();
    }
}

struct Answer {
    status: u16,
    headers: String,
    body: String,
}

impl Answer {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

async fn start(extra: &[(&str, &str)], max_message_size: usize) -> Server {
    let mut pairs: Vec<(String, String)> = [
        ("WORKFLOW_ID", "test-workflow"),
        ("MICROSERVICE_ID", "hl7_rest_server"),
        ("HEALTH_BOARD", "TEST"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_QUEUE_NAME", "egress-queue"),
        ("EGRESS_SESSION_ID", "session"),
        ("MESSAGE_STORE_QUEUE_NAME", "store-queue"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    pairs.extend(
        extra
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
    );

    let mut reader = Reader::from_pairs(pairs);
    let identity = Identity::read(&mut reader);
    let settings = Hl7Settings::read(&mut reader);
    reader.finish().expect("the test configuration is complete");

    let bus = FakeBus::new();
    let wrrs = settings
        .wrrs
        .as_ref()
        .map(|w| bus.sender(w.egress.entity()));
    let processor: Processor<FakeSender, FakeSender, Fixed> = Processor::with_mint(
        identity,
        settings,
        bus.sender("egress-queue"),
        wrrs,
        Some(bus.sender("store-queue")),
        Fixed,
    );
    let handler = Arc::new(Hl7Http::new(processor, "DEV".to_string(), max_message_size));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let shutdown = Shutdown::new();
    let watcher = shutdown.watcher();
    let task = tokio::spawn(async move { serve(address, handler, None, watcher).await });

    for _ in 0..200 {
        if TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Server {
        address,
        bus,
        shutdown,
        task,
    }
}

async fn request(address: SocketAddr, method: &str, path: &str, body: &str) -> Answer {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut buffer = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut chunk)).await {
            Ok(Ok(read @ 1..)) => buffer.extend_from_slice(&chunk[..read]),
            _ => break,
        }
    }

    let text = String::from_utf8_lossy(&buffer).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    Answer {
        status,
        headers: head.to_string(),
        body: body.to_string(),
    }
}

async fn post(server: &Server, content: &str) -> Answer {
    let body = serde_json::json!({ "messageContent": content }).to_string();
    request(server.address, "POST", "/hl7MessageReceiver", &body).await
}

#[tokio::test]
async fn valid_er7_returns_201_with_the_raw_ack_and_sends_once() {
    // test_valid_er7_returns_201_with_raw_ack_and_sends_once; spec/19 §2.
    let server = start(&[], 1_048_576).await;
    let answer = post(&server, VALID).await;

    assert_eq!(answer.status, 201, "{}", answer.body);
    assert!(
        answer
            .headers
            .to_lowercase()
            .contains("content-type: text/plain"),
        "{}",
        answer.headers
    );
    assert!(
        answer.body.starts_with("MSH|^~\\&|DHCW|cymru.nhs.uk|"),
        "{}",
        answer.body
    );
    assert!(answer.body.contains("MSA|AA|MSGID12345"), "{}", answer.body);
    assert_eq!(server.bus.all_sent("egress-queue").len(), 1);
    server.stop().await;
}

#[tokio::test]
async fn valid_xml_is_converted_and_returns_201() {
    // test_valid_xml_is_converted_and_returns_201; spec/19 §2.2.
    let server = start(&[], 1_048_576).await;
    let answer = post(&server, MIN_XML).await;

    assert_eq!(answer.status, 201, "{}", answer.body);
    assert!(answer.body.contains("MSA|AA|MSGID12345"), "{}", answer.body);
    let forwarded = server.bus.all_sent("egress-queue")[0].text().unwrap();
    assert!(
        forwarded.starts_with("MSH|^~\\&|"),
        "forwarded as ER7: {forwarded}"
    );
    server.stop().await;
}

#[tokio::test]
async fn crlf_in_the_content_reaches_the_bus_as_cr() {
    // spec/19 §2.2 and spec/index.md §0.6.
    let server = start(&[], 1_048_576).await;
    let answer = post(&server, &VALID.replace('\r', "\r\n")).await;

    assert_eq!(answer.status, 201, "{}", answer.body);
    assert_eq!(
        server.bus.all_sent("egress-queue")[0].text().unwrap(),
        VALID
    );
    server.stop().await;
}

#[tokio::test]
async fn a_validation_failure_is_a_422_envelope_and_nothing_is_sent() {
    // test_validation_failure_returns_422_envelope_without_send
    let server = start(&[("HL7_VERSION", "2.9")], 1_048_576).await;
    let answer = post(&server, VALID).await;

    assert_eq!(answer.status, 422);
    let body = answer.json();
    assert_eq!(body["StatusCode"], 422);
    assert!(
        body["ErrorMessage"]
            .as_str()
            .unwrap()
            .contains("MSA|AR|MSGID12345"),
        "{body}"
    );
    assert!(
        answer
            .body
            .starts_with("{\"StatusCode\":422,\"ErrorMessage\":"),
        "key order: {}",
        answer.body
    );
    assert_eq!(server.bus.all_sent("egress-queue").len(), 0);
    server.stop().await;
}

#[tokio::test]
async fn an_unparsable_message_is_a_500_with_a_generic_nack() {
    // test_unparsable_message_returns_500_generic_nack
    let server = start(&[], 1_048_576).await;
    let answer = post(&server, "this is not an HL7 message").await;

    assert_eq!(answer.status, 500);
    let body = answer.json();
    assert_eq!(body["StatusCode"], 500);
    let nack = body["ErrorMessage"].as_str().unwrap();
    assert!(nack.contains("MSA|AE|"), "{nack}");
    let control_id = nack
        .split("MSA|AE|")
        .nth(1)
        .unwrap()
        .split('|')
        .next()
        .unwrap();
    assert!(control_id.len() <= 20);
    assert_eq!(server.bus.all_sent("egress-queue").len(), 0);
    server.stop().await;
}

#[tokio::test]
async fn xml_that_does_not_convert_is_a_500_not_a_400() {
    // spec/19 §2.2.
    let server = start(&[], 1_048_576).await;
    let answer = post(&server, "<not-hl7/>").await;

    assert_eq!(answer.status, 500, "{}", answer.body);
    assert!(
        answer.json()["ErrorMessage"]
            .as_str()
            .unwrap()
            .contains("MSA|AE|")
    );
    server.stop().await;
}

#[tokio::test]
async fn an_oversize_message_is_a_400() {
    // test_oversize_message_returns_400; spec/19 §2.1.
    let server = start(&[], 10).await;
    let answer = post(&server, VALID).await;

    assert_eq!(answer.status, 400);
    assert_eq!(answer.json()["StatusCode"], 400);
    assert_eq!(
        answer.json()["ErrorMessage"],
        "Message exceeds the maximum allowed size."
    );
    assert_eq!(server.bus.all_sent("egress-queue").len(), 0);
    server.stop().await;
}

#[tokio::test]
async fn a_body_that_is_not_the_expected_json_is_a_400() {
    // test_missing_body_field_returns_400
    let server = start(&[], 1_048_576).await;
    for body in [
        r#"{"wrong":"field"}"#,
        r#"{"messageContent":5}"#,
        "not json",
        "",
    ] {
        let answer = request(server.address, "POST", "/hl7MessageReceiver", body).await;
        assert_eq!(answer.status, 400, "{body}: {}", answer.body);
        assert_eq!(
            answer.json()["ErrorMessage"],
            "Invalid or missing request body."
        );
    }
    server.stop().await;
}

#[tokio::test]
async fn unknown_paths_are_404_and_wrong_methods_405() {
    // test_unknown_route_returns_404; spec/19 §1.
    let server = start(&[], 1_048_576).await;
    assert_eq!(
        request(server.address, "GET", "/does-not-exist", "")
            .await
            .status,
        404
    );
    assert_eq!(
        request(server.address, "GET", "/hl7MessageReceiver", "")
            .await
            .status,
        405
    );
    assert_eq!(
        request(server.address, "POST", "/hl7MessageReceiver/ping", "")
            .await
            .status,
        405
    );
    // spec/19 §1.1 — the generated docs are not served.
    assert_eq!(
        request(server.address, "GET", "/docs", "").await.status,
        404
    );
    assert_eq!(
        request(server.address, "GET", "/openapi.json", "")
            .await
            .status,
        404
    );
    server.stop().await;
}

#[tokio::test]
async fn a_service_bus_failure_is_a_500_and_there_is_no_ack() {
    // test_service_bus_send_failure_returns_500_no_ack; AGENTS.md rule 2.
    // A deployment with no message store, so the armed failure lands on the
    // egress send: the store is sent to first and would otherwise take it.
    let mut reader = Reader::from_pairs([
        ("WORKFLOW_ID", "w"),
        ("MICROSERVICE_ID", "m"),
        ("HEALTH_BOARD", "h"),
        ("PEER_SERVICE", "p"),
        ("EGRESS_QUEUE_NAME", "egress-queue"),
        ("EGRESS_SESSION_ID", "s"),
    ]);
    let identity = Identity::read(&mut reader);
    let settings = Hl7Settings::read(&mut reader);
    let bus = FakeBus::new();
    let processor: Processor<FakeSender, FakeSender, Fixed> = Processor::with_mint(
        identity,
        settings,
        bus.sender("egress-queue"),
        None,
        None,
        Fixed,
    );
    let handler = Arc::new(Hl7Http::new(processor, "DEV".to_string(), 1_048_576));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let shutdown = Shutdown::new();
    let watcher = shutdown.watcher();
    let task = tokio::spawn(async move { serve(address, handler, None, watcher).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    bus.fail_next_send(BusError::Send {
        destination: "egress-queue".into(),
        attempts: 1,
        reason: "Service Bus unavailable".into(),
        disposition: Disposition::Fatal,
    });
    let body = serde_json::json!({ "messageContent": VALID }).to_string();
    let answer = request(address, "POST", "/hl7MessageReceiver", &body).await;

    assert_eq!(answer.status, 500, "{}", answer.body);
    assert_eq!(answer.json()["StatusCode"], 500);
    assert!(!answer.body.contains("MSA|AA|"), "no ACK: {}", answer.body);
    assert_eq!(bus.all_sent("egress-queue").len(), 0);

    shutdown.stop();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn risp_a40_reaches_both_destinations_over_http() {
    // spec/19 §6.
    let server = start(
        &[
            ("HL7_VALIDATION_FLOW", "risp"),
            ("WRRS_TOPIC_NAME", "wrrs-topic"),
            ("WRRS_EGRESS_SESSION_ID", "risp-to-wrrs"),
            ("WRRS_WORKFLOW_ID", "risp-to-wrrs"),
        ],
        1_048_576,
    )
    .await;
    let answer = post(&server, RISP_A40).await;

    assert_eq!(answer.status, 201, "{}", answer.body);
    assert_eq!(server.bus.all_sent("egress-queue").len(), 1);
    assert_eq!(server.bus.all_sent("wrrs-topic").len(), 1);
    server.stop().await;
}

#[tokio::test]
async fn ping_reports_the_environment_and_the_client() {
    // test_ping_returns_ok; spec/19 §1.2.
    let server = start(&[], 1_048_576).await;
    let answer = request(server.address, "GET", "/hl7MessageReceiver/ping", "").await;

    assert_eq!(answer.status, 200);
    let body = answer.json();
    assert_eq!(body["environment"], "DEV");
    assert_eq!(body["version"], "1.0");
    assert_eq!(body["revision"], "0");
    assert_eq!(body["clientIp"], "127.0.0.1");
    server.stop().await;
}

#[tokio::test]
async fn status_remembers_the_previous_request() {
    // test_status_reports_ok_without_timeout; spec/19 §1.3.
    let server = start(&[], 1_048_576).await;
    let first = request(server.address, "GET", "/hl7MessageReceiver/status", "")
        .await
        .json();
    let second = request(server.address, "GET", "/hl7MessageReceiver/status", "")
        .await
        .json();

    assert_eq!(first["status"], 200);
    assert_eq!(first["timeout"], false);
    assert_eq!(first["application"], "HL7 Message Receiver");
    assert_eq!(first["lastRequested"], "Never");
    assert_ne!(second["lastRequested"], "Never");
    assert_ne!(second["lastHealthy"], "Never");
    server.stop().await;
}
