//! What one transformer deployment is.
//!
//! Implements `spec/02-configuration.md` §5.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ihb_config::{ConfigErrors, Reader, Secret};

/// Messages requested per receive (`spec/02-configuration.md` §5).
pub const DEFAULT_MAX_BATCH_SIZE: usize = 100;

/// One transformer's configuration.
///
/// # Examples
///
/// ```
/// use ihb_transformer::config::Config;
///
/// let config = Config::from_pairs([
///     ("WORKFLOW_ID", "phw-to-mpi"),
///     ("MICROSERVICE_ID", "hl7-phw-transformer"),
///     ("HEALTH_BOARD", "PHW"),
///     ("PEER_SERVICE", "MPI"),
///     ("INGRESS_QUEUE_NAME", "phw-transformer-ingress"),
///     ("EGRESS_QUEUE_NAME", "mpi-sender-ingress"),
/// ])
/// .expect("this configuration is complete");
///
/// assert_eq!(config.max_batch_size, 100);
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// `WORKFLOW_ID`.
    pub workflow_id: String,
    /// `MICROSERVICE_ID`.
    pub microservice_id: String,
    /// `HEALTH_BOARD`.
    pub health_board: String,
    /// `PEER_SERVICE`.
    pub peer_service: String,
    /// `INGRESS_QUEUE_NAME`.
    pub ingress_queue_name: String,
    /// `INGRESS_SESSION_ID`.
    pub ingress_session_id: Option<String>,
    /// `EGRESS_QUEUE_NAME`.
    pub egress_queue_name: String,
    /// `EGRESS_SESSION_ID`.
    pub egress_session_id: Option<String>,
    /// `MAX_BATCH_SIZE`.
    pub max_batch_size: usize,
    /// Where the health check binds.
    pub health_check: SocketAddr,
    /// `SERVICE_BUS_CONNECTION_STRING`.
    pub connection_string: Option<Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    pub namespace: Option<String>,
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
        let ingress_queue_name = reader.required_string("INGRESS_QUEUE_NAME");
        let egress_queue_name = reader.required_string("EGRESS_QUEUE_NAME");

        let health_host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
        let health_port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");

        let config = Self {
            workflow_id,
            microservice_id,
            health_board,
            peer_service,
            ingress_queue_name,
            ingress_session_id: reader.optional_string("INGRESS_SESSION_ID"),
            egress_queue_name,
            egress_session_id: reader.optional_string("EGRESS_SESSION_ID"),
            // §5.1 — an unparseable value fails at startup rather than
            // falling back to 1 as the Python did. A silent batch size of 1
            // is a hundredfold throughput regression that would be blamed
            // on the rewrite. `integer_or` implements §1.6, which is
            // exactly that rule.
            max_batch_size: reader.integer_or("MAX_BATCH_SIZE", DEFAULT_MAX_BATCH_SIZE),
            health_check: SocketAddr::new(
                health_host
                    .parse::<IpAddr>()
                    .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                health_port.unwrap_or(ihb_health::DEFAULT_PORT),
            ),
            connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
            namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
            redaction_enabled: reader.bool_or("HL7_LOG_REDACTION_ENABLED", true),
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
            ("WORKFLOW_ID", "phw-to-mpi"),
            ("MICROSERVICE_ID", "hl7-phw-transformer"),
            ("HEALTH_BOARD", "PHW"),
            ("PEER_SERVICE", "MPI"),
            ("INGRESS_QUEUE_NAME", "in"),
            ("EGRESS_QUEUE_NAME", "out"),
        ]
    }

    fn with(extra: &[(&str, &str)]) -> Result<Config, ConfigErrors> {
        let mut pairs: Vec<(String, String)> = base()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        for (key, value) in extra {
            pairs.push(((*key).to_string(), (*value).to_string()));
        }
        Config::from_pairs(pairs)
    }

    #[test]
    fn both_queues_are_required() {
        // spec/02-configuration.md §5 — a transformer with one end missing
        // is a service that cannot do anything at all.
        for missing in ["INGRESS_QUEUE_NAME", "EGRESS_QUEUE_NAME"] {
            let pairs: Vec<_> = base().into_iter().filter(|(k, _)| *k != missing).collect();
            let errors = Config::from_pairs(pairs).unwrap_err();
            assert!(errors.to_string().contains(missing), "{errors}");
        }
    }

    #[test]
    fn an_unparseable_batch_size_fails_rather_than_defaulting() {
        // spec/02-configuration.md §5.1 — the whole point of the clause.
        // The Python fell back to 1, which is a 100x throughput regression
        // that presents as "the Rust rewrite is slow".
        let errors = with(&[("MAX_BATCH_SIZE", "lots")]).unwrap_err();
        assert!(errors.to_string().contains("MAX_BATCH_SIZE"), "{errors}");

        assert_eq!(with(&[]).unwrap().max_batch_size, DEFAULT_MAX_BATCH_SIZE);
        assert_eq!(
            with(&[("MAX_BATCH_SIZE", "25")]).unwrap().max_batch_size,
            25
        );
    }

    #[test]
    fn sessions_are_optional_at_both_ends() {
        let config = with(&[]).unwrap();
        assert!(config.ingress_session_id.is_none());
        assert!(config.egress_session_id.is_none());

        let sessioned = with(&[
            ("INGRESS_SESSION_ID", "phw-to-mpi"),
            ("EGRESS_SESSION_ID", "mpi"),
        ])
        .unwrap();
        assert_eq!(sessioned.ingress_session_id.as_deref(), Some("phw-to-mpi"));
        assert_eq!(sessioned.egress_session_id.as_deref(), Some("mpi"));
    }
}
