//! What one deployment of the SOAP ingress is.
//!
//! Implements `spec/02-configuration.md` §2 and §4.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use ihb_config::{ConfigErrors, Reader, Secret};
use ihb_validation::Flow;

/// Default HTTP port (`spec/02-configuration.md` §4).
pub const DEFAULT_PORT: u16 = 8080;

/// Default endpoint path.
pub const DEFAULT_PATH: &str = "/soap";

/// Default `ALLOWED_HL7_STRUCTURES`: the root element names (`ADT_A05`,
/// `ADT_A39`) the LIMS flow accepts.
pub const DEFAULT_STRUCTURES: &str = "ADT_A05,ADT_A39";

/// Default `ALLOWED_ASSIGNING_AUTHORITIES`: a single authority code, `328`.
/// A sender asserting any other is refused with a 403 (`spec/08` §4).
pub const DEFAULT_AUTHORITIES: &str = "328";

/// Default request-body limit, in bytes (1 MiB).
pub const DEFAULT_MAX_REQUEST_SIZE: usize = 1_048_576;

/// The ceiling on `MAX_REQUEST_SIZE_BYTES`, in bytes (100 MiB), as `hl7-server`'s
/// (`spec/02-configuration.md` §3.2, referenced by §4).
pub const MAX_REQUEST_SIZE_CEILING: usize = 104_857_600;

/// Where a validated message is forwarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Egress {
    /// `EGRESS_QUEUE_NAME`.
    Queue(String),
    /// `EGRESS_TOPIC_NAME`.
    Topic(String),
}

impl Egress {
    /// The entity name, whichever kind it is.
    #[must_use]
    pub fn entity(&self) -> &str {
        match self {
            Self::Queue(name) | Self::Topic(name) => name,
        }
    }
}

/// A certificate and key pair, both present.
///
/// Modelled as a pair rather than two `Option`s so that "one without the
/// other" cannot be represented after configuration has been read
/// (`spec/02-configuration.md` §4.1). The Python silently ran plaintext in
/// that case; making the invalid state unrepresentable is how this stops
/// being possible rather than merely unlikely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tls {
    /// `TLS_CERT_FILE`. PEM, chain order leaf-first.
    pub certificate: PathBuf,
    /// `TLS_KEY_FILE`. PEM.
    pub key: PathBuf,
}

/// One deployment's configuration.
///
/// # Examples
///
/// ```
/// use hl7_soap_server::config::Config;
///
/// let config = Config::from_pairs([
///     ("WORKFLOW_ID", "lims-soapserver"),
///     ("MICROSERVICE_ID", "hl7-soap-server"),
///     ("HEALTH_BOARD", "WLIMS"),
///     ("PEER_SERVICE", "MPI"),
///     ("EGRESS_QUEUE_NAME", "lims-transformer-ingress"),
///     ("EGRESS_SESSION_ID", "lims-soapserver"),
///     ("MESSAGE_STORE_QUEUE_NAME", "messagestore"),
/// ])
/// .expect("this configuration is complete");
///
/// assert_eq!(config.endpoint_path, "/soap");
/// assert_eq!(config.allowed_structures, ["ADT_A05", "ADT_A39"]);
/// assert_eq!(config.allowed_authorities, ["328"]);
/// assert!(config.tls.is_none(), "plaintext unless both files are given");
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// `WORKFLOW_ID`, the name of the flow; a dimension on every event and
    /// metric.
    pub workflow_id: String,
    /// `MICROSERVICE_ID`.
    pub microservice_id: String,
    /// `HEALTH_BOARD`.
    pub health_board: String,
    /// `PEER_SERVICE`.
    pub peer_service: String,
    /// Where the HTTP listener binds.
    pub listen: SocketAddr,
    /// Where the health check binds.
    pub health_check: SocketAddr,
    /// `SOAP_ENDPOINT_PATH`: the only path served. Any other path is a 404
    /// whatever the method (`spec/08` §1).
    pub endpoint_path: String,
    /// Where a validated message goes.
    pub egress: Egress,
    /// `EGRESS_SESSION_ID`, required: the downstream transformer queue is
    /// session-enabled, so a message without one cannot be sent.
    pub egress_session_id: String,
    /// `MESSAGE_STORE_QUEUE_NAME`. **Required** for this service, unlike
    /// `hl7-server` — the SOAP flow has no other audit trail.
    pub message_store_queue: String,
    /// `MESSAGE_STORE_ENABLED`, default **false** (`spec/02-configuration.md`
    /// §2.1.2). The queue name is still required, as upstream requires it,
    /// but nothing is written unless this is true.
    pub message_store_enabled: bool,
    /// `SERVICE_BUS_CONNECTION_STRING`, a [`Secret`] so it never reaches a
    /// log line.
    pub connection_string: Option<Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    pub namespace: Option<String>,
    /// `HL7_SCHEMA_GROUP`, resolved.
    pub schema_group: Flow,
    /// `ALLOWED_HL7_STRUCTURES`: payload root element names that pass step 4
    /// of `spec/08` §3. Compared exactly, case-sensitively.
    pub allowed_structures: Vec<String>,
    /// `ALLOWED_ASSIGNING_AUTHORITIES`.
    pub allowed_authorities: Vec<String>,
    /// `MAX_REQUEST_SIZE_BYTES`, in bytes, checked against the declared
    /// `Content-Length` before the body is read (`spec/08` §2.1).
    pub max_request_size: usize,
    /// TLS, when both files are configured.
    pub tls: Option<Tls>,
    /// `HL7_LOG_REDACTION_ENABLED`.
    pub redaction_enabled: bool,
}

impl Config {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_env() -> Result<Self, ConfigErrors> {
        Self::read(Reader::from_env())
    }

    /// Read from explicit pairs, for tests and examples.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, ConfigErrors>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self::read(Reader::from_pairs(pairs))
    }

    /// The shared body of the two constructors.
    fn read(mut reader: Reader) -> Result<Self, ConfigErrors> {
        let workflow_id = reader.required_string("WORKFLOW_ID");
        let microservice_id = reader.required_string("MICROSERVICE_ID");
        let health_board = reader.required_string("HEALTH_BOARD");
        let peer_service = reader.required_string("PEER_SERVICE");

        let host = reader.string_or("HOST", "127.0.0.1");
        let port = reader.integer_or("PORT", DEFAULT_PORT);

        let egress = read_egress(&mut reader);
        let egress_session_id = reader.required_string("EGRESS_SESSION_ID");
        let message_store_queue = reader.required_string("MESSAGE_STORE_QUEUE_NAME");
        let message_store_enabled = reader.lenient_bool_or("MESSAGE_STORE_ENABLED", false);

        let config = Self {
            workflow_id,
            microservice_id,
            health_board,
            peer_service,
            listen: socket_addr(&host, port),
            health_check: read_health_check(&mut reader),
            endpoint_path: read_path(&mut reader),
            egress,
            egress_session_id,
            message_store_queue,
            message_store_enabled,
            connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
            namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
            schema_group: read_schema_group(&mut reader),
            allowed_structures: reader.list_or("ALLOWED_HL7_STRUCTURES", DEFAULT_STRUCTURES),
            allowed_authorities: reader
                .list_or("ALLOWED_ASSIGNING_AUTHORITIES", DEFAULT_AUTHORITIES),
            max_request_size: read_max_request_size(&mut reader),
            tls: read_tls(&mut reader),
            redaction_enabled: reader.lenient_bool_or("HL7_LOG_REDACTION_ENABLED", true),
        };

        reader.finish()?;
        Ok(config)
    }
}

/// `HOST`/`PORT` as a socket address.
fn socket_addr(host: &str, port: u16) -> SocketAddr {
    let ip = host
        .parse::<IpAddr>()
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    SocketAddr::new(ip, port)
}

/// `HEALTH_CHECK_HOST`/`HEALTH_CHECK_PORT`.
///
/// Always an address; `spec/05-health-check.md` §2.1.
fn read_health_check(reader: &mut Reader) -> SocketAddr {
    let port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");
    let host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
    socket_addr(&host, port.unwrap_or(ihb_health::DEFAULT_PORT))
}

/// `SOAP_ENDPOINT_PATH`, which must be rooted.
///
/// A path without a leading `/` would never match a request target, so the
/// service would answer 404 to everything while looking correctly
/// configured. Caught at startup instead
/// (`spec/02-configuration.md` §4).
fn read_path(reader: &mut Reader) -> String {
    let path = reader.string_or("SOAP_ENDPOINT_PATH", DEFAULT_PATH);
    if !path.starts_with('/') {
        reader.invalid(
            "SOAP_ENDPOINT_PATH",
            "a path beginning with \"/\"",
            path.clone(),
        );
    }
    path
}

/// `EGRESS_QUEUE_NAME` xor `EGRESS_TOPIC_NAME`
/// (`spec/02-configuration.md` §3.1, referenced by §4).
fn read_egress(reader: &mut Reader) -> Egress {
    let queue = reader.optional_string("EGRESS_QUEUE_NAME");
    let topic = reader.optional_string("EGRESS_TOPIC_NAME");

    match (queue, topic) {
        (Some(queue), None) => Egress::Queue(queue),
        (None, Some(topic)) => Egress::Topic(topic),
        (Some(queue), Some(_)) => {
            reader.conflict("Cannot specify both EGRESS_QUEUE_NAME and EGRESS_TOPIC_NAME.");
            Egress::Queue(queue)
        }
        (None, None) => {
            reader.conflict("Either EGRESS_QUEUE_NAME or EGRESS_TOPIC_NAME must be provided");
            Egress::Queue(String::new())
        }
    }
}

/// `HL7_SCHEMA_GROUP`, defaulting to `phw`.
fn read_schema_group(reader: &mut Reader) -> Flow {
    let name = reader.string_or("HL7_SCHEMA_GROUP", "phw");
    let Ok(flow) = name.parse::<Flow>() else {
        let known: Vec<&str> = Flow::ALL.iter().map(|flow| flow.as_str()).collect();
        reader.invalid(
            "HL7_SCHEMA_GROUP",
            format!("one of {}", known.join(", ")),
            name,
        );
        // A value is still returned so the rest of the configuration is
        // read and its problems reported too; `finish` refuses regardless.
        return Flow::Phw;
    };
    flow
}

/// `MAX_REQUEST_SIZE_BYTES`, with §3.2's asymmetry.
///
/// The same rule as `hl7-server`'s `MAX_MESSAGE_SIZE_BYTES`, because §4
/// says "as §3.2, same 100 MiB ceiling": non-positive or unparseable
/// defaults quietly, oversized is a startup failure.
fn read_max_request_size(reader: &mut Reader) -> usize {
    let Some(raw) = reader
        .raw("MAX_REQUEST_SIZE_BYTES")
        .map(ToString::to_string)
    else {
        return DEFAULT_MAX_REQUEST_SIZE;
    };

    let Ok(size) = raw.parse::<i64>() else {
        return DEFAULT_MAX_REQUEST_SIZE;
    };
    if size <= 0 {
        return DEFAULT_MAX_REQUEST_SIZE;
    }

    let ceiling = i64::try_from(MAX_REQUEST_SIZE_CEILING).unwrap_or(i64::MAX);
    if size > ceiling {
        reader.invalid(
            "MAX_REQUEST_SIZE_BYTES",
            format!("at most {MAX_REQUEST_SIZE_CEILING}, the Service Bus message limit"),
            raw,
        );
        return DEFAULT_MAX_REQUEST_SIZE;
    }

    usize::try_from(size).unwrap_or(DEFAULT_MAX_REQUEST_SIZE)
}

/// `TLS_CERT_FILE` and `TLS_KEY_FILE` — both, or neither.
///
/// `spec/02-configuration.md` §4.1: one without the other is a **startup
/// failure**. The Python silently ran plaintext, which means a deployment
/// with a typo in one variable name serves an NHS interface unencrypted
/// while appearing to be configured for TLS. That is corrected here, and
/// the correction is deliberate rather than incidental.
fn read_tls(reader: &mut Reader) -> Option<Tls> {
    let certificate = reader.optional_string("TLS_CERT_FILE");
    let key = reader.optional_string("TLS_KEY_FILE");

    match (certificate, key) {
        (Some(certificate), Some(key)) => Some(Tls {
            certificate: PathBuf::from(certificate),
            key: PathBuf::from(key),
        }),
        (None, None) => None,
        (Some(_), None) => {
            reader.conflict("TLS_CERT_FILE is set without TLS_KEY_FILE; TLS needs both.");
            None
        }
        (None, Some(_)) => {
            reader.conflict("TLS_KEY_FILE is set without TLS_CERT_FILE; TLS needs both.");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Vec<(&'static str, &'static str)> {
        vec![
            ("WORKFLOW_ID", "lims-soapserver"),
            ("MICROSERVICE_ID", "hl7-soap-server"),
            ("HEALTH_BOARD", "WLIMS"),
            ("PEER_SERVICE", "MPI"),
            ("EGRESS_QUEUE_NAME", "lims-transformer-ingress"),
            ("EGRESS_SESSION_ID", "lims-soapserver"),
            ("MESSAGE_STORE_QUEUE_NAME", "messagestore"),
        ]
    }

    fn with(extra: &[(&str, &str)]) -> Result<Config, ConfigErrors> {
        let mut pairs: Vec<(String, String)> = base()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        for (key, value) in extra {
            pairs.retain(|(name, _)| name != key);
            pairs.push(((*key).to_string(), (*value).to_string()));
        }
        Config::from_pairs(pairs)
    }

    #[test]
    fn the_defaults_are_the_ones_in_section_four() {
        let config = with(&[]).unwrap();

        assert_eq!(config.listen.port(), DEFAULT_PORT);
        assert_eq!(config.endpoint_path, DEFAULT_PATH);
        assert_eq!(config.schema_group, Flow::Phw);
        assert_eq!(config.allowed_structures, ["ADT_A05", "ADT_A39"]);
        assert_eq!(config.allowed_authorities, ["328"]);
        assert_eq!(config.max_request_size, DEFAULT_MAX_REQUEST_SIZE);
    }

    #[test]
    fn the_message_store_queue_is_required_here() {
        // spec/02-configuration.md §4 — unlike `hl7-server`, where it is
        // optional. The SOAP flow has no other audit trail.
        let pairs: Vec<_> = base()
            .into_iter()
            .filter(|(name, _)| *name != "MESSAGE_STORE_QUEUE_NAME")
            .collect();

        let errors = Config::from_pairs(pairs).unwrap_err();
        assert!(
            errors.to_string().contains("MESSAGE_STORE_QUEUE_NAME"),
            "{errors}"
        );
    }

    #[test]
    fn tls_needs_both_files_or_neither() {
        // spec/02-configuration.md §4.1 — the Python ran plaintext when one
        // was missing. That is the defect this test exists to prevent
        // coming back.
        assert!(with(&[]).unwrap().tls.is_none());

        let both = with(&[("TLS_CERT_FILE", "/c.pem"), ("TLS_KEY_FILE", "/k.pem")]).unwrap();
        assert_eq!(
            both.tls,
            Some(Tls {
                certificate: PathBuf::from("/c.pem"),
                key: PathBuf::from("/k.pem"),
            })
        );

        let cert_only = with(&[("TLS_CERT_FILE", "/c.pem")]).unwrap_err();
        assert!(
            cert_only.to_string().contains("TLS needs both"),
            "{cert_only}"
        );

        let key_only = with(&[("TLS_KEY_FILE", "/k.pem")]).unwrap_err();
        assert!(
            key_only.to_string().contains("TLS needs both"),
            "{key_only}"
        );
    }

    #[test]
    fn an_endpoint_path_without_a_leading_slash_is_a_startup_failure() {
        // Otherwise the service answers 404 to everything while looking
        // correctly configured.
        let errors = with(&[("SOAP_ENDPOINT_PATH", "soap")]).unwrap_err();
        assert!(
            errors.to_string().contains("SOAP_ENDPOINT_PATH"),
            "{errors}"
        );

        assert_eq!(
            with(&[("SOAP_ENDPOINT_PATH", "/hl7/soap")])
                .unwrap()
                .endpoint_path,
            "/hl7/soap"
        );
    }

    #[test]
    fn the_allow_lists_are_trimmed_and_blank_falls_back_to_the_default() {
        // spec/02-configuration.md §1.7 and §1.1: a list that resolves to
        // nothing takes the default rather than silently allowing nothing,
        // which would reject every message.
        assert_eq!(
            with(&[("ALLOWED_HL7_STRUCTURES", "ADT_A05 , ADT_A39,")])
                .unwrap()
                .allowed_structures,
            ["ADT_A05", "ADT_A39"]
        );
        assert_eq!(
            with(&[("ALLOWED_ASSIGNING_AUTHORITIES", "   ")])
                .unwrap()
                .allowed_authorities,
            ["328"],
            "blank is absent, so the default applies"
        );
    }

    #[test]
    fn the_request_size_asymmetry_matches_hl7_servers() {
        // spec/02-configuration.md §4, deferring to §3.2.
        assert!(with(&[("MAX_REQUEST_SIZE_BYTES", "104857601")]).is_err());
        for quiet in ["0", "-1", "banana", "  "] {
            assert_eq!(
                with(&[("MAX_REQUEST_SIZE_BYTES", quiet)])
                    .unwrap()
                    .max_request_size,
                DEFAULT_MAX_REQUEST_SIZE,
                "{quiet:?}"
            );
        }
    }

    #[test]
    fn an_unknown_schema_group_is_a_startup_failure() {
        assert!(with(&[("HL7_SCHEMA_GROUP", "cardiology")]).is_err());
        assert_eq!(
            with(&[("HL7_SCHEMA_GROUP", "chemo")]).unwrap().schema_group,
            Flow::Chemo
        );
    }
}
