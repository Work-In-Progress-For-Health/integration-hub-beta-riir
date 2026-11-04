//! The endpoints, over real HTTP.
//!
//! Implements `spec/19-rest-servers.md` §9, §10 and §12: the Python's
//! `test_asgi_app.py`, `test_hl7_soap_server_parity.py` and the routing half
//! of `test_rest_server_application.py`.
//!
//! Driven with a hand-written HTTP/1.1 client, as `hl7-soap-server`'s are, so
//! the tests can send what a well-behaved client cannot: no `Content-Length`,
//! a body that is not UTF-8, a body that lies about its size.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hl7_rest_server::http::Hl7Http;
use hl7_rest_server::process::{Mint, Processor};
use hl7_rest_server::serve::serve;
use ihb_bus::fake::{FakeBus, FakeSender};
use ihb_runtime::shutdown::Shutdown;
use rest_server::config::{Config, Pipeline};
use rest_server::http::{GenericHttp, RestHandler};
use rest_server::process::GenericProcessor;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The real LIMS request.
const LIMS: &str = include_str!("../../../local/sample_messages/lims-to-mpi.sample.xml");

const ER7_A28: &str = "MSH|^~\\&|252|252|MPI|MPI|20240101120000||ADT^A28^ADT_A05|MSGID12345|P|2.5\r\
                       EVN|A28|20240101120000\r\
                       PID|1||123456^^^NHS||SMITH^JOHN||19800101|M";

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

/// The `soap` + `hl7-xsd` + `phw` + `328` + `er7` profile of `rest_merge.md`
/// §9, which reproduces `hl7-soap-server`.
const SOAP_PROFILE: &[(&str, &str)] = &[
    ("ENDPOINT_PATH", "/soap"),
    ("CONTENT_ADAPTER", "soap"),
    ("VALIDATOR_TYPE", "hl7-xsd"),
    ("VALIDATION_SCHEMA", "phw"),
    ("ALLOWED_HL7_STRUCTURES", "ADT_A05,ADT_A39"),
    ("ALLOWED_SOURCE_IDENTIFIERS", "328"),
    ("OUTPUT_FORMAT", "er7"),
];

/// The `xml-raw` + `none` profile.
const RAW_PROFILE: &[(&str, &str)] = &[
    ("CONTENT_ADAPTER", "xml-raw"),
    ("VALIDATOR_TYPE", "none"),
    ("OUTPUT_FORMAT", "raw"),
];

async fn start(profile: &[(&str, &str)], extra: &[(&str, &str)]) -> Server {
    let mut pairs: Vec<(String, String)> = [
        ("WORKFLOW_ID", "workflow-rest"),
        ("MICROSERVICE_ID", "rest-server"),
        ("HEALTH_BOARD", "test-board"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_QUEUE_NAME", "egress-queue"),
        ("EGRESS_SESSION_ID", "rest-session"),
        ("MESSAGE_STORE_QUEUE_NAME", "store-queue"),
        ("PORT", "0"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (key, value) in profile.iter().chain(extra) {
        pairs.retain(|(name, _)| name != key);
        pairs.push(((*key).to_string(), (*value).to_string()));
    }

    let config = Config::from_pairs(pairs).expect("the test configuration is complete");
    let bus = FakeBus::new();
    let egress = bus.sender("egress-queue");
    let store = Some(bus.sender("store-queue"));

    let handler: RestHandler<FakeSender, FakeSender, Fixed> = match config.pipeline {
        Pipeline::Generic(generic) => RestHandler::Generic(GenericHttp::new(
            GenericProcessor::with_mint(config.identity, generic, egress, store, Fixed),
            config.max_request_size,
        )),
        Pipeline::Hl7(settings) => {
            let wrrs = settings
                .wrrs
                .as_ref()
                .map(|w| bus.sender(w.egress.entity()));
            RestHandler::Hl7(Hl7Http::new(
                Processor::with_mint(config.identity, settings, egress, wrrs, store, Fixed),
                config.environment,
                config.max_request_size,
            ))
        }
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let shutdown = Shutdown::new();
    let watcher = shutdown.watcher();
    let handler = Arc::new(handler);
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

/// Send raw bytes and read the whole response.
async fn raw(address: SocketAddr, request: &[u8]) -> Answer {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request).await.unwrap();
    stream.flush().await.unwrap();

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

async fn request(address: SocketAddr, method: &str, path: &str, body: &[u8]) -> Answer {
    let mut bytes = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    raw(address, &bytes).await
}

// ---- The generic pipeline's routes (test_asgi_app.py) --------------------

#[tokio::test]
async fn a_post_to_the_configured_path_reaches_the_processor() {
    // test_post_to_configured_path_is_forwarded_to_processor
    let server = start(RAW_PROFILE, &[]).await;
    let answer = request(server.address, "POST", "/ingest", b"<Document/>").await;

    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(
        answer
            .headers
            .to_lowercase()
            .contains("content-type: application/xml; charset=utf-8"),
        "{}",
        answer.headers
    );
    assert!(
        answer.body.starts_with("<ack><status>Success</status>"),
        "{}",
        answer.body
    );
    assert_eq!(server.bus.all_sent("egress-queue").len(), 1);
    server.stop().await;
}

#[tokio::test]
async fn an_unknown_path_is_404_and_a_get_on_the_endpoint_is_405() {
    // test_post_to_unknown_path_returns_404, test_get_on_ingest_path_returns_405
    let server = start(RAW_PROFILE, &[]).await;
    assert_eq!(
        request(server.address, "POST", "/unknown", b"<Document/>")
            .await
            .status,
        404
    );
    assert_eq!(
        request(server.address, "GET", "/ingest", b"").await.status,
        405
    );
    assert_eq!(server.bus.all_sent("egress-queue").len(), 0);
    server.stop().await;
}

#[tokio::test]
async fn an_empty_body_and_a_non_utf8_body_are_400s_in_the_adapters_format() {
    // test_empty_body_returns_400, test_non_utf8_body_returns_400
    let server = start(RAW_PROFILE, &[]).await;

    let empty = request(server.address, "POST", "/ingest", b"").await;
    assert_eq!(empty.status, 400);
    assert!(
        empty.body.contains("Request body is empty"),
        "{}",
        empty.body
    );

    let binary = request(server.address, "POST", "/ingest", &[0xff, 0xfe]).await;
    assert_eq!(binary.status, 400);
    assert!(binary.body.contains("UTF-8"), "{}", binary.body);
    assert_eq!(server.bus.all_sent("egress-queue").len(), 0);
    server.stop().await;
}

#[tokio::test]
async fn an_oversized_body_is_413_whatever_it_declares() {
    // test_oversized_body_returns_413; spec/19 §9.1.
    let server = start(RAW_PROFILE, &[("MAX_REQUEST_SIZE_BYTES", "10")]).await;

    let declared = request(server.address, "POST", "/ingest", &[b'x'; 100]).await;
    assert_eq!(declared.status, 413);
    assert!(
        declared.body.contains("exceeds configured limit"),
        "{}",
        declared.body
    );

    // No Content-Length at all: the bound is on the stream, not the header.
    let chunked = raw(
        server.address,
        format!(
            "POST /ingest HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n64\r\n{}\r\n0\r\n\r\n",
            "x".repeat(100)
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(chunked.status, 413, "{}", chunked.body);
    assert_eq!(server.bus.all_sent("egress-queue").len(), 0);
    server.stop().await;
}

#[tokio::test]
async fn health_is_ok_and_the_generated_docs_are_not_served() {
    // test_health_returns_ok; spec/19 §9 and §1.1.
    let server = start(RAW_PROFILE, &[]).await;
    let health = request(server.address, "GET", "/health", b"").await;
    assert_eq!(health.status, 200);
    assert_eq!(health.body, "{\"status\":\"ok\"}");

    assert_eq!(
        request(server.address, "GET", "/docs", b"").await.status,
        404
    );
    assert_eq!(
        request(server.address, "GET", "/openapi.json", b"")
            .await
            .status,
        404
    );
    server.stop().await;
}

// ---- The soap profile against hl7-soap-server (the parity tests) ---------

#[tokio::test]
async fn a_valid_lims_request_is_200_and_forwarded_as_er7() {
    // test_valid_request_matches_hl7_soap_server_200_and_forwards
    let server = start(SOAP_PROFILE, &[]).await;
    let answer = request(server.address, "POST", "/soap", LIMS.as_bytes()).await;

    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(
        answer.body.contains("<Status>Success</Status>"),
        "{}",
        answer.body
    );
    assert!(
        answer
            .body
            .contains("<MessageControlId>6778031837018553261z82215</MessageControlId>")
    );

    let sent = &server.bus.all_sent("egress-queue");
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].text().unwrap().starts_with("MSH|^~\\&|328|328"),
        "{}",
        sent[0].text().unwrap()
    );
    assert_eq!(
        sent[0].properties["MessageType"], "A28",
        "spec/19 §10.1 (e090e23)"
    );
    assert_eq!(sent[0].properties["SourceSystem"], "328");
    assert_eq!(server.bus.all_sent("store-queue").len(), 1);
    server.stop().await;
}

#[tokio::test]
async fn malformed_soap_is_a_400_fault_and_nothing_is_sent() {
    // test_malformed_soap_matches_hl7_soap_server_400_without_schema_validation
    let server = start(SOAP_PROFILE, &[]).await;
    let answer = request(server.address, "POST", "/soap", b"<not-xml").await;

    assert_eq!(answer.status, 400);
    assert!(answer.body.contains("<soapenv:Fault>"), "{}", answer.body);
    assert_eq!(server.bus.all_sent("egress-queue").len(), 0);
    server.stop().await;
}

#[tokio::test]
async fn a_schema_invalid_payload_is_a_400_and_not_forwarded() {
    // test_schema_invalid_payload_matches_hl7_soap_server_400_and_not_forwarded
    let server = start(SOAP_PROFILE, &[]).await;
    let invalid = LIMS
        .replace("<ns0:PID>", "<ns0:PIDX>")
        .replace("</ns0:PID>", "</ns0:PIDX>");
    let answer = request(server.address, "POST", "/soap", invalid.as_bytes()).await;

    assert_eq!(answer.status, 400, "{}", answer.body);
    assert!(
        answer.body.contains("Payload schema validation failed")
            || answer.body.contains("Unable to"),
        "{}",
        answer.body
    );
    assert_eq!(server.bus.all_sent("egress-queue").len(), 0);
    server.stop().await;
}

#[tokio::test]
async fn an_unauthorised_assigning_authority_is_a_403() {
    // test_unauthorised_assigning_authority_matches_hl7_soap_server_403
    let server = start(SOAP_PROFILE, &[]).await;
    let other = LIMS.replacen("<ns0:HD.1>328</ns0:HD.1>", "<ns0:HD.1>999</ns0:HD.1>", 1);
    let answer = request(server.address, "POST", "/soap", other.as_bytes()).await;

    assert_eq!(answer.status, 403, "{}", answer.body);
    assert!(answer.body.contains("not authorised"), "{}", answer.body);
    assert_eq!(server.bus.all_sent("egress-queue").len(), 0);
    server.stop().await;
}

#[tokio::test]
async fn a_missing_authority_is_a_400_not_a_403() {
    // SoapParityGapFixesTests::test_missing_assigning_authority_raises_400...
    let server = start(SOAP_PROFILE, &[]).await;
    let bare = "<SOAP-ENV:Envelope xmlns:SOAP-ENV=\"http://schemas.xmlsoap.org/soap/envelope/\"><SOAP-ENV:Body>\
                <ADT_A05><MSH><MSH.10>MSG1</MSH.10></MSH></ADT_A05></SOAP-ENV:Body></SOAP-ENV:Envelope>";
    let answer = request(server.address, "POST", "/soap", bare.as_bytes()).await;

    assert_eq!(answer.status, 400, "{}", answer.body);
    assert!(
        answer
            .body
            .contains("Unable to determine assigning authority"),
        "{}",
        answer.body
    );
    server.stop().await;
}

#[tokio::test]
async fn an_allowed_structure_with_no_schema_is_a_500_configuration_fault() {
    // SoapParityGapFixesTests::test_unmapped_schema_structure_raises_500...
    let server = start(
        SOAP_PROFILE,
        &[("ALLOWED_HL7_STRUCTURES", "ADT_A05,NOT_A_REAL_STRUCTURE")],
    )
    .await;
    let unmapped = LIMS.replace("ns0:ADT_A05", "ns0:NOT_A_REAL_STRUCTURE");
    let answer = request(server.address, "POST", "/soap", unmapped.as_bytes()).await;

    assert_eq!(answer.status, 500, "{}", answer.body);
    assert!(
        answer.body.contains("Server.Configuration"),
        "{}",
        answer.body
    );
    server.stop().await;
}

// ---- PIPELINE=hl7 through the same listener -------------------------------

#[tokio::test]
async fn the_hl7_pipeline_is_served_by_the_same_image() {
    // spec/19 §0.1 and §9: `PIPELINE=hl7` serves §1, and uses
    // MAX_REQUEST_SIZE_BYTES as its limit.
    let server = start(
        &[("PIPELINE", "hl7")],
        &[
            ("HL7_VALIDATION_FLOW", "mpi"),
            ("MAX_REQUEST_SIZE_BYTES", "2048"),
        ],
    )
    .await;

    let message = "MSH|^~\\&|100|100|252|252|2024||ADT^A31^ADT_A05|M1|P|2.5\r\
                   PID|1|8888888^^^252^PI|4444444444^^^NHS^NH";
    let body = serde_json::json!({ "messageContent": message }).to_string();
    let answer = request(
        server.address,
        "POST",
        "/hl7MessageReceiver",
        body.as_bytes(),
    )
    .await;

    assert_eq!(answer.status, 201, "{}", answer.body);
    assert!(answer.body.contains("MSA|AA|M1"), "{}", answer.body);
    assert_eq!(
        server.bus.all_sent("egress-queue")[0].properties["UpdateSources"],
        "|252|"
    );

    let big = serde_json::json!({ "messageContent": "x".repeat(4096) }).to_string();
    let oversize = request(
        server.address,
        "POST",
        "/hl7MessageReceiver",
        big.as_bytes(),
    )
    .await;
    assert_eq!(oversize.status, 400, "{}", oversize.body);

    let ping = request(server.address, "GET", "/hl7MessageReceiver/ping", b"").await;
    assert_eq!(ping.status, 200);
    assert!(
        ping.body.contains("\"environment\":\"DEV\""),
        "{}",
        ping.body
    );

    // The generic route does not exist on this pipeline.
    assert_eq!(
        request(server.address, "POST", "/ingest", b"<a/>")
            .await
            .status,
        404
    );
    server.stop().await;
}

#[tokio::test]
async fn er7_posted_to_the_hl7_pipeline_is_forwarded_untouched() {
    let server = start(&[("PIPELINE", "hl7")], &[]).await;
    let body = serde_json::json!({ "messageContent": ER7_A28 }).to_string();
    let answer = request(
        server.address,
        "POST",
        "/hl7MessageReceiver",
        body.as_bytes(),
    )
    .await;

    assert_eq!(answer.status, 201, "{}", answer.body);
    assert_eq!(
        server.bus.all_sent("egress-queue")[0].text().unwrap(),
        ER7_A28
    );
    server.stop().await;
}
