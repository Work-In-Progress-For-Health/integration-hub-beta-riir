//! What one sender deployment is.
//!
//! Implements `spec/02-configuration.md` §6.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use ihb_config::{ConfigErrors, Reader, Secret};

/// Messages requested per receive before §2.3's reduction.
pub const DEFAULT_MAX_BATCH_SIZE: usize = 100;

/// How long to wait for an acknowledgement.
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a sender reads from.
///
/// `spec/10-senders.md` §5.1 — the queue and subscription variants differ
/// in this and nothing else, which is why it is one type rather than two
/// services' worth of duplicated pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ingress {
    /// A queue, with a required session (`spec/02-configuration.md` §6.1).
    Queue {
        /// `INGRESS_QUEUE_NAME`.
        name: String,
        /// `INGRESS_SESSION_ID`.
        session_id: String,
    },
    /// A topic subscription, with an optional session (§6.2).
    Subscription {
        /// `INGRESS_TOPIC_NAME`.
        topic: String,
        /// `INGRESS_SUBSCRIPTION_NAME`.
        subscription: String,
        /// `INGRESS_SESSION_ID`, optional here.
        session_id: Option<String>,
    },
}

impl Ingress {
    /// What this ingress is called in a log line or a metric.
    ///
    /// `spec/10-senders.md` §5.3, via `spec/03-message-bus.md` §2.2: a
    /// subscription names itself `topic/subscription`.
    #[must_use]
    pub fn entity(&self) -> String {
        match self {
            Self::Queue { name, .. } => name.clone(),
            Self::Subscription {
                topic,
                subscription,
                ..
            } => format!("{topic}/{subscription}"),
        }
    }

    /// The session to open with, if any.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Queue { session_id, .. } => Some(session_id),
            Self::Subscription { session_id, .. } => session_id.as_deref(),
        }
    }
}

/// One sender's configuration.
///
/// # Examples
///
/// ```
/// use ihb_sender::config::{Config, Ingress, Variant};
///
/// let config = Config::from_pairs(Variant::Queue, [
///     ("WORKFLOW_ID", "phw-to-mpi"),
///     ("MICROSERVICE_ID", "hl7-sender-mpi"),
///     ("HEALTH_BOARD", "PHW"),
///     ("PEER_SERVICE", "MPI"),
///     ("INGRESS_QUEUE_NAME", "mpi-sender-ingress"),
///     ("INGRESS_SESSION_ID", "mpi"),
///     ("MESSAGE_STORE_QUEUE_NAME", "messagestore"),
/// ])
/// .expect("this configuration is complete");
///
/// assert_eq!(config.ingress.entity(), "mpi-sender-ingress");
/// assert_eq!(config.max_batch_size, 100);
/// assert!(config.max_messages_per_minute.is_none(), "unthrottled by default");
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
    /// Where messages are read from.
    pub ingress: Ingress,
    /// `MESSAGE_STORE_QUEUE_NAME`. `None` for a subscription sender, which
    /// does not write to the store (`spec/10-senders.md` §5.2).
    pub message_store_queue: Option<String>,
    /// `MAX_BATCH_SIZE`, before §2.3's reduction.
    pub max_batch_size: usize,
    /// `MAX_MESSAGES_PER_MINUTE`.
    pub max_messages_per_minute: Option<u32>,
    /// Where the health check binds.
    pub health_check: SocketAddr,
    /// `SERVICE_BUS_CONNECTION_STRING`.
    pub connection_string: Option<Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    pub namespace: Option<String>,
    /// `HL7_LOG_REDACTION_ENABLED`.
    pub redaction_enabled: bool,
}

/// Which ingress shape a service reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// A queue: `hl7-sender`, `soap-sender`.
    Queue,
    /// A topic subscription: the `-subscription-sender` services.
    Subscription,
}

impl Config {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_env(variant: Variant) -> Result<Self, ConfigErrors> {
        Self::read(variant, Reader::from_env())
    }

    /// Read from explicit pairs, for tests and examples.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_pairs<I, K, V>(variant: Variant, pairs: I) -> Result<Self, ConfigErrors>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self::read(variant, Reader::from_pairs(pairs))
    }

    /// The shared body of the two constructors.
    fn read(variant: Variant, mut reader: Reader) -> Result<Self, ConfigErrors> {
        let workflow_id = reader.required_string("WORKFLOW_ID");
        let microservice_id = reader.required_string("MICROSERVICE_ID");
        let health_board = reader.required_string("HEALTH_BOARD");
        let peer_service = reader.required_string("PEER_SERVICE");

        let ingress = match variant {
            Variant::Queue => Ingress::Queue {
                name: reader.required_string("INGRESS_QUEUE_NAME"),
                // Required here, unlike the subscription variant: every
                // pipeline queue is session-enabled
                // (`spec/01-architecture.md` §2.3.1).
                session_id: reader.required_string("INGRESS_SESSION_ID"),
            },
            Variant::Subscription => Ingress::Subscription {
                topic: reader.required_string("INGRESS_TOPIC_NAME"),
                subscription: reader.required_string("INGRESS_SUBSCRIPTION_NAME"),
                session_id: reader.optional_string("INGRESS_SESSION_ID"),
            },
        };

        let message_store_queue = match variant {
            Variant::Queue => Some(reader.required_string("MESSAGE_STORE_QUEUE_NAME")),
            // §5.2 — not read by a subscription sender at all. The message
            // was already stored by whichever component published to the
            // topic, and storing it again would double every row.
            Variant::Subscription => None,
        };

        let health_host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
        let health_port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");

        let config = Self {
            workflow_id,
            microservice_id,
            health_board,
            peer_service,
            ingress,
            message_store_queue,
            max_batch_size: reader.integer_or("MAX_BATCH_SIZE", DEFAULT_MAX_BATCH_SIZE),
            // §2.4.3 — a configured zero is a startup failure, which
            // `optional_positive_integer` is exactly.
            max_messages_per_minute: reader.optional_positive_integer("MAX_MESSAGES_PER_MINUTE"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_pairs() -> Vec<(&'static str, &'static str)> {
        vec![
            ("WORKFLOW_ID", "phw-to-mpi"),
            ("MICROSERVICE_ID", "hl7-sender-mpi"),
            ("HEALTH_BOARD", "PHW"),
            ("PEER_SERVICE", "MPI"),
            ("INGRESS_QUEUE_NAME", "mpi-sender-ingress"),
            ("INGRESS_SESSION_ID", "mpi"),
            ("MESSAGE_STORE_QUEUE_NAME", "messagestore"),
        ]
    }

    #[test]
    fn the_queue_variant_requires_a_session_and_a_store() {
        // spec/02-configuration.md §6.1.
        for missing in [
            "INGRESS_QUEUE_NAME",
            "INGRESS_SESSION_ID",
            "MESSAGE_STORE_QUEUE_NAME",
        ] {
            let pairs: Vec<_> = queue_pairs()
                .into_iter()
                .filter(|(k, _)| *k != missing)
                .collect();
            let errors = Config::from_pairs(Variant::Queue, pairs).unwrap_err();
            assert!(errors.to_string().contains(missing), "{errors}");
        }
    }

    #[test]
    fn the_subscription_variant_has_an_optional_session_and_no_store() {
        // spec/02-configuration.md §6.2 and spec/10-senders.md §5.2.
        let config = Config::from_pairs(
            Variant::Subscription,
            [
                ("WORKFLOW_ID", "mpi-to-topic"),
                ("MICROSERVICE_ID", "hl7-subscription-sender"),
                ("HEALTH_BOARD", "MPI"),
                ("PEER_SERVICE", "DOWNSTREAM"),
                ("INGRESS_TOPIC_NAME", "mpihl7input"),
                ("INGRESS_SUBSCRIPTION_NAME", "wcp"),
            ],
        )
        .expect("no session and no store is a complete configuration here");

        assert_eq!(config.ingress.session_id(), None);
        assert_eq!(
            config.message_store_queue, None,
            "§5.2 — the message was already stored by the publisher"
        );
        assert_eq!(
            config.ingress.entity(),
            "mpihl7input/wcp",
            "§5.3 — a subscription names itself this way"
        );
    }

    #[test]
    fn a_zero_rate_is_a_startup_failure() {
        // spec/10-senders.md §2.4.3 — zero asks for no messages at all.
        let mut pairs = queue_pairs();
        pairs.push(("MAX_MESSAGES_PER_MINUTE", "0"));
        assert!(Config::from_pairs(Variant::Queue, pairs).is_err());

        let mut pairs = queue_pairs();
        pairs.push(("MAX_MESSAGES_PER_MINUTE", "60"));
        assert_eq!(
            Config::from_pairs(Variant::Queue, pairs)
                .unwrap()
                .max_messages_per_minute,
            Some(60)
        );
    }

    #[test]
    fn an_unparseable_batch_size_fails_rather_than_defaulting() {
        let mut pairs = queue_pairs();
        pairs.push(("MAX_BATCH_SIZE", "many"));
        assert!(Config::from_pairs(Variant::Queue, pairs).is_err());
    }
}
