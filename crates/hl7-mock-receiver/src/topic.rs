//! What the topic variant of the mock receiver is.
//!
//! Implements `spec/11-mock-receivers.md` §5 and `spec/02-configuration.md`
//! §8.1a.
//!
//! Everything except the egress is [`crate::config::Config`]'s: the same
//! listener, the same size limit. What differs is that the destination is a
//! topic and is **optional** (§5.2) — the Python service this ports could
//! be started with no Service Bus configuration at all, so a developer's
//! tooling can run it standalone.

use std::net::SocketAddr;

use ihb_config::{ConfigErrors, Reader, Secret};

use crate::config::{DEFAULT_PORT, socket_addr};

/// Everything the topic receiver needs to start.
///
/// # Examples
///
/// ```
/// use hl7_mock_receiver::topic::TopicConfig;
///
/// let config = TopicConfig::from_pairs([("EGRESS_TOPIC_NAME", "mpi-topic")])
///     .expect("a topic name is all this needs");
///
/// assert_eq!(config.listen.port(), 2576);
/// assert!(!config.forwards(), "a topic with no connection is log-only (§5.2)");
/// ```
#[derive(Debug, Clone)]
pub struct TopicConfig {
    /// Where the MLLP listener binds.
    pub listen: SocketAddr,
    /// Where the health check binds.
    pub health_check: SocketAddr,
    /// `EGRESS_TOPIC_NAME`. Optional (§5.2).
    pub egress_topic_name: Option<String>,
    /// `EGRESS_SESSION_ID`.
    pub egress_session_id: Option<String>,
    /// `SERVICE_BUS_CONNECTION_STRING`, a [`Secret`] so the shared-access
    /// key never reaches a log line.
    pub connection_string: Option<Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    pub namespace: Option<String>,
    /// `MAX_MESSAGE_SIZE_BYTES`: the MLLP framer's per-frame limit, in bytes.
    pub max_message_size: usize,
}

impl TopicConfig {
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
        let health_host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
        let health_port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");

        let config = Self {
            listen: socket_addr(&host, port),
            health_check: socket_addr(
                &health_host,
                health_port.unwrap_or(ihb_health::DEFAULT_PORT),
            ),
            egress_topic_name: reader.optional_string("EGRESS_TOPIC_NAME"),
            egress_session_id: reader.optional_string("EGRESS_SESSION_ID"),
            connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
            namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
            max_message_size: reader
                .optional_positive_integer("MAX_MESSAGE_SIZE_BYTES")
                .unwrap_or(hl7_2_mllp::DEFAULT_LIMIT),
        };

        reader.finish()?;
        Ok(config)
    }

    /// Whether there is any connection information at all: a connection
    /// string or a namespace. Says nothing about a topic; see
    /// [`Self::forwards`].
    #[must_use]
    pub const fn has_broker(&self) -> bool {
        self.connection_string.is_some() || self.namespace.is_some()
    }

    /// Whether messages are forwarded: a topic **and** a connection
    /// (`spec/11-mock-receivers.md` §5.2).
    #[must_use]
    pub const fn forwards(&self) -> bool {
        self.egress_topic_name.is_some() && self.has_broker()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(extra: &[(&str, &str)]) -> TopicConfig {
        TopicConfig::from_pairs(extra.iter().copied()).unwrap()
    }

    #[test]
    fn no_variables_at_all_is_valid_and_log_only() {
        // spec/11-mock-receivers.md §5.2 — unlike §2, nothing is required.
        let config = pairs(&[]);
        assert_eq!(config.listen.port(), DEFAULT_PORT);
        assert!(!config.forwards());
    }

    #[test]
    fn a_topic_alone_is_log_only() {
        // §5.2 — the Python's `service_bus_enabled`.
        let config = pairs(&[("EGRESS_TOPIC_NAME", "t")]);
        assert!(config.egress_topic_name.is_some());
        assert!(!config.forwards());
    }

    #[test]
    fn a_connection_alone_is_log_only() {
        let config = pairs(&[("SERVICE_BUS_NAMESPACE", "ns.servicebus.windows.net")]);
        assert!(config.has_broker());
        assert!(!config.forwards());
    }

    #[test]
    fn topic_plus_connection_string_forwards() {
        let config = pairs(&[
            ("EGRESS_TOPIC_NAME", "t"),
            ("SERVICE_BUS_CONNECTION_STRING", "Endpoint=sb://x;"),
        ]);
        assert!(config.forwards());
    }

    #[test]
    fn topic_plus_namespace_forwards() {
        let config = pairs(&[
            ("EGRESS_TOPIC_NAME", "t"),
            ("SERVICE_BUS_NAMESPACE", "ns.servicebus.windows.net"),
        ]);
        assert!(config.forwards());
    }

    #[test]
    fn the_queue_variable_is_not_read() {
        // §5.3 — and an unread variable is not an error.
        let config = pairs(&[("EGRESS_QUEUE_NAME", "q")]);
        assert!(config.egress_topic_name.is_none());
    }

    #[test]
    fn blank_is_absent() {
        // spec/02-configuration.md §1.1.
        let config = pairs(&[("EGRESS_TOPIC_NAME", "  "), ("EGRESS_SESSION_ID", "")]);
        assert!(config.egress_topic_name.is_none());
        assert!(config.egress_session_id.is_none());
    }
}
