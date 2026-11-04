//! The ingress servers: MLLP validation, the REST HL7 pipeline, and SOAP.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §4.6 to §4.8.

use hl7_server::ack;
use hl7_server::validate::{Rejection, check_fields, is_accepted_message_type, message_type_of};
use ihb_bus::FakeBus;

use super::block_on;
use crate::plugin::{Outcome, Plugin, Sample, display, heading, summary_lines, to_er7};

/// Synthetic sample `VALID_A28` (`spec/integration-hub-tester-component-preview.md` §5.1).
const VALID_A28: &str = "\
MSH|^~\\&|252|252|100|100|20250505232328||ADT^A28^ADT_A05|202505052323326666666666|P|2.5|||||GBR||EN
EVN||20250502102000|20250505232328|||20250505232328
PID|||8888888^^^252^PI~9990000018^^^NHS^NH||TESTSURNAME^TESTFIRST^TESTMIDDLE^^MR||19870101|M|||ADDRESS1^ADDRESS2^ADDRESS3^ADDRESS4^XX99 9XX^^H|||||||||||||||01
PD1|||^^W99999^|G7777777
PV1||U";

/// Synthetic sample `VALID_A31` (`spec/integration-hub-tester-component-preview.md` §5.1).
const VALID_A31: &str = "\
MSH|^~\\&|192|192|200|200|20250624161510||ADT^A31|369913945290925|P|2.5|||NE|NE
EVN|Sub|20250624161510
PID|1|1000000001^^^^NH|1000000001^^^^NH~B1000001^^^^PAS||TEST^TEST^^^Mrs.||20000101|F|||1 TEST^TEST^TEST^TEST^CF11 9AD||01000000001^PRN|01000000001^WPN||||||||||||||||||1
PD1||||G7000001
PV1||U";

/// Synthetic sample `WRONG_VERSION` (`spec/integration-hub-tester-component-preview.md` §5.1).
const WRONG_VERSION: &str = "\
MSH|^~\\&|192|192|200|200|20250624161510||ADT^A31|369913945290925|P|2.3|||NE|NE
EVN|Sub|20250624161510
PID|1|1000000001^^^^NH||TEST^TEST|||F
PV1||U";

/// Synthetic sample `UNPARSABLE` (`spec/integration-hub-tester-component-preview.md` §5.1).
const UNPARSABLE: &str = "THIS IS NOT AN HL7 MESSAGE AT ALL";

/// `hl7-server` (§4.6): the ingress pipeline's parse, allow-list and
/// field-check steps in order, ending in the ACK or NACK the sender would see.
/// Nothing is persisted or published.
#[derive(Debug, Default, Clone, Copy)]
pub struct Hl7ServerPlugin;

/// The version the preview checks against: the Python's
/// `HL7Validator(hl7_version="2.5")`.
const PREVIEW_VERSION: &str = "2.5";

impl Hl7ServerPlugin {
    /// The NACK block for a rejection (`spec/07-hl7-server.md` §4.3).
    fn nack_block(
        message: Option<&hl7_2::Message>,
        rejection: &Rejection,
        now: &str,
    ) -> Vec<String> {
        let nack = match message {
            Some(m) if !ihb_fields::read(m, "MSH-10").is_empty() => {
                ack::build_nack(m, rejection.ack_code, &rejection.reason, now)
            }
            _ => ack::build_generic_nack(&rejection.reason, None, now),
        };
        vec![display(&nack)]
    }
}

impl Plugin for Hl7ServerPlugin {
    fn id(&self) -> &'static str {
        "hl7-server"
    }
    fn tab_label(&self) -> &'static str {
        "HL7 Server"
    }
    fn description(&self) -> &'static str {
        "Validate an inbound HL7v2 message and preview the ACK (AA) / NACK (AR validation \
         failure, AE parse or unexpected error) the server would return"
    }
    fn input_label(&self) -> &'static str {
        "Inbound HL7v2 ER7 (as received by the MLLP server)"
    }
    fn output_label(&self) -> &'static str {
        "Validation result + ACK/NACK preview"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("Valid A28 (v2.5)", VALID_A28),
            ("Valid A31 (v2.5)", VALID_A31),
            ("Wrong version (A31 v2.3) -> AR NACK", WRONG_VERSION),
            ("Unparsable -> AE NACK (generic)", UNPARSABLE),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        let er7 = to_er7(input);
        let now = ack::timestamp();
        let mut lines: Vec<String> = Vec::new();

        let message = match ihb_validation::parse(&er7) {
            Ok(message) => message,
            Err(error) => {
                // `spec/07-hl7-server.md` §7.2.
                let rejection = Rejection::unparseable(format!("Invalid HL7 Message: {error}"));
                lines.push(heading("PARSE FAILURE"));
                lines.push(format!("  x  {}", rejection.reason));
                lines.extend(Self::nack_block(None, &rejection, &now));
                return Ok(Outcome::rejected(
                    lines.join("\n"),
                    "could not parse - AE NACK (generic) preview generated",
                ));
            }
        };

        lines.push(heading("PARSED MESSAGE SUMMARY"));
        lines.extend(summary_lines(&message));

        lines.push(String::new());
        lines.push(heading(
            "VALIDATION  (no flow-specific rules - generic server check)",
        ));

        // `spec/07-hl7-server.md` §7.1 first, as the pipeline does, then
        // the field layers of §3 with the version the Python preview used.
        let (code, trigger, structure) = message_type_of(&message);
        let checked = if is_accepted_message_type(&code, &trigger, &structure) {
            check_fields(&message, Some(PREVIEW_VERSION), &[], None)
        } else {
            Err(Rejection::unsupported_message_type(format!(
                "Unsupported Message Type: {code}^{trigger}^{structure}"
            )))
        };

        if let Err(rejection) = checked {
            lines.push(format!("  x  Validation failed: {}", rejection.reason));
            lines.push(String::new());
            lines.push(heading(&format!(
                "NACK THAT WOULD BE RETURNED  ({})",
                if rejection.ack_code == ack::REJECT {
                    "AR - Application Reject"
                } else {
                    "AE - Application Error"
                }
            )));
            lines.extend(Self::nack_block(Some(&message), &rejection, &now));
            return Ok(Outcome::rejected(
                lines.join("\n"),
                format!(
                    "validation failed - {} NACK preview generated",
                    rejection.ack_code
                ),
            ));
        }

        lines.push("  ok  Message passed all validation checks".to_string());
        lines.push(String::new());
        lines.push(heading(
            "ACK THAT WOULD BE RETURNED  (AA - Application Accept)",
        ));
        lines.push(display(&ack::build(&message, &now)));
        Ok(Outcome::accepted(
            lines.join("\n"),
            "valid - ACK preview generated",
        ))
    }
}

/// `rest-server` (§4.7): runs the REST ingress processor against a fake bus
/// and shows the HTTP status and body a caller would get.
#[derive(Debug, Default, Clone, Copy)]
pub struct RestServerPlugin;

/// The identity the REST preview runs as. Nothing here reaches a bus: the
/// egress is a `FakeBus` entity.
const REST_IDENTITY: [(&str, &str); 6] = [
    ("WORKFLOW_ID", "tester-demo"),
    ("MICROSERVICE_ID", "tester-demo"),
    ("HEALTH_BOARD", "tester"),
    ("PEER_SERVICE", "tester"),
    ("EGRESS_QUEUE_NAME", "tester-demo"),
    ("EGRESS_SESSION_ID", "tester-demo-session"),
];

impl Plugin for RestServerPlugin {
    fn id(&self) -> &'static str {
        "rest-server"
    }
    fn tab_label(&self) -> &'static str {
        "REST Server (HL7)"
    }
    fn description(&self) -> &'static str {
        "POST /hl7MessageReceiver {\"messageContent\": \"...\"} -> HL7 ACK (201) / NACK (422/500) \
         - hl7-rest-server, no flow"
    }
    fn input_label(&self) -> &'static str {
        "Inbound HL7v2 ER7 or v2.xml (the JSON messageContent, shown raw)"
    }
    fn output_label(&self) -> &'static str {
        "HTTP status + ACK/NACK body"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("Valid A28 (v2.5)", VALID_A28),
            ("Malformed (unparsable)", "THIS IS NOT AN HL7 MESSAGE"),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        use hl7_rest_server::config::Config;
        use hl7_rest_server::input::to_er7 as rest_to_er7;
        use hl7_rest_server::process::Processor;
        use ihb_bus::fake::FakeSender;

        let config = Config::from_pairs(REST_IDENTITY).map_err(|e| e.to_string())?;
        let bus = FakeBus::new();
        let processor: Processor<FakeSender, FakeSender> = Processor::new(
            config.identity,
            config.settings,
            bus.sender("tester-demo"),
            None,
            None,
        );

        let body = serde_json::json!({ "messageContent": input.trim() }).to_string();
        let mut lines = vec![
            heading("REQUEST BODY  POST /hl7MessageReceiver"),
            body,
            String::new(),
        ];

        // `spec/19-rest-servers.md` §2.2: line endings, then v2.xml to ER7;
        // a failure here is a 500 with the generic NACK.
        let er7 = match rest_to_er7(input.trim()) {
            Ok(er7) => er7,
            Err(error) => {
                let nack = processor.generic_nack(&error.to_string());
                lines.push(heading("500 Internal Server Error"));
                lines.push(display(&nack));
                return Ok(Outcome::rejected(
                    lines.join("\n"),
                    "500 - could not convert the message",
                ));
            }
        };

        match block_on(processor.process(&er7))? {
            Ok(acknowledgement) => {
                lines.push(heading("201 Created - ACK"));
                lines.push(display(&acknowledgement));
                Ok(Outcome::accepted(
                    lines.join("\n"),
                    "201 - AA ACK generated",
                ))
            }
            Err(failure) => {
                lines.push(heading(&format!("{} - NACK", failure.status)));
                lines.push(display(&failure.nack));
                Ok(Outcome::rejected(
                    lines.join("\n"),
                    format!("{} - {}", failure.status, failure.reason),
                ))
            }
        }
    }
}

/// Synthetic sample `SOAP_VALID` (`spec/integration-hub-tester-component-preview.md` §5.1).
const SOAP_VALID: &str = "\
MSH|^~\\&|328|328|100|100|20260729095037||ADT^A28^ADT_A05|6778031837018553261z82215|P|2.5|||||GBR||EN
EVN|A28|20260729095037|20260729095037|||20260729095037
PID|||B0000010612^^^328^PI||LIMS^TEST
PV1||";

/// Synthetic sample `SOAP_UNKNOWN_AUTHORITY` (`spec/integration-hub-tester-component-preview.md` §5.1).
const SOAP_UNKNOWN_AUTHORITY: &str = "\
MSH|^~\\&|999|999|100|100|20260729095037||ADT^A28^ADT_A05|6778031837018553261z82217|P|2.5|||||GBR||EN
EVN|A28|20260729095037|20260729095037|||20260729095037
PID|||B0000010612^^^999^PI||LIMS^TEST
PV1||";

/// Synthetic sample `SOAP_MALFORMED` (`spec/integration-hub-tester-component-preview.md` §5.1).
const SOAP_MALFORMED: &str = "<not-a-valid-soap-request";

/// `hl7-soap-server` (§4.8): runs the SOAP ingress processor with a fixed
/// PHW configuration and shows the SOAP response or fault.
#[derive(Debug, Default, Clone, Copy)]
pub struct Hl7SoapServerPlugin;

/// The PHW recipe, fixed (§4.8): the environment the SOAP processor is built
/// from. Fixed rather than read from the process so a preview gives the same
/// answer on every machine; the egress names point at nothing real.
const SOAP_CONFIG: [(&str, &str); 10] = [
    ("WORKFLOW_ID", "tester-demo"),
    ("MICROSERVICE_ID", "tester-demo"),
    ("HEALTH_BOARD", "tester"),
    ("PEER_SERVICE", "tester"),
    ("EGRESS_QUEUE_NAME", "tester-demo"),
    ("EGRESS_SESSION_ID", "tester-demo-session"),
    ("MESSAGE_STORE_QUEUE_NAME", "tester-demo-store"),
    ("HL7_SCHEMA_GROUP", "phw"),
    ("ALLOWED_HL7_STRUCTURES", "ADT_A05,ADT_A39"),
    ("ALLOWED_ASSIGNING_AUTHORITIES", "328"),
];

/// Synthetic sample `SOAP_NS` (`spec/integration-hub-tester-component-preview.md` §5.1).
const SOAP_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";

impl Plugin for Hl7SoapServerPlugin {
    fn id(&self) -> &'static str {
        "hl7-soap-server"
    }
    fn tab_label(&self) -> &'static str {
        "HL7 SOAP Server"
    }
    fn description(&self) -> &'static str {
        "POST SOAP+HL7v2.xml envelope -> SOAP AckResponse (200) / SOAP Fault (400/403/500) - \
         hl7-soap-server, schema group phw, structures ADT_A05/ADT_A39, authority 328"
    }
    fn input_label(&self) -> &'static str {
        "Inbound HL7v2 ER7 (converted to v2.xml and wrapped in a SOAP envelope here), or raw XML"
    }
    fn output_label(&self) -> &'static str {
        "SOAP response envelope (AckResponse or Fault)"
    }
    fn samples(&self) -> &'static [Sample] {
        &[
            ("Valid ADT_A05 (authority 328)", SOAP_VALID),
            ("Malformed SOAP request", SOAP_MALFORMED),
            ("Unknown assigning authority (999)", SOAP_UNKNOWN_AUTHORITY),
        ]
    }
    fn run(&self, input: &str) -> Result<Outcome, String> {
        use hl7_soap_server::config::Config;
        use hl7_soap_server::http::TARGET_NAMESPACE;
        use hl7_soap_server::process::Processor;

        let stripped = input.trim();
        let request = if stripped.starts_with('<') {
            // Already XML: send as given, skipping the conversion.
            stripped.to_string()
        } else {
            let flow = "phw"
                .parse::<ihb_validation::Flow>()
                .map_err(|e| e.to_string())?;
            match ihb_validation::er7_to_xml_for_flow(&to_er7(stripped), flow, None) {
                Ok(xml) => format!(
                    "<SOAP-ENV:Envelope xmlns:SOAP-ENV=\"{SOAP_NS}\"><SOAP-ENV:Body>{xml}\
                     </SOAP-ENV:Body></SOAP-ENV:Envelope>"
                ),
                Err(error) => {
                    return Ok(Outcome::rejected(
                        String::new(),
                        format!("could not convert ER7 to HL7 v2.xml: {error}"),
                    ));
                }
            }
        };

        let config = Config::from_pairs(SOAP_CONFIG).map_err(|e| e.to_string())?;
        let bus = FakeBus::new();
        let processor = Processor::new(
            config,
            bus.sender("tester-demo"),
            Some(bus.sender("tester-store")),
        );

        let (status, response) = match block_on(processor.process(&request))? {
            Ok(accepted) => (
                200,
                hl7_2_soap::response::success_in(&accepted.control_id, TARGET_NAMESPACE),
            ),
            Err(fault) => (fault.status, fault.to_envelope()),
        };

        let output = [
            heading("SOAP REQUEST (as sent to the server)"),
            request,
            String::new(),
            heading(&format!("HTTP {status} - SOAP RESPONSE")),
            response,
        ]
        .join("\n");
        Ok(if status == 200 {
            Outcome::accepted(output, "200 OK")
        } else {
            Outcome::rejected(output, format!("HTTP {status} - see output"))
        })
    }
}
