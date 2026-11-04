//! The client against a local listener.
//!
//! Implements `spec/09-transformers.md` §9.7: the Python's service tests,
//! over a real socket rather than a mocked `requests`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wrds_service::{Client, Error, Row, to_code};

const RESPONSE: &str = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><GetResultSetResponse xmlns="http://www.wales.nhs.uk/namespaces/MessageRelease2"><Row><AttributeValuePair><Attribute><Name>FromCode</Name></Attribute><Value>1</Value></AttributeValuePair><AttributeValuePair><Attribute><Name>Type</Name></Attribute><Value>Sex</Value></AttributeValuePair><AttributeValuePair><Attribute><Name>ToCode</Name></Attribute><Value>M</Value></AttributeValuePair></Row></GetResultSetResponse></soap:Body></soap:Envelope>"#;

/// Serve one canned reply; returns the address and a handle yielding the
/// raw request that arrived.
async fn serve(
    status: &'static str,
    body: &'static str,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0_u8; 4096];
        while !String::from_utf8_lossy(&seen).contains("</soap:Envelope>") {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            seen.extend_from_slice(&buffer[..read]);
        }
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&seen).into_owned()
    });
    (format!("http://{address}/wrdssoapservice"), handle)
}

fn client(endpoint: String, timeout: Duration) -> Client {
    Client::new(endpoint, timeout, None, None).unwrap()
}

async fn lookup(client: &Client) -> Result<Vec<Row>, Error> {
    client
        .get_result_set(
            "FioranoCodeTranslation",
            &[("FromSystem", "349"), ("ToSystem", "100")],
            &["FromCode", "type", "ToCode"],
            true,
        )
        .await
}

#[tokio::test]
async fn a_round_trip_returns_the_rows_and_sends_the_right_request() {
    // §9.2 and §9.3.
    let (endpoint, request) = serve("200 OK", RESPONSE).await;

    let rows = lookup(&client(endpoint, Duration::from_secs(5)))
        .await
        .unwrap();
    assert_eq!(to_code(&rows, "1", "Sex"), "M");

    let request = request.await.unwrap();
    assert!(
        request.starts_with("POST /wrdssoapservice HTTP/1.1"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: text/xml; charset=utf-8"),
        "{request}"
    );
    assert!(request.contains("<Value>349</Value>"));
    assert!(request.contains("<Name>FioranoCodeTranslation</Name>"));
}

#[tokio::test]
async fn a_status_other_than_200_is_an_error() {
    // §9.3.
    let (endpoint, _) = serve("500 Internal Server Error", "").await;
    let error = lookup(&client(endpoint, Duration::from_secs(5)))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Status(500)), "{error}");
}

#[tokio::test]
async fn a_server_that_never_answers_times_out() {
    // §9.5. The listener accepts and then says nothing.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/x", listener.local_addr().unwrap());
    let _hold = tokio::spawn(async move {
        let _connection = listener.accept().await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let error = lookup(&client(endpoint, Duration::from_secs(1)))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Timeout(1)), "{error}");
}

#[tokio::test]
async fn a_refused_connection_is_a_connection_error() {
    // §9.5.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/x", listener.local_addr().unwrap());
    drop(listener);

    let error = lookup(&client(endpoint, Duration::from_secs(5)))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Connection(_)), "{error}");
}

#[tokio::test]
async fn fixture_mode_reads_the_file_and_makes_no_call() {
    // §9.6 — the endpoint is unreachable and that does not matter.
    let dir = std::env::temp_dir().join(format!("wrds-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fixture.xml");
    std::fs::write(&path, RESPONSE).unwrap();

    let client = Client::new(
        "http://127.0.0.1:1/unreachable",
        Duration::from_secs(1),
        None,
        Some(path),
    )
    .unwrap();
    let rows = lookup(&client).await.unwrap();
    assert_eq!(
        to_code(&rows, "1", "Sex"),
        "M",
        "filters are not applied (§9.6)"
    );

    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn a_missing_fixture_is_an_error() {
    // §9.6.
    let client = Client::new(
        "http://127.0.0.1:1/x",
        Duration::from_secs(1),
        None,
        Some("/no/such/fixture.xml".into()),
    )
    .unwrap();
    assert!(matches!(lookup(&client).await, Err(Error::Fixture(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_blocking_form_works_inside_a_multi_thread_runtime() {
    // §9.1 — how the synchronous transformer trait calls this.
    let (endpoint, _) = serve("200 OK", RESPONSE).await;
    let client = client(endpoint, Duration::from_secs(5));

    let rows = client
        .get_result_set_blocking("t", &[("a", "b")], &["ToCode"], true)
        .unwrap();
    assert_eq!(rows.len(), 1);
}

#[test]
fn the_blocking_form_works_with_no_runtime_at_all() {
    // A private runtime stands in (a plain unit test).
    let client = Client::new(
        "http://127.0.0.1:1/x",
        Duration::from_secs(1),
        None,
        Some("/no/such/fixture.xml".into()),
    )
    .unwrap();
    assert!(matches!(
        client.get_result_set_blocking("t", &[], &[], true),
        Err(Error::Fixture(_))
    ));
}

fn row(pairs: &[(&str, &str)]) -> Row {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn to_code_compares_names_case_insensitively_and_values_exactly() {
    // §9.4.
    let rows = vec![row(&[
        ("fromcode", "EN"),
        ("TYPE", "Language"),
        ("ToCode", "eng"),
    ])];
    assert_eq!(to_code(&rows, "EN", "Language"), "eng");
    assert_eq!(to_code(&rows, "en", "Language"), "", "values are exact");
    assert_eq!(to_code(&rows, "EN", "language"), "", "values are exact");
}

#[test]
fn to_code_returns_empty_for_no_match_or_an_empty_to_code() {
    // §9.4.
    assert_eq!(to_code(&[], "1", "Sex"), "");
    let rows = vec![row(&[("FromCode", "1"), ("Type", "Sex"), ("ToCode", "")])];
    assert_eq!(to_code(&rows, "1", "Sex"), "");
}

#[test]
fn to_code_takes_the_first_matching_row() {
    // §9.4.
    let rows = vec![
        row(&[("FromCode", "1"), ("Type", "Sex"), ("ToCode", "M")]),
        row(&[("FromCode", "1"), ("Type", "Sex"), ("ToCode", "X")]),
    ];
    assert_eq!(to_code(&rows, "1", "Sex"), "M");
}
