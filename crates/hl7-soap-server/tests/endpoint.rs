//! The endpoint, over real HTTP.
//!
//! Implements `spec/08-hl7-soap-server.md` §8.1, §8.2, §8.5, §8.6 and §8.7.
//!
//! Driven with a hand-written HTTP/1.1 client rather than a library, so the
//! tests can send the malformed requests §2 is about: a missing
//! `Content-Length`, one that lies, one that is not a number. A well-behaved
//! client cannot produce those, and they are exactly what has to be refused.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use hl7_soap_server::config::Config;
use hl7_soap_server::process::{Mint, Processor};
use hl7_soap_server::serve::serve;
use ihb_bus::fake::{FakeBus, FakeSender};
use ihb_runtime::shutdown::Shutdown;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The egress queue.
const EGRESS: &str = "lims-transformer-ingress";

/// The message-store queue.
const STORE: &str = "messagestore";

/// The real LIMS request.
const LIMS: &str = include_str!("../../../local/sample_messages/lims-to-mpi.sample.xml");

/// Fixed values, so a whole message can be asserted on.
struct FixedMint;

impl Mint for FixedMint {
    fn correlation_id(&self) -> String {
        "fixed-correlation-id".to_string()
    }
    fn received_at(&self) -> String {
        "2026-08-20T09:15:00+00:00".to_string()
    }
}

/// A running server.
struct Server {
    address: std::net::SocketAddr,
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

/// One raw HTTP response.
struct Answer {
    status: u16,
    headers: String,
    body: String,
}

/// Start a server on an ephemeral port.
async fn start(extra: &[(&str, &str)]) -> Server {
    let mut pairs: Vec<(String, String)> = [
        ("WORKFLOW_ID", "lims-soapserver"),
        ("MICROSERVICE_ID", "hl7-soap-server"),
        ("HEALTH_BOARD", "WLIMS"),
        ("PEER_SERVICE", "MPI"),
        ("EGRESS_QUEUE_NAME", EGRESS),
        ("EGRESS_SESSION_ID", "lims-soapserver"),
        ("MESSAGE_STORE_QUEUE_NAME", STORE),
        ("PORT", "0"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    for (key, value) in extra {
        pairs.retain(|(name, _)| name != key);
        pairs.push(((*key).to_string(), (*value).to_string()));
    }

    let config = Config::from_pairs(pairs).expect("the test configuration is complete");
    let bus = FakeBus::new();
    let processor: Arc<Processor<FakeSender, FakeSender, FixedMint>> =
        Arc::new(Processor::with_mint(
            config.clone(),
            bus.sender(EGRESS),
            Some(bus.sender(STORE)),
            FixedMint,
        ));

    let listener = tokio::net::TcpListener::bind(config.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let shutdown = Shutdown::new();
    let watcher = shutdown.watcher();
    let task = tokio::spawn(async move { serve(address, processor, None, watcher).await });

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
async fn raw(address: std::net::SocketAddr, request: &[u8]) -> Answer {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request).await.unwrap();
    stream.flush().await.unwrap();

    let mut buffer = Vec::new();
    // The server closes or stalls after the response; a short read timeout
    // ends the read without waiting for keep-alive to expire.
    loop {
        let mut chunk = [0_u8; 4096];
        // A closed peer, a read error and a quiet socket all end the
        // response the same way, so they share an arm.
        match tokio::time::timeout(Duration::from_millis(400), stream.read(&mut chunk)).await {
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

/// A well-formed POST of `body`.
async fn post(address: std::net::SocketAddr, body: &str) -> Answer {
    let request = format!(
        "POST /soap HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/xml\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    raw(address, request.as_bytes()).await
}

#[tokio::test]
async fn the_real_lims_request_is_accepted_and_forwarded() {
    // spec/08-hl7-soap-server.md §8.5 — the whole path, over real HTTP,
    // with the actual sample.
    let server = start(&[]).await;

    let answer = post(server.address, LIMS).await;

    assert_eq!(answer.status, 200, "{}\n{}", answer.headers, answer.body);
    assert!(
        answer.body.contains("<Status>Success</Status>"),
        "{}",
        answer.body
    );
    assert!(
        answer.body.contains("6778031837018553261z82215"),
        "the control id comes back: {}",
        answer.body
    );

    // §1.1.
    assert!(
        answer
            .headers
            .to_lowercase()
            .contains("content-type: text/xml; charset=utf-8"),
        "{}",
        answer.headers
    );

    // The ER7 reached the queue.
    let sent = server.bus.all_sent(EGRESS);
    assert_eq!(sent.len(), 1);
    let er7 = String::from_utf8_lossy(&sent[0].body);
    assert!(er7.starts_with("MSH|^~\\&"), "{er7}");
    assert!(er7.contains("ADT^A28"), "{er7}");
    assert_eq!(
        sent[0].message_id.as_deref(),
        Some("6778031837018553261z82215")
    );

    // §3 step 12 — RawPayload is the whole SOAP request, not the ER7.
    let stored = server.bus.all_sent(STORE);
    assert_eq!(stored.len(), 1);
    let event =
        ihb_bus::store::StoreEvent::from_json(std::str::from_utf8(&stored[0].body).unwrap())
            .unwrap();
    assert!(
        event.raw_payload.contains("SOAP-ENV:Envelope"),
        "the whole request"
    );
    assert!(
        event
            .xml_payload
            .as_deref()
            .unwrap()
            .starts_with("<ns0:ADT_A05"),
        "and the payload element on its own"
    );

    server.stop().await;
}

#[tokio::test]
async fn every_admission_check_of_section_two() {
    // spec/08-hl7-soap-server.md §2 and §8.1 — one table, so a change to
    // one row cannot be made without seeing the others.
    let server = start(&[("MAX_REQUEST_SIZE_BYTES", "2048")]).await;
    let a = server.address;

    // No Content-Length.
    let answer = raw(
        a,
        b"POST /soap HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(
        answer.status, 411,
        "headers={:?} body={:?}",
        answer.headers, answer.body
    );
    assert!(
        answer.body.contains("Content-Length header is required."),
        "{}",
        answer.body
    );

    // Unparseable Content-Length. §2 asks for 400 with a fault envelope;
    // hyper rejects it as a protocol violation before the handler runs and
    // answers 400 with an empty body. The status — which is what a client
    // acts on — is right; the body is not. See `spec/08` §2.2 and
    // `plan.md` §9.10.
    let answer = raw(
        a,
        b"POST /soap HTTP/1.1\r\nHost: localhost\r\nContent-Length: banana\r\n\r\n",
    )
    .await;
    assert_eq!(answer.status, 400, "{}", answer.headers);

    // Zero Content-Length.
    let answer = raw(
        a,
        b"POST /soap HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(answer.status, 400);
    assert!(
        answer.body.contains("SOAP request body is empty."),
        "{}",
        answer.body
    );

    // Over the limit — refused on the declared length, before reading.
    let answer = raw(
        a,
        b"POST /soap HTTP/1.1\r\nHost: localhost\r\nContent-Length: 99999\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(answer.status, 413);
    assert!(
        answer
            .body
            .contains("exceeds configured limit of 2048 bytes. Received: 99999 bytes."),
        "{}",
        answer.body
    );

    // Not UTF-8.
    let mut request =
        b"POST /soap HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\nConnection: close\r\n\r\n"
            .to_vec();
    request.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0xFC]);
    let answer = raw(a, &request).await;
    assert_eq!(answer.status, 400);
    assert!(
        answer.body.contains("must be UTF-8 encoded."),
        "{}",
        answer.body
    );

    server.stop().await;
}

#[tokio::test]
async fn the_endpoint_table_of_section_one() {
    // spec/08-hl7-soap-server.md §1.
    let server = start(&[]).await;
    let a = server.address;

    // GET without ?wsdl → 405.
    let answer = raw(
        a,
        b"GET /soap HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(answer.status, 405);
    assert!(
        answer.body.contains("SOAP endpoint accepts POST only."),
        "{}",
        answer.body
    );

    // POST to another path → 404.
    let answer = raw(
        a,
        b"POST /other HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(answer.status, 404);
    assert!(
        answer.body.contains("Unknown SOAP endpoint path."),
        "{}",
        answer.body
    );

    server.stop().await;
}

#[tokio::test]
async fn the_wsdl_is_served_in_both_request_target_forms() {
    // spec/08-hl7-soap-server.md §1.2 and §8.6. Azure Container Apps
    // ingress forwards absolute-form targets, and splitting on '?' to find
    // the path would take `https://host/soap` as the path and answer 404 to
    // every WSDL request in production while passing every local test.
    let server = start(&[]).await;
    let a = server.address;

    let origin = raw(
        a,
        b"GET /soap?wsdl HTTP/1.1\r\nHost: lims.example\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(origin.status, 200, "origin-form: {}", origin.headers);
    assert!(
        origin.body.contains("<wsdl:definitions") || origin.body.contains("<definitions"),
        "{}",
        origin.body
    );

    let absolute = raw(
        a,
        b"GET http://lims.example/soap?wsdl HTTP/1.1\r\nHost: lims.example\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(absolute.status, 200, "absolute-form: {}", absolute.headers);
    assert!(absolute.body.contains("lims.example"), "{}", absolute.body);

    server.stop().await;
}

#[tokio::test]
async fn the_wsdl_address_follows_x_forwarded_proto() {
    // spec/08-hl7-soap-server.md §6.2 — behind an ingress the connection
    // into this process is plaintext even when the client used HTTPS, so
    // without this the WSDL advertises an address nothing can reach.
    let server = start(&[]).await;

    let answer = raw(
        server.address,
        b"GET /soap?wsdl HTTP/1.1\r\nHost: lims.example\r\nX-Forwarded-Proto: https\r\nConnection: close\r\n\r\n",
    )
    .await;

    assert_eq!(answer.status, 200);
    assert!(
        answer.body.contains("https://lims.example/soap"),
        "the advertised address should be the client's, not the socket's: {}",
        answer.body
    );

    server.stop().await;
}

#[tokio::test]
async fn an_xml_bomb_is_refused_without_expanding() {
    // spec/08-hl7-soap-server.md §3.5 and §8.7. The billion-laughs attack:
    // ten nested entities, each referencing the previous ten times. A
    // parser that expands them allocates gigabytes from a request of a few
    // hundred bytes.
    let server = start(&[]).await;

    let bomb = concat!(
        r#"<?xml version="1.0"?><!DOCTYPE lolz ["#,
        r#"<!ENTITY lol "lol">"#,
        r#"<!ENTITY lol1 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">"#,
        r#"<!ENTITY lol2 "&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;">"#,
        r#"<!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">"#,
        r#"<!ENTITY lol4 "&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;">"#,
        r#"<!ENTITY lol5 "&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;">"#,
        r#"<!ENTITY lol6 "&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;">"#,
        r#"<!ENTITY lol7 "&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;">"#,
        r#"<!ENTITY lol8 "&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;">"#,
        r#"<!ENTITY lol9 "&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;">"#,
        r#"]><Envelope><Body><ADT_A05>&lol9;</ADT_A05></Body></Envelope>"#,
    );

    let started = std::time::Instant::now();
    let answer = post(server.address, bomb).await;
    let elapsed = started.elapsed();

    assert!(
        answer.status >= 400,
        "the bomb must be refused, not processed: {}",
        answer.status
    );
    // The real assertion. A parser that expanded this would take far longer
    // than a second and allocate gigabytes; one that does not is fast.
    assert!(
        elapsed < Duration::from_secs(5),
        "expansion appears to have happened: {elapsed:?}"
    );
    assert!(
        server.bus.all_sent(EGRESS).is_empty(),
        "and nothing forwarded"
    );

    server.stop().await;
}

#[tokio::test]
async fn an_external_entity_is_not_resolved() {
    // spec/08-hl7-soap-server.md §3.5 and §8.7 — XXE. If the parser
    // resolved this, the response would carry the contents of a file from
    // the container, which is how a SOAP endpoint becomes a file server.
    let server = start(&[]).await;

    let xxe = concat!(
        r#"<?xml version="1.0"?>"#,
        r#"<!DOCTYPE foo [<!ENTITY xxe SYSTEM "file:///etc/passwd">]>"#,
        r#"<Envelope><Body><ADT_A05>&xxe;</ADT_A05></Body></Envelope>"#,
    );

    let answer = post(server.address, xxe).await;

    assert!(answer.status >= 400, "refused: {}", answer.status);
    assert!(
        !answer.body.contains("root:"),
        "no file content may appear in the response: {}",
        answer.body
    );
    assert_eq!(server.bus.all_sent(EGRESS), [] as [ihb_bus::Incoming; 0]);

    server.stop().await;
}

#[tokio::test]
async fn every_fault_of_section_three_one() {
    // spec/08-hl7-soap-server.md §3.1 and §8.1 — status and fault string,
    // for each row that can be reached from outside.
    let server = start(&[]).await;
    let a = server.address;

    let cases: [(&str, u16, &str); 5] = [
        ("not xml at all", 400, "Malformed SOAP XML request."),
        ("<NotAnEnvelope/>", 400, "SOAP Envelope element is missing."),
        (
            "<Envelope><Header/></Envelope>",
            400,
            "SOAP Body element is missing.",
        ),
        (
            "<Envelope><Body/></Envelope>",
            400,
            "SOAP Body must contain exactly one business payload element.",
        ),
        (
            "<Envelope><Body><ORU_R01><MSH/></ORU_R01></Body></Envelope>",
            400,
            // §5.3 — the fault string is XML-escaped, so the quotes are
            // `&apos;` on the wire. Asserting the escaped form is asserting
            // what the sender actually receives.
            "Unsupported HL7 message structure &apos;ORU_R01&apos;. \
             Allowed values: ADT_A05, ADT_A39",
        ),
    ];

    for (body, status, fault) in cases {
        let answer = post(a, body).await;
        assert_eq!(answer.status, status, "for {body}: {}", answer.body);
        assert!(answer.body.contains(fault), "for {body}: {}", answer.body);
    }

    server.stop().await;
}

#[tokio::test]
async fn an_unauthorised_assigning_authority_is_forbidden() {
    // spec/08-hl7-soap-server.md §3.1 — a 403, not a 400. The message is
    // well formed; the sender is not permitted.
    let server = start(&[("ALLOWED_ASSIGNING_AUTHORITIES", "999")]).await;

    let answer = post(server.address, LIMS).await;

    assert_eq!(answer.status, 403, "{}", answer.body);
    assert!(
        answer
            .body
            .contains("Assigning authority &apos;328&apos; is not authorised. Allowed values: 999"),
        "{}",
        answer.body
    );
    assert_eq!(server.bus.all_sent(EGRESS), [] as [ihb_bus::Incoming; 0]);

    server.stop().await;
}

#[tokio::test]
async fn a_payload_naming_no_authority_is_refused() {
    // spec/08-hl7-soap-server.md §4.1 — a message that names none of the
    // three cannot be attributed to a system.
    let server = start(&[]).await;

    let anonymous = concat!(
        r#"<Envelope><Body><ADT_A05 xmlns="urn:hl7-org:v2xml">"#,
        r#"<MSH><MSH.1>|</MSH.1><MSH.9><MSG.1>ADT</MSG.1><MSG.2>A28</MSG.2></MSH.9>"#,
        r#"<MSH.10>M1</MSH.10></MSH></ADT_A05></Body></Envelope>"#,
    );

    let answer = post(server.address, anonymous).await;

    assert_eq!(answer.status, 400, "{}", answer.body);
    assert!(
        answer
            .body
            .contains("Unable to determine assigning authority from payload."),
        "{}",
        answer.body
    );

    server.stop().await;
}

#[tokio::test]
async fn the_same_envelope_is_read_in_every_prefix_spelling() {
    // spec/08-hl7-soap-server.md §3.2 and §8.2. Four sending stacks, four
    // spellings, one meaning.
    for prefix in ["soapenv:", "soap:", "SOAP-ENV:", ""] {
        let server = start(&[]).await;

        let request = LIMS
            .replace("SOAP-ENV:", prefix)
            .replace("xmlns:=", "xmlns:SOAP-ENV=");

        let answer = post(server.address, &request).await;

        assert_eq!(
            answer.status, 200,
            "prefix {prefix:?} should be accepted: {}",
            answer.body
        );
        assert_eq!(server.bus.all_sent(EGRESS).len(), 1, "prefix {prefix:?}");

        server.stop().await;
    }
}

#[tokio::test]
async fn a_control_id_with_xml_metacharacters_is_escaped() {
    // spec/08-hl7-soap-server.md §5.3 and §8.4. An unescaped control id
    // would let a sender inject elements into the response envelope.
    let dangerous = r#"<&"'>"#;
    let escaped = hl7_2_soap::response::success_in(dangerous, "urn:inthub:hl7-soap-server");

    // All five characters §5.3 names, each in its escaped form.
    let element = escaped
        .split("<MessageControlId>")
        .nth(1)
        .and_then(|rest| rest.split("</MessageControlId>").next())
        .expect("the response carries a control id");

    assert_eq!(
        element, "&lt;&amp;&quot;&apos;&gt;",
        "every metacharacter is escaped, so nothing a sender supplies can \
         become markup in the response"
    );

    // And the raw forms are nowhere in the element.
    for raw in ['<', '>', '&', '"', '\''] {
        assert!(
            !element.contains(raw) || raw == '&',
            "{raw} appears unescaped: {element}"
        );
    }
}
