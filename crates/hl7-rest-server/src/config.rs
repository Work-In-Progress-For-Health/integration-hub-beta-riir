//! What one deployment of the REST HL7 ingress is.
//!
//! Implements `spec/02-configuration.md` §11 and §12 (the parts `rest-server`
//! shares), and `spec/19-rest-servers.md` §8.
//!
//! # Split in three, because two binaries read it
//!
//! [`Identity`] (who this is and where it sends), [`Hl7Settings`] (how an HL7
//! message is judged and routed) and the listener settings. The standalone
//! service reads all three in [`Config`]; `rest-server` reads the first two
//! with the same functions and supplies its own listener settings, so the
//! two services cannot drift apart on, say, what `WRRS_QUEUE_NAME` means.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use ihb_config::{ConfigErrors, Reader, Secret};
use ihb_validation::{Flow, SUPPORTED_VERSIONS};

/// Default HTTP port (`spec/02-configuration.md` §11).
pub const DEFAULT_PORT: u16 = 8080;

/// Default size limit, 1 MiB.
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 1_048_576;

/// The Service Bus Premium message ceiling, 100 MiB
/// (`spec/02-configuration.md` §3.2).
pub const SERVICE_BUS_LIMIT: usize = 104_857_600;

/// Where a validated message goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Egress {
    /// A queue.
    Queue(String),
    /// A topic.
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

/// A certificate and key pair, both present (`spec/02-configuration.md` §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tls {
    /// `TLS_CERT_FILE`.
    pub certificate: PathBuf,
    /// `TLS_KEY_FILE`.
    pub key: PathBuf,
}

/// Who this deployment is and where it sends.
#[derive(Debug, Clone)]
pub struct Identity {
    /// `WORKFLOW_ID`.
    pub workflow_id: String,
    /// `MICROSERVICE_ID`.
    pub microservice_id: String,
    /// `HEALTH_BOARD`.
    pub health_board: String,
    /// `PEER_SERVICE`.
    pub peer_service: String,
    /// `EGRESS_QUEUE_NAME` xor `EGRESS_TOPIC_NAME`.
    pub egress: Egress,
    /// `EGRESS_SESSION_ID`.
    pub egress_session_id: String,
    /// `MESSAGE_STORE_QUEUE_NAME`, when `MESSAGE_STORE_ENABLED` is true
    /// (`spec/02-configuration.md` §2.1.2). Optional (`spec/19-rest-servers.md`
    /// §3.5).
    pub message_store_queue: Option<String>,
    /// A queue name ignored because the store is off (§2.1.2c).
    pub message_store_unused_queue: Option<String>,
    /// `SERVICE_BUS_CONNECTION_STRING`.
    pub connection_string: Option<Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    pub namespace: Option<String>,
    /// `HL7_LOG_REDACTION_ENABLED`.
    pub redaction_enabled: bool,
}

impl Identity {
    /// Read the shared identity variables.
    pub fn read(reader: &mut Reader) -> Self {
        let workflow_id = reader.required_string("WORKFLOW_ID");
        let microservice_id = reader.required_string("MICROSERVICE_ID");
        let health_board = reader.required_string("HEALTH_BOARD");
        let peer_service = reader.required_string("PEER_SERVICE");
        let egress = read_egress(reader);
        let egress_session_id = reader.required_string("EGRESS_SESSION_ID");

        let store = ihb_config::message_store(reader);
        Self {
            workflow_id,
            microservice_id,
            health_board,
            peer_service,
            egress,
            egress_session_id,
            message_store_queue: store.queue,
            message_store_unused_queue: store.unused_queue,
            connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
            namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
            redaction_enabled: reader.lenient_bool_or("HL7_LOG_REDACTION_ENABLED", true),
        }
    }
}

/// The second destination of the `risp` flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrrs {
    /// `WRRS_QUEUE_NAME` xor `WRRS_TOPIC_NAME`.
    pub egress: Egress,
    /// `WRRS_EGRESS_SESSION_ID`.
    pub session_id: String,
    /// `WRRS_WORKFLOW_ID`.
    pub workflow_id: String,
}

/// What `HL7_VALIDATION_FLOW` selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowRule {
    /// A schema group (`spec/19-rest-servers.md` §3.3).
    Schema(Flow),
    /// The MPI outbound field rules (§5.2).
    Mpi,
    /// RISP validation and fan-out (§5.3, §6).
    Risp,
}

impl FlowRule {
    /// The configured name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Schema(flow) => flow.as_str(),
            Self::Mpi => "mpi",
            Self::Risp => "risp",
        }
    }
}

/// How an HL7 message is judged and routed.
#[derive(Debug, Clone, Default)]
pub struct Hl7Settings {
    /// `HL7_VERSION`.
    pub hl7_version: Option<String>,
    /// `SENDING_APP`, split.
    pub sending_apps: Vec<String>,
    /// `HL7_VALIDATION_FLOW`.
    pub flow: Option<FlowRule>,
    /// `HL7_VALIDATION_STANDARD`.
    pub standard: Option<String>,
    /// The `WRRS_*` variables; `Some` only for the `risp` flow.
    pub wrrs: Option<Wrrs>,
}

impl Hl7Settings {
    /// Read the HL7 variables.
    pub fn read(reader: &mut Reader) -> Self {
        let flow = read_flow(reader);
        let wrrs = if flow == Some(FlowRule::Risp) {
            read_wrrs(reader)
        } else {
            // `spec/02-configuration.md` §11.1 — read and ignored.
            None
        };

        Self {
            hl7_version: reader.optional_string("HL7_VERSION"),
            sending_apps: reader.list("SENDING_APP"),
            flow,
            standard: reader.optional_one_of("HL7_VALIDATION_STANDARD", &SUPPORTED_VERSIONS),
            wrrs,
        }
    }

    /// The name used in event text, or `None`.
    #[must_use]
    pub fn flow_name(&self) -> Option<&'static str> {
        self.flow.map(FlowRule::as_str)
    }
}

/// `HL7_VALIDATION_FLOW`.
fn read_flow(reader: &mut Reader) -> Option<FlowRule> {
    let name = reader.optional_string("HL7_VALIDATION_FLOW")?;
    match name.as_str() {
        "mpi" => Some(FlowRule::Mpi),
        "risp" => Some(FlowRule::Risp),
        other => other.parse::<Flow>().map_or_else(
            |_| {
                let known: Vec<&str> = Flow::ALL.iter().map(|flow| flow.as_str()).collect();
                reader.invalid(
                    "HL7_VALIDATION_FLOW",
                    format!("one of {}, mpi or risp", known.join(", ")),
                    name.clone(),
                );
                None
            },
            |flow| Some(FlowRule::Schema(flow)),
        ),
    }
}

/// The four `WRRS_*` rules of `spec/02-configuration.md` §11.1.
fn read_wrrs(reader: &mut Reader) -> Option<Wrrs> {
    let queue = reader.optional_string("WRRS_QUEUE_NAME");
    let topic = reader.optional_string("WRRS_TOPIC_NAME");
    let session = reader.optional_string("WRRS_EGRESS_SESSION_ID");
    let workflow = reader.optional_string("WRRS_WORKFLOW_ID");

    let egress = match (queue, topic) {
        (Some(queue), None) => Some(Egress::Queue(queue)),
        (None, Some(topic)) => Some(Egress::Topic(topic)),
        (Some(_), Some(_)) => {
            reader.conflict("Cannot specify both WRRS_QUEUE_NAME and WRRS_TOPIC_NAME.");
            None
        }
        (None, None) => {
            reader.conflict(
                "The 'risp' flow requires either WRRS_QUEUE_NAME or WRRS_TOPIC_NAME to be provided.",
            );
            None
        }
    };
    if session.is_none() {
        reader.conflict("The 'risp' flow requires WRRS_EGRESS_SESSION_ID to be provided.");
    }
    if workflow.is_none() {
        reader.conflict("The 'risp' flow requires WRRS_WORKFLOW_ID to be provided.");
    }

    Some(Wrrs {
        egress: egress?,
        session_id: session?,
        workflow_id: workflow?,
    })
}

/// `EGRESS_QUEUE_NAME` xor `EGRESS_TOPIC_NAME` (`spec/02-configuration.md`
/// §3.1).
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

/// `HOST`/`PORT` as a socket address.
#[must_use]
pub fn socket_addr(host: &str, port: u16) -> SocketAddr {
    let ip = host
        .parse::<IpAddr>()
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    SocketAddr::new(ip, port)
}

/// `TLS_CERT_FILE` and `TLS_KEY_FILE` — both, or neither
/// (`spec/02-configuration.md` §4.1).
pub fn read_tls(reader: &mut Reader) -> Option<Tls> {
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

/// A size limit with `spec/02-configuration.md` §3.2's asymmetry: absent,
/// unparseable or non-positive defaults, above the ceiling fails.
pub fn read_size_limit(reader: &mut Reader, name: &str) -> usize {
    let Some(raw) = reader.raw(name).map(ToString::to_string) else {
        return DEFAULT_MAX_MESSAGE_SIZE;
    };
    let Ok(size) = raw.parse::<i64>() else {
        return DEFAULT_MAX_MESSAGE_SIZE;
    };
    if size <= 0 {
        return DEFAULT_MAX_MESSAGE_SIZE;
    }

    let ceiling = i64::try_from(SERVICE_BUS_LIMIT).unwrap_or(i64::MAX);
    if size > ceiling {
        reader.invalid(
            name,
            format!("at most {SERVICE_BUS_LIMIT}, the Service Bus message limit"),
            raw,
        );
        return DEFAULT_MAX_MESSAGE_SIZE;
    }
    usize::try_from(size).unwrap_or(DEFAULT_MAX_MESSAGE_SIZE)
}

/// The standalone service's configuration.
///
/// # Examples
///
/// ```
/// use hl7_rest_server::config::Config;
///
/// let config = Config::from_pairs([
///     ("WORKFLOW_ID", "risp-to-mpi"),
///     ("MICROSERVICE_ID", "hl7-rest-server"),
///     ("HEALTH_BOARD", "RISP"),
///     ("PEER_SERVICE", "MPI"),
///     ("EGRESS_QUEUE_NAME", "risp-transformer-ingress"),
///     ("EGRESS_SESSION_ID", "risp-to-mpi"),
/// ])
/// .expect("this configuration is complete");
///
/// assert_eq!(config.listen.to_string(), "0.0.0.0:8080");
/// assert_eq!(config.environment, "DEV");
/// assert!(config.identity.message_store_queue.is_none(), "optional");
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// Who this is.
    pub identity: Identity,
    /// How messages are judged.
    pub settings: Hl7Settings,
    /// Where the HTTP listener binds.
    pub listen: SocketAddr,
    /// `ENVIRONMENT`, upper-cased.
    pub environment: String,
    /// `MAX_MESSAGE_SIZE_BYTES`.
    pub max_message_size: usize,
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
        let identity = Identity::read(&mut reader);
        let settings = Hl7Settings::read(&mut reader);

        let host = reader.string_or("HOST", "0.0.0.0");
        let port = reader.integer_or("PORT", DEFAULT_PORT);

        let config = Self {
            identity,
            settings,
            listen: socket_addr(&host, port),
            environment: reader.string_or("ENVIRONMENT", "DEV").to_uppercase(),
            max_message_size: read_size_limit(&mut reader, "MAX_MESSAGE_SIZE_BYTES"),
        };

        reader.finish()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Vec<(&'static str, &'static str)> {
        vec![
            ("WORKFLOW_ID", "w"),
            ("MICROSERVICE_ID", "m"),
            ("HEALTH_BOARD", "h"),
            ("PEER_SERVICE", "p"),
            ("EGRESS_QUEUE_NAME", "q"),
            ("EGRESS_SESSION_ID", "s"),
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

    const WRRS: [(&str, &str); 3] = [
        ("WRRS_QUEUE_NAME", "wrrs-queue"),
        ("WRRS_EGRESS_SESSION_ID", "risp-to-wrrs"),
        ("WRRS_WORKFLOW_ID", "risp-to-wrrs"),
    ];

    #[test]
    fn the_defaults_are_the_ones_in_section_eleven() {
        // spec/02-configuration.md §11 — and the standalone HOST default,
        // which differs from rest-server's.
        let config = with(&[]).unwrap();
        assert_eq!(config.listen.to_string(), "0.0.0.0:8080");
        assert_eq!(config.max_message_size, 1_048_576);
        assert_eq!(config.environment, "DEV");
        assert!(config.settings.flow.is_none());
        assert!(config.identity.message_store_queue.is_none());
    }

    #[test]
    fn the_environment_is_upper_cased() {
        assert_eq!(with(&[("ENVIRONMENT", "sit")]).unwrap().environment, "SIT");
    }

    #[test]
    fn exactly_one_egress_is_required() {
        // test_app_config.py::test_cannot_configure_both_queue_and_topic,
        // spec/02-configuration.md §3.1.
        let both = with(&[("EGRESS_TOPIC_NAME", "t")]).unwrap_err().to_string();
        assert!(
            both.contains("Cannot specify both EGRESS_QUEUE_NAME"),
            "{both}"
        );

        let pairs: Vec<(&str, &str)> = base()
            .into_iter()
            .filter(|(k, _)| *k != "EGRESS_QUEUE_NAME")
            .collect();
        let none = Config::from_pairs(pairs).unwrap_err().to_string();
        assert!(
            none.contains("Either EGRESS_QUEUE_NAME or EGRESS_TOPIC_NAME"),
            "{none}"
        );
    }

    #[test]
    fn a_message_size_over_the_service_bus_limit_is_a_startup_failure() {
        // spec/02-configuration.md §11, §3.2.
        let error = with(&[("MAX_MESSAGE_SIZE_BYTES", "104857601")]).unwrap_err();
        assert!(
            error.to_string().contains("MAX_MESSAGE_SIZE_BYTES"),
            "{error}"
        );
        // ... while zero and negative quietly default.
        assert_eq!(
            with(&[("MAX_MESSAGE_SIZE_BYTES", "-5")])
                .unwrap()
                .max_message_size,
            1_048_576
        );
    }

    #[test]
    fn the_risp_flow_requires_its_wrrs_destination() {
        // spec/02-configuration.md §11.1; test_app_config.py::test_risp_flow_requires_wrrs_config.
        let error = with(&[("HL7_VALIDATION_FLOW", "risp")])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("WRRS_QUEUE_NAME or WRRS_TOPIC_NAME"),
            "{error}"
        );
        assert!(error.contains("WRRS_EGRESS_SESSION_ID"), "{error}");
        assert!(error.contains("WRRS_WORKFLOW_ID"), "all together: {error}");
    }

    #[test]
    fn the_risp_flow_with_a_wrrs_destination_is_accepted() {
        let mut extra = vec![("HL7_VALIDATION_FLOW", "risp")];
        extra.extend(WRRS);
        let config = with(&extra).unwrap();
        let wrrs = config.settings.wrrs.unwrap();
        assert_eq!(wrrs.egress, Egress::Queue("wrrs-queue".into()));
        assert_eq!(wrrs.workflow_id, "risp-to-wrrs");
    }

    #[test]
    fn wrrs_queue_and_topic_together_are_refused() {
        let mut extra = vec![("HL7_VALIDATION_FLOW", "risp"), ("WRRS_TOPIC_NAME", "t")];
        extra.extend(WRRS);
        let error = with(&extra).unwrap_err().to_string();
        assert!(
            error.contains("Cannot specify both WRRS_QUEUE_NAME"),
            "{error}"
        );
    }

    #[test]
    fn wrrs_is_ignored_for_other_flows() {
        let mut extra = vec![("HL7_VALIDATION_FLOW", "mpi")];
        extra.extend(WRRS);
        assert!(with(&extra).unwrap().settings.wrrs.is_none());
    }

    #[test]
    fn an_unknown_flow_is_a_startup_failure() {
        let error = with(&[("HL7_VALIDATION_FLOW", "nope")])
            .unwrap_err()
            .to_string();
        assert!(error.contains("HL7_VALIDATION_FLOW"), "{error}");
    }

    #[test]
    fn the_flows_resolve() {
        assert_eq!(
            with(&[("HL7_VALIDATION_FLOW", "phw")])
                .unwrap()
                .settings
                .flow,
            Some(FlowRule::Schema(Flow::Phw))
        );
        assert_eq!(
            with(&[("HL7_VALIDATION_FLOW", "mpi")])
                .unwrap()
                .settings
                .flow,
            Some(FlowRule::Mpi)
        );
    }

    #[test]
    fn sending_apps_split_and_the_standard_is_checked() {
        let config = with(&[
            ("SENDING_APP", "252, 109"),
            ("HL7_VALIDATION_STANDARD", "2.5"),
        ])
        .unwrap();
        assert_eq!(config.settings.sending_apps, ["252", "109"]);
        assert_eq!(config.settings.standard.as_deref(), Some("2.5"));

        assert!(with(&[("HL7_VALIDATION_STANDARD", "9.9")]).is_err());
    }

    #[test]
    fn a_connection_string_does_not_print() {
        // spec/02-configuration.md §10.4.
        let config = with(&[(
            "SERVICE_BUS_CONNECTION_STRING",
            "Endpoint=sb://x;SharedAccessKey=hunter2",
        )])
        .unwrap();
        assert!(!format!("{config:?}").contains("hunter2"));
    }
}
