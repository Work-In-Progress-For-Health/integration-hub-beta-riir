//! `fhir-sender` against the Promptly mock, end to end.
//!
//! Implements `spec/11-mock-receivers.md` §3.10 and `spec/10-senders.md` §8.8.
//!
//! The mock and the sender were written from the same upstream description
//! of the IG's asynchronous pattern and have never been shown to agree. The
//! real client, over a real socket, against the real mock, for each scenario
//! the mock offers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use fhir_sender::auth::Auth;
use fhir_sender::client::{FhirClient, Settings, TokioClock};
use fhir_sender::error::DeliveryError;
use fhir_sender::http::HyperTransport;
use http_mock_receiver::fhir::FhirMock;
use http_mock_receiver::server::{Receiver, serve};
use ihb_bus::fake::FakeBus;
use ihb_config::Secret;
use ihb_runtime::shutdown::Shutdown;
use tokio::net::TcpStream;

const QUEUE: &str = "fhir-egress";

struct Mock {
    base: String,
    bus: FakeBus,
    shutdown: Shutdown,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Mock {
    async fn stop(self) {
        self.shutdown.stop();
        self.task.await.unwrap().unwrap();
    }
}

async fn mock() -> Mock {
    let bus = FakeBus::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let receiver = Arc::new(Receiver {
        sink: Some(bus.sender(QUEUE)),
        session_id: Some("wpas-to-proms".to_string()),
        fhir: FhirMock::new(),
    });
    let shutdown = Shutdown::new();
    let watcher = shutdown.watcher();
    let task = tokio::spawn(async move { serve(address, receiver, watcher).await });
    for _ in 0..200 {
        if TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Mock {
        base: format!("http://{address}"),
        bus,
        shutdown,
        task,
    }
}

fn client(base: &str) -> FhirClient<HyperTransport, TokioClock> {
    FhirClient::new(
        &format!("{base}/fhir/r4/$process-message"),
        Auth::api_key(Secret::new("local-dev-key")).unwrap(),
        Settings {
            allow_insecure_http: true,
            backoff_seconds: 0.01,
            ..Settings::default()
        },
        HyperTransport::new().unwrap(),
        TokioClock::new(),
    )
    .unwrap()
}

fn bundle(id: &str) -> String {
    format!(r#"{{"resourceType":"Bundle","type":"message","id":"{id}"}}"#)
}

#[tokio::test]
async fn an_accepted_bundle_is_polled_to_success_and_forwarded_once() {
    let mock = mock().await;

    client(&mock.base)
        .send_bundle(&bundle("b-1"))
        .await
        .unwrap();

    let forwarded = mock.bus.receive(QUEUE, 10);
    assert_eq!(forwarded.len(), 1);
    assert_eq!(String::from_utf8_lossy(&forwarded[0].body), bundle("b-1"));
    mock.stop().await;
}

#[tokio::test]
async fn a_rejected_post_is_permanent_and_is_not_forwarded() {
    let mock = mock().await;

    let error = client(&mock.base)
        .send_bundle(&bundle("mock-400"))
        .await
        .unwrap_err();

    assert!(
        matches!(error, DeliveryError::Permanent { status: 400, .. }),
        "{error:?}"
    );
    assert_eq!(mock.bus.receive(QUEUE, 10).len(), 0);
    mock.stop().await;
}

#[tokio::test]
async fn an_error_outcome_after_polling_is_permanent_even_though_the_post_was_accepted() {
    let mock = mock().await;

    let error = client(&mock.base)
        .send_bundle(&bundle("mock-error-outcome"))
        .await
        .unwrap_err();

    assert!(
        matches!(&error, DeliveryError::Permanent { detail, .. } if detail.contains("Patient could not be matched")),
        "{error:?}"
    );
    mock.stop().await;
}

#[tokio::test]
async fn a_credential_rejection_is_an_authorization_failure() {
    let mock = mock().await;
    let error = client(&mock.base)
        .send_bundle(&bundle("mock-403"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, DeliveryError::Authorization { status: 403, .. }),
        "{error:?}"
    );
    mock.stop().await;
}

#[tokio::test]
async fn a_failure_that_clears_on_retry_is_delivered_and_forwarded_once() {
    // The README's point: only the accepted POST is forwarded.
    let mock = mock().await;

    client(&mock.base)
        .send_bundle(&bundle("mock-500-once"))
        .await
        .unwrap();

    assert_eq!(mock.bus.receive(QUEUE, 10).len(), 1);
    mock.stop().await;
}

#[tokio::test]
async fn the_percent_encoded_operation_url_reaches_the_fhir_mock_not_the_soap_endpoint() {
    // The local env files write `$` as `%24` for Compose; the real sender
    // sends it that way (found by the wpas-to-proms compose run).
    let mock = mock().await;
    let mut client = FhirClient::new(
        &format!("{}/fhir/r4/%24process-message", mock.base),
        Auth::api_key(Secret::new("k")).unwrap(),
        Settings {
            allow_insecure_http: true,
            ..Settings::default()
        },
        HyperTransport::new().unwrap(),
        TokioClock::new(),
    )
    .unwrap();

    client.send_bundle(&bundle("b-enc")).await.unwrap();

    assert_eq!(mock.bus.receive(QUEUE, 10).len(), 1);
    mock.stop().await;
}
