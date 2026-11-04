//! What one sender deployment is.
//!
//! Implements `spec/02-configuration.md` §6.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use ihb_config::{ConfigErrors, Reader, Secret};

/// Default for `MAX_BATCH_SIZE`: messages requested per receive, before
/// the throttle-driven reduction of `spec/10-senders.md` §2.3 clamps it so a
/// whole batch fits inside the lock-renewal window.
pub const DEFAULT_MAX_BATCH_SIZE: usize = 100;

/// Default `ACK_TIMEOUT_SECONDS`: the read timeout on the receiver's reply
/// (`spec/10-senders.md` §3). A timeout counts as "not delivered", so the
/// message is abandoned and redelivered, never completed.
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a sender reads from.
///
/// `spec/10-senders.md` §5.1 — the queue and subscription variants differ
/// in this and nothing else, which is why it is one type rather than two
/// services' worth of duplicated pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ingress {
    /// A queue (`spec/02-configuration.md` §6.1). The session is required
    /// for `soap-sender`, optional for `hl7-sender` (which may instead
    /// accept any session).
    Queue {
        /// `INGRESS_QUEUE_NAME`.
        name: String,
        /// `INGRESS_SESSION_ID`.
        session_id: Option<String>,
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
            Self::Queue { session_id, .. } | Self::Subscription { session_id, .. } => {
                session_id.as_deref()
            }
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
    /// `MESSAGE_STORE_QUEUE_NAME`, when `MESSAGE_STORE_ENABLED` is true
    /// (`spec/02-configuration.md` §2.1.2). `None` for a subscription sender,
    /// which does not write to the store (`spec/10-senders.md` §5.2), and for
    /// a queue sender that is not configured with one (§6.1.4).
    pub message_store_queue: Option<String>,
    /// A queue name ignored because the store is off (§2.1.2c), kept so
    /// startup can say so.
    pub message_store_unused_queue: Option<String>,
    /// `INGRESS_ACCEPT_ANY_SESSION`: take whichever session is available
    /// rather than one fixed session (`spec/02-configuration.md` §6.1).
    pub accept_any_session: bool,
    /// `REPLAY_SESSION_ID`: the session recorded on a stored message when
    /// neither the message nor `INGRESS_SESSION_ID` supplies one.
    pub replay_session_id: Option<String>,
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
///
/// It decides which environment variables are mandatory (`spec/02-configuration.md` §6). Chosen by the
/// binary, not by the operator: each service fixes its own variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// A session queue: `soap-sender`.
    Queue,
    /// A queue or a topic subscription, with a session that is fixed,
    /// accepted from any, or absent: `hl7-sender`.
    Hl7,
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
                session_id: Some(reader.required_string("INGRESS_SESSION_ID")),
            },
            Variant::Hl7 => read_hl7_ingress(&mut reader),
            Variant::Subscription => Ingress::Subscription {
                topic: reader.required_string("INGRESS_TOPIC_NAME"),
                subscription: reader.required_string("INGRESS_SUBSCRIPTION_NAME"),
                session_id: reader.optional_string("INGRESS_SESSION_ID"),
            },
        };

        let accept_any_session = match variant {
            Variant::Hl7 => reader.lenient_bool_or("INGRESS_ACCEPT_ANY_SESSION", false),
            Variant::Queue | Variant::Subscription => false,
        };
        let replay_session_id = match variant {
            Variant::Hl7 => reader.optional_string("REPLAY_SESSION_ID"),
            Variant::Queue | Variant::Subscription => None,
        };

        let store = match variant {
            // §6.1.4 — optional since upstream `387b20a`: a deployment may
            // run without the store, and an absent queue name means no
            // store sender rather than a startup failure.
            Variant::Queue | Variant::Hl7 => ihb_config::message_store(&reader),
            // §5.2 — not read by a subscription sender at all. The message
            // was already stored by whichever component published to the
            // topic, and storing it again would double every row.
            Variant::Subscription => ihb_config::MessageStore::default(),
        };
        let message_store_queue = store.queue;

        if variant == Variant::Hl7 {
            if accept_any_session && ingress.session_id().is_some() {
                reader.conflict(
                    "Invalid configuration: INGRESS_SESSION_ID cannot be set together with \
                     INGRESS_ACCEPT_ANY_SESSION=true. Configure only one session-selection mode.",
                );
            }
            if message_store_queue.is_some()
                && ingress.session_id().is_none()
                && replay_session_id.is_none()
                && !accept_any_session
            {
                reader.conflict(
                    "Missing required configuration: set REPLAY_SESSION_ID when \
                     MESSAGE_STORE_QUEUE_NAME is configured without INGRESS_SESSION_ID \
                     (unless INGRESS_ACCEPT_ANY_SESSION=true).",
                );
            }
        }

        let health_host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
        let health_port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");

        let config = Self {
            workflow_id,
            microservice_id,
            health_board,
            peer_service,
            ingress,
            message_store_queue,
            message_store_unused_queue: store.unused_queue,
            accept_any_session,
            replay_session_id,
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

/// `hl7-sender`'s ingress: a queue xor a topic with its subscription, the
/// session fixed, accepted from any, or absent (`spec/02-configuration.md`
/// §6.1). The messages are the Python's, shared with the transformers.
fn read_hl7_ingress(reader: &mut Reader) -> Ingress {
    let queue = reader.optional_string("INGRESS_QUEUE_NAME");
    let topic = reader.optional_string("INGRESS_TOPIC_NAME");
    let subscription = reader.optional_string("INGRESS_SUBSCRIPTION_NAME");
    let session_id = reader.optional_string("INGRESS_SESSION_ID");

    match (queue, topic, subscription) {
        (Some(name), None, None) => Ingress::Queue { name, session_id },
        (Some(name), _, _) => {
            reader.conflict(
                "Invalid ingress configuration: INGRESS_QUEUE_NAME cannot be set together with \
                 INGRESS_TOPIC_NAME/INGRESS_SUBSCRIPTION_NAME. Configure only one ingress transport.",
            );
            Ingress::Queue { name, session_id }
        }
        (None, Some(topic), Some(subscription)) => Ingress::Subscription {
            topic,
            subscription,
            session_id,
        },
        (None, None, None) => {
            reader.conflict(
                "Missing required configuration: set either INGRESS_QUEUE_NAME, or both \
                 INGRESS_TOPIC_NAME and INGRESS_SUBSCRIPTION_NAME.",
            );
            Ingress::Queue {
                name: String::new(),
                session_id,
            }
        }
        (None, topic, subscription) => {
            reader.conflict(
                "Missing required configuration: INGRESS_TOPIC_NAME and \
                 INGRESS_SUBSCRIPTION_NAME must both be set when using topic-based ingress.",
            );
            Ingress::Subscription {
                topic: topic.unwrap_or_default(),
                subscription: subscription.unwrap_or_default(),
                session_id,
            }
        }
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
    fn the_queue_variant_requires_a_session_but_not_a_store() {
        // spec/02-configuration.md §6.1 and §6.1.4 — the store became
        // optional upstream in `387b20a`.
        for missing in ["INGRESS_QUEUE_NAME", "INGRESS_SESSION_ID"] {
            let pairs: Vec<_> = queue_pairs()
                .into_iter()
                .filter(|(k, _)| *k != missing)
                .collect();
            let errors = Config::from_pairs(Variant::Queue, pairs).unwrap_err();
            assert!(errors.to_string().contains(missing), "{errors}");
        }

        let without_store: Vec<_> = queue_pairs()
            .into_iter()
            .filter(|(k, _)| *k != "MESSAGE_STORE_QUEUE_NAME")
            .collect();
        let config = Config::from_pairs(Variant::Queue, without_store).unwrap();
        assert_eq!(config.message_store_queue, None);
    }

    fn hl7_with(extra: &[(&str, &str)]) -> Result<Config, ConfigErrors> {
        let mut pairs: Vec<(String, String)> = [
            ("WORKFLOW_ID", "w"),
            ("MICROSERVICE_ID", "m"),
            ("HEALTH_BOARD", "H"),
            ("PEER_SERVICE", "P"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
        pairs.extend(
            extra
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
        );
        Config::from_pairs(Variant::Hl7, pairs)
    }

    #[test]
    fn the_hl7_variant_takes_a_queue_or_a_topic_and_an_optional_session() {
        // spec/02-configuration.md §6.1, upstream `hl7_sender/app_config.py`.
        let queue = hl7_with(&[("INGRESS_QUEUE_NAME", "q")]).unwrap();
        assert_eq!(queue.ingress.entity(), "q");
        assert_eq!(queue.ingress.session_id(), None);

        let topic = hl7_with(&[
            ("INGRESS_TOPIC_NAME", "mpi"),
            ("INGRESS_SUBSCRIPTION_NAME", "wcp"),
            ("INGRESS_ACCEPT_ANY_SESSION", "true"),
        ])
        .unwrap();
        assert_eq!(topic.ingress.entity(), "mpi/wcp");
        assert!(topic.accept_any_session);

        for (extra, wanted) in [
            (vec![], "set either INGRESS_QUEUE_NAME"),
            (
                vec![("INGRESS_QUEUE_NAME", "q"), ("INGRESS_TOPIC_NAME", "t")],
                "Configure only one ingress transport",
            ),
            (vec![("INGRESS_TOPIC_NAME", "t")], "must both be set"),
        ] {
            let error = hl7_with(&extra).unwrap_err().to_string();
            assert!(error.contains(wanted), "{error}");
        }
    }

    #[test]
    fn a_fixed_session_and_accept_any_are_exclusive() {
        // spec/02-configuration.md §6.1.
        let error = hl7_with(&[
            ("INGRESS_QUEUE_NAME", "q"),
            ("INGRESS_SESSION_ID", "s"),
            ("INGRESS_ACCEPT_ANY_SESSION", "true"),
        ])
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("Configure only one session-selection mode"),
            "{error}"
        );
    }

    #[test]
    fn a_store_needs_somewhere_to_get_its_session_from() {
        // spec/02-configuration.md §6.1 — replay puts a stored message back
        // on a session, so one must be knowable.
        // The queue name is ignored unless the store is enabled (§2.1.2), so
        // the test has to switch it on for the session check to apply.
        let store = ("MESSAGE_STORE_QUEUE_NAME", "messagestore");
        let enabled = ("MESSAGE_STORE_ENABLED", "true");
        let queue = ("INGRESS_QUEUE_NAME", "q");

        let error = hl7_with(&[queue, store, enabled]).unwrap_err().to_string();
        assert!(error.contains("set REPLAY_SESSION_ID"), "{error}");

        for ok in [
            vec![queue, store, enabled, ("REPLAY_SESSION_ID", "r")],
            vec![queue, store, enabled, ("INGRESS_SESSION_ID", "s")],
            vec![
                queue,
                store,
                enabled,
                ("INGRESS_ACCEPT_ANY_SESSION", "true"),
            ],
        ] {
            hl7_with(&ok).unwrap();
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
