//! What one mock receiver is.
//!
//! Implements `spec/02-configuration.md` §8.1.
//!
//! The names and defaults are the Python service's, so a container
//! definition moves across unchanged. What has changed is the *reading*:
//! every problem is reported at once (`spec/02-configuration.md` §1.4)
//! rather than one per restart, and blank is absent uniformly (§1.1).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ihb_config::{ConfigErrors, Reader, Secret};

/// The mock's own MLLP port, distinct from every real ingress port
/// (`spec/01-architecture.md` §2.1).
pub const DEFAULT_PORT: u16 = 2576;

/// Everything the receiver needs to start.
///
/// # Examples
///
/// ```
/// use hl7_mock_receiver::config::Config;
///
/// let config = Config::from_pairs([("EGRESS_QUEUE_NAME", "mpi-mock-receiver")])
///     .expect("a queue name is all this needs");
///
/// assert_eq!(config.listen.port(), 2576);
/// assert!(
///     config.connection_string.is_none() && config.namespace.is_none(),
///     "no broker is a supported configuration; see §2.6"
/// );
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the MLLP listener binds.
    pub listen: SocketAddr,
    /// Where the health check binds.
    pub health_check: SocketAddr,
    /// `EGRESS_QUEUE_NAME`. **Required** — a mock with nowhere to forward
    /// to still answers, but it must say which queue it is standing in for.
    pub egress_queue_name: String,
    /// `EGRESS_SESSION_ID`, for a session-enabled queue.
    pub egress_session_id: Option<String>,
    /// `SERVICE_BUS_CONNECTION_STRING`.
    pub connection_string: Option<Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    pub namespace: Option<String>,
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
        let host = reader.string_or("HOST", "127.0.0.1");
        let port = reader.integer_or("PORT", DEFAULT_PORT);
        let egress_queue_name = reader.required_string("EGRESS_QUEUE_NAME");

        let health_host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
        let health_port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");

        let config = Self {
            listen: socket_addr(&host, port),
            health_check: socket_addr(
                &health_host,
                health_port.unwrap_or(ihb_health::DEFAULT_PORT),
            ),
            egress_queue_name,
            egress_session_id: reader.optional_string("EGRESS_SESSION_ID"),
            connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
            namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
            max_message_size: reader
                .optional_positive_integer("MAX_MESSAGE_SIZE_BYTES")
                .unwrap_or(hl7_2_mllp::DEFAULT_LIMIT),
            redaction_enabled: reader.bool_or("HL7_LOG_REDACTION_ENABLED", true),
        };

        reader.finish()?;
        Ok(config)
    }

    /// Whether a broker is configured at all.
    ///
    /// `spec/11-mock-receivers.md` §2.6: neither variable set is a
    /// supported configuration, not an error. It is how the mock runs on a
    /// laptop, and `spec/02-configuration.md` §8's note calls this service
    /// out as the one exception to "a bus client needs a connection".
    #[must_use]
    pub const fn has_broker(&self) -> bool {
        self.connection_string.is_some() || self.namespace.is_some()
    }
}

/// `HOST`/`PORT` as a socket address.
fn socket_addr(host: &str, port: u16) -> SocketAddr {
    let ip = host
        .parse::<IpAddr>()
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    SocketAddr::new(ip, port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_ones_in_section_eight_one() {
        let config = Config::from_pairs([("EGRESS_QUEUE_NAME", "q")]).unwrap();

        assert_eq!(config.listen.port(), DEFAULT_PORT);
        assert_eq!(config.max_message_size, hl7_2_mllp::DEFAULT_LIMIT);
        assert_eq!(config.max_message_size, 16 * 1024 * 1024, "16 MiB");
        assert!(config.egress_session_id.is_none());
    }

    #[test]
    fn the_queue_name_is_required_even_with_no_broker() {
        // spec/11-mock-receivers.md §2.6 — a mock with no broker still says
        // which queue it is standing in for, because that is what its log
        // line reports and what a test reads to know it is wired up right.
        let errors = Config::from_pairs([] as [(&str, &str); 0]).unwrap_err();
        assert!(errors.to_string().contains("EGRESS_QUEUE_NAME"), "{errors}");
    }

    #[test]
    fn no_broker_is_a_configuration_not_a_failure() {
        // spec/11-mock-receivers.md §2.6, and the exception noted at
        // spec/02-configuration.md §8.
        let config = Config::from_pairs([("EGRESS_QUEUE_NAME", "q")]).unwrap();
        assert!(!config.has_broker());

        let with_string = Config::from_pairs([
            ("EGRESS_QUEUE_NAME", "q"),
            ("SERVICE_BUS_CONNECTION_STRING", "Endpoint=sb://x;"),
        ])
        .unwrap();
        assert!(with_string.has_broker());
    }

    #[test]
    fn blank_is_absent_here_too() {
        // spec/02-configuration.md §1.1, applied uniformly — the Python
        // read these variables this way and the rewrite keeps it.
        let config = Config::from_pairs([
            ("EGRESS_QUEUE_NAME", "q"),
            ("EGRESS_SESSION_ID", "   "),
            ("SERVICE_BUS_NAMESPACE", ""),
        ])
        .unwrap();

        assert!(config.egress_session_id.is_none());
        assert!(!config.has_broker());
    }
}
