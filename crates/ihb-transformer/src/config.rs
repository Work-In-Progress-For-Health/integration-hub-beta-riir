//! What one transformer deployment is.
//!
//! Implements `spec/02-configuration.md` §5.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ihb_config::{ConfigErrors, Reader, Secret};

/// Default `MAX_BATCH_SIZE`: how many messages one receive asks the broker
/// for (`spec/02-configuration.md` §5). A count of messages, not bytes.
pub const DEFAULT_MAX_BATCH_SIZE: usize = 100;

/// Where a transformer reads from (`spec/02-configuration.md` §5.2).
///
/// Exactly one shape is configured; both-or-neither is a startup error
/// reported together with every other configuration problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ingress {
    /// `INGRESS_QUEUE_NAME`.
    Queue(String),
    /// `INGRESS_TOPIC_NAME` and `INGRESS_SUBSCRIPTION_NAME`.
    Subscription {
        /// The topic.
        topic: String,
        /// The subscription on it.
        subscription: String,
    },
}

impl Ingress {
    /// The name used in logs and errors: the queue, or `topic/subscription`
    /// (`spec/03-message-bus.md` §2.2).
    #[must_use]
    pub fn entity(&self) -> String {
        match self {
            Self::Queue(queue) => queue.clone(),
            Self::Subscription {
                topic,
                subscription,
            } => format!("{topic}/{subscription}"),
        }
    }
}

/// Where a transformer writes to (`spec/02-configuration.md` §5.2).
///
/// The bus layer opens either by name, so the distinction matters only for
/// logs and for validating that exactly one was set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Egress {
    /// `EGRESS_QUEUE_NAME`.
    Queue(String),
    /// `EGRESS_TOPIC_NAME`.
    Topic(String),
}

impl Egress {
    /// The queue or topic name.
    #[must_use]
    pub fn entity(&self) -> &str {
        match self {
            Self::Queue(name) | Self::Topic(name) => name,
        }
    }
}

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
    /// `INGRESS_QUEUE_NAME`, or `INGRESS_TOPIC_NAME` with
    /// `INGRESS_SUBSCRIPTION_NAME`.
    pub ingress: Ingress,
    /// `INGRESS_SESSION_ID`.
    pub ingress_session_id: Option<String>,
    /// `EGRESS_QUEUE_NAME` or `EGRESS_TOPIC_NAME`.
    pub egress: Egress,
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
        let ingress = read_ingress(&mut reader);
        let egress = read_egress(&mut reader);

        let health_host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
        let health_port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");

        let config = Self {
            workflow_id,
            microservice_id,
            health_board,
            peer_service,
            ingress,
            ingress_session_id: reader.optional_string("INGRESS_SESSION_ID"),
            egress,
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
            redaction_enabled: reader.lenient_bool_or("HL7_LOG_REDACTION_ENABLED", true),
        };

        reader.finish()?;
        Ok(config)
    }
}

/// `INGRESS_QUEUE_NAME` xor a topic with its subscription (§5.2).
///
/// The Python checks this at startup rather than when the configuration is
/// built, so that its test fixtures can build partial ones. Here the
/// configuration is only ever built at startup, so the two are the same.
fn read_ingress(reader: &mut Reader) -> Ingress {
    let queue = reader.optional_string("INGRESS_QUEUE_NAME");
    let topic = reader.optional_string("INGRESS_TOPIC_NAME");
    let subscription = reader.optional_string("INGRESS_SUBSCRIPTION_NAME");

    match (queue, topic, subscription) {
        (Some(queue), None, None) => Ingress::Queue(queue),
        (Some(queue), _, _) => {
            reader.conflict(
                "Invalid ingress configuration: INGRESS_QUEUE_NAME cannot be set together with \
                 INGRESS_TOPIC_NAME/INGRESS_SUBSCRIPTION_NAME. Configure only one ingress transport.",
            );
            Ingress::Queue(queue)
        }
        (None, Some(topic), Some(subscription)) => Ingress::Subscription {
            topic,
            subscription,
        },
        (None, None, None) => {
            reader.conflict(
                "Missing required configuration: set either INGRESS_QUEUE_NAME, or both \
                 INGRESS_TOPIC_NAME and INGRESS_SUBSCRIPTION_NAME.",
            );
            Ingress::Queue(String::new())
        }
        (None, topic, subscription) => {
            reader.conflict(
                "Missing required configuration: INGRESS_TOPIC_NAME and INGRESS_SUBSCRIPTION_NAME \
                 must both be set when using topic-based ingress.",
            );
            Ingress::Subscription {
                topic: topic.unwrap_or_default(),
                subscription: subscription.unwrap_or_default(),
            }
        }
    }
}

/// `EGRESS_QUEUE_NAME` xor `EGRESS_TOPIC_NAME` (`spec/02-configuration.md` §5.2).
///
/// Records a conflict on `reader` for both-set or neither-set instead of
/// returning an error, so that all configuration problems surface at once;
/// the placeholder returned in that case is never used.
fn read_egress(reader: &mut Reader) -> Egress {
    let queue = reader.optional_string("EGRESS_QUEUE_NAME");
    let topic = reader.optional_string("EGRESS_TOPIC_NAME");

    match (queue, topic) {
        (Some(queue), None) => Egress::Queue(queue),
        (None, Some(topic)) => Egress::Topic(topic),
        (Some(queue), Some(_)) => {
            reader.conflict(
                "Invalid egress configuration: EGRESS_QUEUE_NAME cannot be set together with \
                 EGRESS_TOPIC_NAME. Configure only one egress transport.",
            );
            Egress::Queue(queue)
        }
        (None, None) => {
            reader.conflict(
                "Missing required configuration: set either EGRESS_QUEUE_NAME or EGRESS_TOPIC_NAME.",
            );
            Egress::Queue(String::new())
        }
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
    fn both_ends_are_required() {
        // spec/02-configuration.md §5 — a transformer with one end missing
        // is a service that cannot do anything at all.
        for missing in ["INGRESS_QUEUE_NAME", "EGRESS_QUEUE_NAME"] {
            let pairs: Vec<_> = base().into_iter().filter(|(k, _)| *k != missing).collect();
            let errors = Config::from_pairs(pairs).unwrap_err();
            assert!(errors.to_string().contains(missing), "{errors}");
        }
    }

    #[test]
    fn exactly_one_ingress_transport_is_required() {
        // spec/02-configuration.md §5.2.
        let without_queue = |extra: &[(&str, &str)]| {
            let mut pairs: Vec<(String, String)> = base()
                .into_iter()
                .filter(|(k, _)| *k != "INGRESS_QUEUE_NAME")
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            pairs.extend(
                extra
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
            );
            Config::from_pairs(pairs)
        };

        let topic = without_queue(&[
            ("INGRESS_TOPIC_NAME", "mpi"),
            ("INGRESS_SUBSCRIPTION_NAME", "risp"),
        ])
        .unwrap();
        assert_eq!(topic.ingress.entity(), "mpi/risp");

        let neither = without_queue(&[]).unwrap_err().to_string();
        assert!(
            neither.contains("set either INGRESS_QUEUE_NAME"),
            "{neither}"
        );

        let half = without_queue(&[("INGRESS_TOPIC_NAME", "mpi")])
            .unwrap_err()
            .to_string();
        assert!(half.contains("must both be set"), "{half}");

        let both = with(&[
            ("INGRESS_TOPIC_NAME", "mpi"),
            ("INGRESS_SUBSCRIPTION_NAME", "risp"),
        ])
        .unwrap_err()
        .to_string();
        assert!(
            both.contains("Configure only one ingress transport"),
            "{both}"
        );
    }

    #[test]
    fn exactly_one_egress_transport_is_required() {
        // spec/02-configuration.md §5.2.
        let topic_only = |extra: &[(&str, &str)]| {
            let mut pairs: Vec<(String, String)> = base()
                .into_iter()
                .filter(|(k, _)| *k != "EGRESS_QUEUE_NAME")
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            pairs.extend(
                extra
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
            );
            Config::from_pairs(pairs)
        };

        let config = topic_only(&[("EGRESS_TOPIC_NAME", "mpihl7input")]).unwrap();
        assert_eq!(config.egress, Egress::Topic("mpihl7input".into()));

        let neither = topic_only(&[]).unwrap_err().to_string();
        assert!(
            neither.contains("set either EGRESS_QUEUE_NAME"),
            "{neither}"
        );

        let both = with(&[("EGRESS_TOPIC_NAME", "mpihl7input")])
            .unwrap_err()
            .to_string();
        assert!(
            both.contains("Configure only one egress transport"),
            "{both}"
        );
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
