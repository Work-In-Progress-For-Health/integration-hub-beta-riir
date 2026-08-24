//! What one deployment of this server is.
//!
//! Implements `spec/02-configuration.md` §2 and §3.
//!
//! The server is generic: PHW, Paris, ChemoCare, PIMS, WDS, Mosaiq and the
//! MPI outbound flow are the same binary with different values here
//! (`spec/07-hl7-server.md` §1). So this module is where the differences
//! between flows actually live, and reading it is the fastest way to see
//! what a flow *is*.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ihb_config::{ConfigErrors, Reader, Secret};
use ihb_validation::Flow;

/// The largest frame accepted when `MAX_MESSAGE_SIZE_BYTES` says nothing
/// usable (`spec/02-configuration.md` §3.2).
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 1_048_576;

/// The largest value `MAX_MESSAGE_SIZE_BYTES` may be set to.
///
/// 100 MiB, the Azure Service Bus Premium message limit. Above this a
/// message could be accepted at ingress and then be unsendable, which is
/// the one shape `spec/07-hl7-server.md` §5.3 exists to prevent.
pub const MAX_MESSAGE_SIZE_CEILING: usize = 104_857_600;

/// The default MLLP port, which is PHW's.
pub const DEFAULT_PORT: u16 = 2575;

/// HL7 releases `HL7_VALIDATION_STANDARD` may name
/// (`spec/02-configuration.md` §3.3).
pub const SUPPORTED_STANDARDS: &[&str] = &["2.4", "2.5", "2.5.1", "2.6"];

/// Where a validated message is forwarded.
///
/// Exactly one of the two, enforced at startup — the Python allowed the
/// ambiguity to reach the send path, where "both set" silently preferred
/// the queue (`spec/02-configuration.md` §3.1).
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

/// One deployment's configuration.
///
/// # Examples
///
/// The PHW flow, as `local/hl7_server_phw.env` sets it up:
///
/// ```
/// use hl7_server::config::{Config, Egress};
///
/// let config = Config::from_pairs([
///     ("WORKFLOW_ID", "phw-to-mpi"),
///     ("MICROSERVICE_ID", "hl7-server-phw"),
///     ("HEALTH_BOARD", "PHW"),
///     ("PEER_SERVICE", "MPI"),
///     ("PORT", "2575"),
///     ("EGRESS_QUEUE_NAME", "local-inthub-phw-transformer-ingress"),
///     ("EGRESS_SESSION_ID", "phw-to-mpi"),
///     ("HL7_VERSION", "2.5"),
///     ("SENDING_APP", "252"),
///     ("HL7_VALIDATION_FLOW", "phw"),
/// ])
/// .expect("this configuration is complete");
///
/// assert_eq!(config.egress, Egress::Queue("local-inthub-phw-transformer-ingress".into()));
/// assert_eq!(config.hl7_version.as_deref(), Some("2.5"));
/// assert_eq!(config.sending_apps, ["252"]);
/// ```
///
/// Every problem is reported at once, not one per restart
/// (`spec/02-configuration.md` §1.4):
///
/// ```
/// use hl7_server::config::Config;
///
/// let errors = Config::from_pairs([("PORT", "2575")]).unwrap_err();
/// let report = errors.to_string();
///
/// assert!(report.contains("WORKFLOW_ID"));
/// assert!(report.contains("MICROSERVICE_ID"));
/// assert!(report.contains("EGRESS_SESSION_ID"));
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// `WORKFLOW_ID`. A telemetry dimension and the `WorkflowID` property.
    pub workflow_id: String,
    /// `MICROSERVICE_ID`. This component's name in events and traces.
    pub microservice_id: String,
    /// `HEALTH_BOARD`. A metric dimension.
    pub health_board: String,
    /// `PEER_SERVICE`. A metric dimension, and the store event's default
    /// `TargetSystem`.
    pub peer_service: String,
    /// Where the MLLP listener binds.
    pub listen: SocketAddr,
    /// Where the health check binds.
    ///
    /// Not optional: `spec/05-health-check.md` §2.1 says an absent
    /// `HEALTH_CHECK_PORT` resolves to the default rather than disabling
    /// the listener, and `spec/01-architecture.md` §1.3 requires every
    /// process to expose one.
    pub health_check: SocketAddr,
    /// The queue or topic a validated message goes to.
    pub egress: Egress,
    /// `EGRESS_SESSION_ID`. Required: every pipeline queue needs a session
    /// (`spec/01-architecture.md` §2.3.1).
    pub egress_session_id: String,
    /// `MESSAGE_STORE_QUEUE_NAME`. Absent means no store sender, and the
    /// non-blocking step 10 of `spec/07-hl7-server.md` §5 does nothing.
    pub message_store_queue: Option<String>,
    /// `SERVICE_BUS_CONNECTION_STRING`.
    pub connection_string: Option<Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    pub namespace: Option<String>,
    /// `HL7_VERSION`. When set, `MSH-12` must equal it exactly.
    pub hl7_version: Option<String>,
    /// `SENDING_APP`, split on commas. Empty disables the check.
    pub sending_apps: Vec<String>,
    /// `HL7_VALIDATION_FLOW`, resolved. `None` disables flow validation.
    pub validation_flow: Option<Flow>,
    /// The raw `HL7_VALIDATION_FLOW`, kept because the `mpi` value selects
    /// behaviour but is not a schema group, so it does not resolve to a
    /// [`Flow`] (`spec/01-architecture.md` §2.2.1).
    pub validation_flow_name: Option<String>,
    /// `HL7_VALIDATION_STANDARD`.
    pub validation_standard: Option<String>,
    /// `MAX_MESSAGE_SIZE_BYTES`.
    pub max_message_size: usize,
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

        let validation_flow_name = reader.optional_string("HL7_VALIDATION_FLOW");
        let validation_flow = read_flow(&mut reader, validation_flow_name.as_deref());

        let config = Self {
            workflow_id,
            microservice_id,
            health_board,
            peer_service,
            listen: socket_addr(&host, port),
            health_check: read_health_check(&mut reader),
            egress,
            egress_session_id,
            message_store_queue: reader.optional_string("MESSAGE_STORE_QUEUE_NAME"),
            connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
            namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
            hl7_version: reader.optional_string("HL7_VERSION"),
            sending_apps: reader.list("SENDING_APP"),
            validation_flow,
            validation_flow_name,
            validation_standard: reader
                .optional_one_of("HL7_VALIDATION_STANDARD", SUPPORTED_STANDARDS),
            max_message_size: read_max_message_size(&mut reader),
            redaction_enabled: reader.lenient_bool_or("HL7_LOG_REDACTION_ENABLED", true),
        };

        reader.finish()?;
        Ok(config)
    }

    /// Whether this deployment is the MPI outbound flow.
    ///
    /// `mpi` is not a schema group. It selects the custom field rules of
    /// `spec/07-hl7-server.md` §3.3.2 and the routing properties of §6.2,
    /// and it **suppresses** flow-schema validation — the Python guards on
    /// `self.flow_name != "mpi"` and this is that guard.
    #[must_use]
    pub fn is_mpi_flow(&self) -> bool {
        self.validation_flow_name.as_deref() == Some("mpi")
    }
}

/// `HOST`/`PORT` as a socket address.
///
/// A name that is not an address binds the loopback rather than failing:
/// the Python passed the string to `socket.bind`, and a container that
/// cannot bind is a restart loop rather than a diagnosable error. The
/// address is logged at startup so the resolved value is visible.
fn socket_addr(host: &str, port: u16) -> SocketAddr {
    let ip = host
        .parse::<IpAddr>()
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    SocketAddr::new(ip, port)
}

/// `HEALTH_CHECK_HOST`/`HEALTH_CHECK_PORT`.
///
/// Always yields an address. `spec/05-health-check.md` §2.1 is explicit
/// that an absent value resolves to the default and does **not** disable
/// the listener: the Python passes `None` and its constructor substitutes
/// the default. A service with no health check would never be marked ready
/// by its orchestrator, so "absent means off" would be a silent outage
/// rather than a lighter configuration.
fn read_health_check(reader: &mut Reader) -> SocketAddr {
    let port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");
    let host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
    socket_addr(&host, port.unwrap_or(ihb_health::DEFAULT_PORT))
}

/// `EGRESS_QUEUE_NAME` xor `EGRESS_TOPIC_NAME` (§3.1).
///
/// Both messages are the Python's, verbatim, because an operator who has
/// seen one before should recognise it. Note that only one of them ends in
/// a full stop; that asymmetry is in the Python and is preserved.
fn read_egress(reader: &mut Reader) -> Egress {
    let queue = reader.optional_string("EGRESS_QUEUE_NAME");
    let topic = reader.optional_string("EGRESS_TOPIC_NAME");

    match (queue, topic) {
        (Some(queue), None) => Egress::Queue(queue),
        (None, Some(topic)) => Egress::Topic(topic),
        (Some(queue), Some(_)) => {
            reader.conflict("Cannot specify both EGRESS_QUEUE_NAME and EGRESS_TOPIC_NAME.");
            // A value is still returned so the rest of the configuration is
            // read and its problems reported too (§1.4). `finish` refuses.
            Egress::Queue(queue)
        }
        (None, None) => {
            reader.conflict("Either EGRESS_QUEUE_NAME or EGRESS_TOPIC_NAME must be provided");
            Egress::Queue(String::new())
        }
    }
}

/// Resolve `HL7_VALIDATION_FLOW` to a schema group.
///
/// `mpi` is accepted and resolves to `None`: it is a real flow name that
/// selects real behaviour, but it has no schema group, so it must not be
/// reported as an unknown value.
fn read_flow(reader: &mut Reader, name: Option<&str>) -> Option<Flow> {
    let name = name?;
    if name == "mpi" {
        return None;
    }

    let Ok(flow) = name.parse::<Flow>() else {
        let known: Vec<&str> = Flow::ALL.iter().map(|flow| flow.as_str()).collect();
        reader.invalid(
            "HL7_VALIDATION_FLOW",
            format!("one of {}, or mpi", known.join(", ")),
            name,
        );
        return None;
    };
    Some(flow)
}

/// `MAX_MESSAGE_SIZE_BYTES`, with the asymmetry of §3.2.
///
/// Absent, unparseable, zero or negative resolves to the default; a value
/// above [`MAX_MESSAGE_SIZE_CEILING`] is a startup failure. That is not
/// symmetrical and it is not an oversight: the Python's
/// `_read_and_validate_message_size` defaults quietly on the low side and
/// raises on the high side, and both halves have operational meaning. A
/// too-small limit rejects messages one at a time, visibly, in the logs; a
/// too-large one is only discovered when Service Bus refuses a message the
/// server has already acknowledged — the one failure
/// `spec/07-hl7-server.md` §5.3 exists to prevent.
///
/// Note this is a **deliberate exception** to
/// `spec/02-configuration.md` §1.6, which otherwise forbids silently
/// defaulting an unparseable integer. §3.2 names this variable and says so.
/// It is read through [`Reader::raw`] rather than through the integer
/// getters for exactly that reason — the getters implement §1.6 correctly,
/// and this row does not want §1.6.
fn read_max_message_size(reader: &mut Reader) -> usize {
    let Some(raw) = reader.raw("MAX_MESSAGE_SIZE_BYTES") else {
        return DEFAULT_MAX_MESSAGE_SIZE;
    };

    // `i64` rather than `usize` so that "-1" parses and is *recognised* as
    // non-positive, instead of failing to parse and reaching the default by
    // the other route. The two are the same outcome here, but only one of
    // them is the outcome the specification describes.
    let Ok(size) = raw.parse::<i64>() else {
        return DEFAULT_MAX_MESSAGE_SIZE;
    };

    if size <= 0 {
        return DEFAULT_MAX_MESSAGE_SIZE;
    }

    let ceiling = i64::try_from(MAX_MESSAGE_SIZE_CEILING).unwrap_or(i64::MAX);
    if size > ceiling {
        reader.invalid(
            "MAX_MESSAGE_SIZE_BYTES",
            format!("at most {MAX_MESSAGE_SIZE_CEILING}, the Service Bus message limit"),
            raw.to_string(),
        );
        return DEFAULT_MAX_MESSAGE_SIZE;
    }

    usize::try_from(size).unwrap_or(DEFAULT_MAX_MESSAGE_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration with everything required, for tests that vary one
    /// thing. Any test that cares about a value sets it explicitly.
    fn base() -> Vec<(&'static str, &'static str)> {
        vec![
            ("WORKFLOW_ID", "phw-to-mpi"),
            ("MICROSERVICE_ID", "hl7-server-phw"),
            ("HEALTH_BOARD", "PHW"),
            ("PEER_SERVICE", "MPI"),
            ("EGRESS_QUEUE_NAME", "pre-phw-transform"),
            ("EGRESS_SESSION_ID", "phw-to-mpi"),
        ]
    }

    fn with(extra: &[(&str, &str)]) -> Result<Config, ConfigErrors> {
        let mut pairs: Vec<(String, String)> = base()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        for (k, v) in extra {
            pairs.retain(|(name, _)| name != k);
            pairs.push(((*k).to_string(), (*v).to_string()));
        }
        Config::from_pairs(pairs)
    }

    #[test]
    fn the_port_defaults_to_phws() {
        // spec/02-configuration.md §3.
        assert_eq!(with(&[]).unwrap().listen.port(), DEFAULT_PORT);
    }

    #[test]
    fn a_queue_and_a_topic_together_is_a_startup_failure() {
        // spec/02-configuration.md §3.1 — the message is the Python's.
        let errors = with(&[("EGRESS_TOPIC_NAME", "mpihl7input")]).unwrap_err();
        assert!(
            errors
                .to_string()
                .contains("Cannot specify both EGRESS_QUEUE_NAME and EGRESS_TOPIC_NAME."),
            "{errors}"
        );
    }

    #[test]
    fn neither_a_queue_nor_a_topic_is_a_startup_failure() {
        // spec/02-configuration.md §3.1. Note: no full stop, unlike the
        // message above. That asymmetry is the Python's.
        let pairs: Vec<_> = base()
            .into_iter()
            .filter(|(name, _)| *name != "EGRESS_QUEUE_NAME")
            .collect();
        let errors = Config::from_pairs(pairs).unwrap_err();
        assert!(
            errors
                .to_string()
                .contains("Either EGRESS_QUEUE_NAME or EGRESS_TOPIC_NAME must be provided"),
            "{errors}"
        );
    }

    #[test]
    fn a_topic_is_an_egress_too() {
        let pairs: Vec<_> = base()
            .into_iter()
            .filter(|(name, _)| *name != "EGRESS_QUEUE_NAME")
            .chain([("EGRESS_TOPIC_NAME", "mpihl7input")])
            .collect();
        let config = Config::from_pairs(pairs).unwrap();
        assert_eq!(config.egress, Egress::Topic("mpihl7input".into()));
        assert_eq!(config.egress.entity(), "mpihl7input");
    }

    #[test]
    fn an_oversized_message_limit_fails_but_a_zero_one_defaults() {
        // spec/02-configuration.md §3.2 — the asymmetry, in one test so it
        // cannot be half-changed.
        let too_big = with(&[("MAX_MESSAGE_SIZE_BYTES", "104857601")]).unwrap_err();
        assert!(too_big.to_string().contains("MAX_MESSAGE_SIZE_BYTES"));

        for quiet in ["0", "-1", "banana", "", "   "] {
            let config = with(&[("MAX_MESSAGE_SIZE_BYTES", quiet)])
                .unwrap_or_else(|error| panic!("{quiet:?} should default quietly: {error}"));
            assert_eq!(
                config.max_message_size, DEFAULT_MAX_MESSAGE_SIZE,
                "{quiet:?}"
            );
        }
    }

    #[test]
    fn the_message_limit_ceiling_is_inclusive() {
        let config = with(&[("MAX_MESSAGE_SIZE_BYTES", "104857600")]).unwrap();
        assert_eq!(config.max_message_size, MAX_MESSAGE_SIZE_CEILING);
    }

    #[test]
    fn mpi_is_a_flow_name_but_not_a_schema_group() {
        // spec/01-architecture.md §2.2.1 — `mpi` selects behaviour and
        // suppresses schema validation, so resolving it to `None` must not
        // be confused with "no flow configured".
        let config = with(&[("HL7_VALIDATION_FLOW", "mpi")]).unwrap();
        assert!(config.is_mpi_flow());
        assert!(config.validation_flow.is_none());
        assert_eq!(config.validation_flow_name.as_deref(), Some("mpi"));
    }

    #[test]
    fn an_unknown_flow_is_a_startup_failure_that_lists_the_known_ones() {
        let errors = with(&[("HL7_VALIDATION_FLOW", "cardiology")]).unwrap_err();
        let report = errors.to_string();
        assert!(report.contains("HL7_VALIDATION_FLOW"), "{report}");
        assert!(
            report.contains("phw"),
            "the known flows should be listed: {report}"
        );
        assert!(report.contains("mpi"), "including mpi: {report}");
    }

    #[test]
    fn an_unsupported_standard_is_a_startup_failure() {
        // spec/02-configuration.md §3.3.
        assert!(with(&[("HL7_VALIDATION_STANDARD", "2.7")]).is_err());
        for good in SUPPORTED_STANDARDS {
            assert!(with(&[("HL7_VALIDATION_STANDARD", good)]).is_ok(), "{good}");
        }
    }

    #[test]
    fn a_blank_sending_app_disables_the_check() {
        // spec/07-hl7-server.md §3.2.1, via spec/02 §1.1.
        assert!(
            with(&[("SENDING_APP", "  ")])
                .unwrap()
                .sending_apps
                .is_empty()
        );
        assert_eq!(
            with(&[("SENDING_APP", "245, 212 ,192")])
                .unwrap()
                .sending_apps,
            ["245", "212", "192"],
            "elements are trimmed"
        );
    }

    #[test]
    fn redaction_is_on_unless_explicitly_turned_off() {
        // spec/04-observability.md §2. The default matters more than the
        // parsing does: a missing variable must never mean "log payloads".
        assert!(with(&[]).unwrap().redaction_enabled);
        assert!(
            !with(&[("HL7_LOG_REDACTION_ENABLED", "false")])
                .unwrap()
                .redaction_enabled
        );
        assert!(
            with(&[("HL7_LOG_REDACTION_ENABLED", "yes")])
                .unwrap()
                .redaction_enabled
        );
    }
}
