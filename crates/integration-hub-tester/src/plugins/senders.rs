//! The senders: MLLP framing, acknowledgement classification, SOAP.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §4.9 to §4.11.

use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use ihb_sender::Verdict;
use tokio::net::TcpStream;

use super::block_on;
use crate::plugin::{Outcome, Plugin, Sample, heading, summary_lines, to_er7};

// `VT`, `FS`, `CR`: `spec/10-senders.md` §3.2; HL7 Appendix C.
/// MLLP start block (`VT`), written before the payload.
const MLLP_START: u8 = 0x0B;
/// MLLP end block (`FS`), written after the payload and before [`MLLP_CR`].
const MLLP_END: u8 = 0x1C;
/// MLLP trailing carriage return, the last byte of a frame.
const MLLP_CR: u8 = 0x0D;

/// Synthetic sample `SAMPLE_A01` (`spec/integration-hub-tester-component-preview.md` §5.1).
const SAMPLE_A01: &str = "\
MSH|^~\\&|SENDAPP|SENDFAC|RECVAPP|RECVFAC|20250703120000||ADT^A01^ADT_A01|MSG000001|P|2.5
EVN||20250703120000
PID|||9990000018^^^^NH||TEST^TEST^^^Mr||19800115|M|||10 TEST STREET^TESTTOWN^^CF10 1AA||02920000001^PRN
PV1||I|WARD1^ROOM2^BED3";

/// Synthetic sample `SAMPLE_A28` (`spec/integration-hub-tester-component-preview.md` §5.1).
const SAMPLE_A28: &str = "\
MSH|^~\\&|PIMS|BroMor HL7Sender|MPI|MPI|20250703120000||ADT^A28^ADT_A01|MSG000002|P|2.5
EVN||20250703120000
PID|||9990000026^^^^NH~SB001^^^^PI||TEST^PERSON^^^Mr||19800115|M|||1 TEST STREET^^TESTTOWN^SA1 1AA
PD1||||G7777777
PV1||U";

/// Synthetic sample `SAMPLE_FAIL` (`spec/integration-hub-tester-component-preview.md` §5.1).
const SAMPLE_FAIL: &str = "\
MSH|^~\\&|SENDAPP|SENDFAC|RECVAPP|RECVFAC|20250703120000||ADT^A01^ADT_A01|MSG_FAIL_01|P|2.5
EVN||20250703120000
PID|||fail^^^^NH||TRIGGER^FAULT^^^Mr||19800115|M
PV1||U";

/// The MLLP frame of `payload` (`spec/integration-hub-tester-component-preview.md` §4.9).
#[must_use]
pub fn mllp_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 3);
    frame.push(MLLP_START);
    frame.extend_from_slice(payload);
    frame.push(MLLP_END);
    frame.push(MLLP_CR);
    frame
}

/// `hl7-sender` (§4.9): shows the MLLP frame the sender would write.
///
/// Never opens a socket; the frame is built locally and described, so the
/// preview is safe to run with no receiver listening.
#[derive(Debug, Default, Clone, Copy)]
pub struct Hl7SenderPlugin;

impl Plugin for Hl7SenderPlugin {
    fn id(&self) -> &'static str {
        "hl7-sender"
    }
    fn tab_label(&self) -> &'static str {
        "HL7 Sender"
    }
    fn description(&self) -> &'static str {
        "Preview the MLLP frame the sender would transmit downstream (no connection made)"
    }
    fn input_label(&self) -> &'static str {
        "HL7v2 ER7 (as it would arrive from the Service Bus queue)"
    }
    fn output_label(&self) -> &'static str {
        "MLLP frame preview + message breakdown"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("ADT A01 (Inpatient admit)", SAMPLE_A01),
            ("ADT A28 (New patient)", SAMPLE_A28),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        let er7 = to_er7(input);
        let message = ihb_validation::parse(&er7)
            .map_err(|error| format!("could not parse the message: {error}"))?;
        let payload = er7.as_bytes();
        let frame = mllp_frame(payload);
        let n = payload.len();

        let mut lines = vec![heading("MESSAGE SUMMARY")];
        lines.extend(summary_lines(&message));
        lines.extend([
            String::new(),
            heading(&format!("MLLP BYTE FRAME  (total: {} bytes)", frame.len())),
            String::new(),
            "  Byte 0      : 0x0B  <- MLLP Start Block (VT)".to_string(),
            format!("  Bytes 1-{n}  : HL7 ER7 payload ({n} bytes, UTF-8)"),
            format!("  Byte {}    : 0x1C  <- MLLP End Block (FS)", n + 1),
            format!("  Byte {}    : 0x0D  <- Carriage Return", n + 2),
            String::new(),
            heading("HL7 PAYLOAD  (as transmitted - segment separator = CR 0x0D)"),
            String::new(),
        ]);
        lines.extend(
            er7.split('\r')
                .filter(|segment| !segment.trim().is_empty())
                .map(|segment| format!("  {segment}")),
        );
        lines.extend([
            String::new(),
            heading("MLLP CONTROL BYTES (hex)"),
            "  Start : 0B  (0x0B)".to_string(),
            "  End   : 1C0D  (0x1C 0x0D)".to_string(),
        ]);
        Ok(Outcome::accepted(
            lines.join("\n"),
            format!("MLLP frame preview - {} bytes total", frame.len()),
        ))
    }
}

/// Synthetic sample `ACK_AA` (`spec/integration-hub-tester-component-preview.md` §5.1).
const ACK_AA: &str = "\
MSH|^~\\&|MPI|MPI|PIMS|BroMor HL7Sender|20250703120005||ACK^A28^ACK|MSG000002|P|2.5
MSA|AA|MSG000002";

/// Synthetic sample `NACK_AE` (`spec/integration-hub-tester-component-preview.md` §5.1).
const NACK_AE: &str = "\
MSH|^~\\&|MPI|MPI|PIMS|BroMor HL7Sender|20250703120005||ACK^A28^ACK|MSG000002|P|2.5
MSA|AE|MSG000002|Temporary application error - please retry";

/// Synthetic sample `NACK_AR` (`spec/integration-hub-tester-component-preview.md` §5.1).
const NACK_AR: &str = "\
MSH|^~\\&|MPI|MPI|PIMS|BroMor HL7Sender|20250703120005||ACK^A28^ACK|MSG000002|P|2.5
MSA|AR|MSG000002|Message rejected - patient not found";

/// `hl7-sender-ack` (§4.10).
///
/// Feeds a pasted reply to the sender's real ACK evaluation and reports what the sender would do with it. `ok` is true only
/// for `SUCCESS`; an `AE`, `AR` or unparseable reply is a rejection.
#[derive(Debug, Default, Clone, Copy)]
pub struct Hl7SenderAckPlugin;

impl Plugin for Hl7SenderAckPlugin {
    fn id(&self) -> &'static str {
        "hl7-sender-ack"
    }
    fn tab_label(&self) -> &'static str {
        "HL7 Sender ACK"
    }
    fn description(&self) -> &'static str {
        "Classify a received ACK/NACK the way hl7-sender does: SUCCESS (AA/CA), AE (recoverable, \
         retried), AR (non-recoverable, dead-lettered), INVALID"
    }
    fn input_label(&self) -> &'static str {
        "Simulated ACK/NACK response (as received back over MLLP)"
    }
    fn output_label(&self) -> &'static str {
        "Classification"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("AA - success", ACK_AA),
            ("AE - recoverable, retried", NACK_AE),
            ("AR - non-recoverable, dead-lettered", NACK_AR),
            ("Malformed response", "NOT A VALID HL7 ACK RESPONSE"),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        use ihb_sender::NackClass;

        let reply = to_er7(input);
        let verdict = hl7_sender::evaluate(&reply);
        let control_id =
            ihb_validation::parse(&reply).map_or_default(|m| ihb_fields::read(&m, "MSH-10"));

        let (label, code, behaviour) = match &verdict {
            Verdict::Accepted { .. } => (
                "SUCCESS",
                ihb_validation::parse(&reply).map_or_default(|m| ihb_fields::read(&m, "MSA-1")),
                "Send treated as successful - no retry.",
            ),
            Verdict::Nack { class, code, .. } => (
                class.label(),
                code.clone().unwrap_or_default(),
                match class {
                    NackClass::Recoverable => {
                        "Recoverable - the message will be retried (redelivered) until \
                         MaxDeliveryCount."
                    }
                    NackClass::Reject => {
                        "Non-recoverable - the message is dead-lettered, not retried."
                    }
                    NackClass::Invalid => {
                        "Malformed or unparsable response - treated as a recoverable failure \
                         (same handling path as AE)."
                    }
                },
            ),
            Verdict::Rejected { .. } => (
                "REJECTED",
                String::new(),
                "The sender abandons the message.",
            ),
        };

        let output = [
            heading("ACK/NACK CLASSIFICATION"),
            format!("  Outcome        : {label}"),
            format!(
                "  MSA-1 ack code : {}",
                if code.is_empty() {
                    "(none - unparsable/missing MSA)"
                } else {
                    &code
                }
            ),
            format!(
                "  Control ID     : {}",
                if control_id.is_empty() {
                    "(none)"
                } else {
                    &control_id
                }
            ),
            format!("  is_success     : {}", verdict.is_accepted()),
            String::new(),
            behaviour.to_string(),
        ]
        .join("\n");
        Ok(if verdict.is_accepted() {
            Outcome::accepted(output, label)
        } else {
            Outcome::rejected(output, label)
        })
    }
}

/// The URL used when `SOAP_MOCK_URL` is unset (§4.11): where
/// `integration-hub-tester mock soap` listens.
pub const DEFAULT_MOCK_URL: &str = "http://localhost:8080/soap";

/// How long the post may take (`spec/integration-hub-tester-component-preview.md` §4.11).
const POST_TIMEOUT: Duration = Duration::from_secs(10);

/// Which of the two SOAP tabs this is. They differ only in the banner
/// (§4.11): the Python kept two files to show both senders produce one
/// envelope, and the ids are kept for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoapPath {
    /// Queue ingress.
    Queue,
    /// Topic subscription ingress.
    Subscription,
}

/// `soap-sender` and `soap-subscription-sender` (§4.11): shows the envelope
/// and posts it to a mock receiver.
///
/// The only plugin that touches the network, and only an `http` URL is
/// attempted (§4.11.1); the URL is operator-typed, not patient data.
#[derive(Debug, Clone)]
pub struct SoapSenderPlugin {
    /// Which tab.
    path: SoapPath,
    /// Where to post.
    url: String,
}

impl SoapSenderPlugin {
    /// The queue sender, posting to `SOAP_MOCK_URL` or the default.
    #[must_use]
    pub fn queue() -> Self {
        Self::new(SoapPath::Queue)
    }

    /// The subscription sender, posting to `SOAP_MOCK_URL` or the default.
    #[must_use]
    pub fn subscription() -> Self {
        Self::new(SoapPath::Subscription)
    }

    /// A sender for `path`, posting where `SOAP_MOCK_URL` says.
    fn new(path: SoapPath) -> Self {
        let url = std::env::var("SOAP_MOCK_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MOCK_URL.to_string());
        Self { path, url }
    }

    /// Post somewhere else; for tests, so a local listener can stand in for
    /// the mock receiver without touching the process environment.
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }
}

/// `POST` one envelope. `Err` is a connection-level failure, with the hint
/// of §4.11; an HTTP error status is still `Ok`, because a SOAP fault
/// rides on a 500 and is the thing being previewed.
async fn post(url: &str, body: String) -> Result<(u16, String), String> {
    let uri: hyper::Uri = url.parse().map_err(|e| format!("bad URL: {e}"))?;
    // Upstream (`soap_sender_plugin._post_to_mock`, tester change after
    // `9e797ac`, Bandit B310): only http and https are accepted, so a typed
    // `file://` or `ftp://` URL is refused rather than opened. Here https is
    // refused too, separately, because this client speaks plain HTTP/1.1
    // only. The URL is operator-typed, not patient data, so it may appear in
    // the message.
    match uri.scheme_str() {
        Some("http") => {}
        Some("https") => {
            return Err("https endpoints are not supported by the tester".to_string());
        }
        _ => {
            return Err(format!(
                "Unsupported URL scheme (only http/https allowed): {url}"
            ));
        }
    }
    let host = uri.host().ok_or("the URL names no host")?.to_string();
    let port = uri.port_u16().unwrap_or(80);

    let refused = |e: &dyn std::fmt::Display| {
        format!("Connection failed - is the SOAP mock receiver running? ({e})")
    };

    let stream = tokio::time::timeout(POST_TIMEOUT, TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| "Request timed out after 10 seconds.".to_string())?
        .map_err(|e| refused(&e))?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| refused(&e))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let request = Request::builder()
        .method("POST")
        // Origin-form, as HTTP/1.1 servers expect outside a proxy.
        .uri(
            uri.path_and_query()
                .map_or("/", hyper::http::uri::PathAndQuery::as_str),
        )
        .header(hyper::header::CONTENT_TYPE, "text/xml; charset=utf-8")
        .header(hyper::header::HOST, host)
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| e.to_string())?;

    let exchange = async {
        let response = sender
            .send_request(request)
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        Ok::<_, String>((status, String::from_utf8_lossy(&bytes).into_owned()))
    };
    tokio::time::timeout(POST_TIMEOUT, exchange)
        .await
        .map_err(|_| "Request timed out after 10 seconds.".to_string())?
}

impl Plugin for SoapSenderPlugin {
    fn id(&self) -> &'static str {
        match self.path {
            SoapPath::Queue => "soap-sender",
            SoapPath::Subscription => "soap-subscription-sender",
        }
    }
    fn tab_label(&self) -> &'static str {
        match self.path {
            SoapPath::Queue => "SOAP Sender",
            SoapPath::Subscription => "SOAP Sub Sender",
        }
    }
    fn description(&self) -> &'static str {
        match self.path {
            SoapPath::Queue => {
                "Send HL7 wrapped in a SOAP envelope to the mock receiver - start it first with \
                 `mock soap`"
            }
            SoapPath::Subscription => {
                "Send HL7 wrapped in a SOAP envelope (subscription sender path) - start the mock \
                 receiver first with `mock soap`"
            }
        }
    }
    fn input_label(&self) -> &'static str {
        match self.path {
            SoapPath::Queue => "HL7v2 ER7 (as it would arrive from the Service Bus queue)",
            SoapPath::Subscription => {
                "HL7v2 ER7 (as it would arrive from the Service Bus topic subscription)"
            }
        }
    }
    fn output_label(&self) -> &'static str {
        "SOAP envelope + HTTP response"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("ADT A01 (Inpatient admit)", SAMPLE_A01),
            ("ADT A28 (New patient)", SAMPLE_A28),
            ("Fault trigger (contains 'fail')", SAMPLE_FAIL),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        let er7 = to_er7(input);
        let message = ihb_validation::parse(&er7)
            .map_err(|error| format!("could not parse the message: {error}"))?;
        let control_id = ihb_fields::read(&message, "MSH-10");

        // The real envelope, not a private copy (§3.1).
        let envelope = soap_sender::envelope::wrap(&er7);

        let mut lines = vec![heading("HL7 MESSAGE SUMMARY")];
        lines.extend(summary_lines(&message));
        lines.extend([
            String::new(),
            heading(&format!("SOAP ENVELOPE SENT  ->  {}", self.url)),
            envelope.clone(),
            String::new(),
        ]);

        match block_on(post(&self.url, envelope))? {
            Err(reason) => {
                lines.extend([
                    heading("MOCK RECEIVER NOT REACHABLE"),
                    format!("  {reason}"),
                    String::new(),
                    "  Start the SOAP mock receiver (`integration-hub-tester mock soap`),"
                        .to_string(),
                    "  then run this again.".to_string(),
                ]);
                Ok(Outcome::rejected(
                    lines.join("\n"),
                    format!("mock receiver not reachable - {reason}"),
                ))
            }
            Ok((status, body)) => {
                let verdict = soap_sender::envelope::evaluate(status, &body);
                let accepted = verdict.is_accepted();
                let label = if accepted {
                    "AA - message accepted"
                } else {
                    "FAULT - message rejected"
                };
                lines.extend([
                    heading(&format!("SOAP RESPONSE  (HTTP {status})  -  {label}")),
                    body,
                ]);
                let summary = format!("HTTP {status} - {label} | control_id={control_id}");
                Ok(if accepted {
                    Outcome::accepted(lines.join("\n"), summary)
                } else {
                    Outcome::rejected(lines.join("\n"), summary)
                })
            }
        }
    }
}
