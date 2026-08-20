//! A message, as it crosses the bus.
//!
//! Implements `spec/03-message-bus.md` §3.1 and §4.2.
//!
//! Deliberately not `azservicebus`'s own type. Two reasons, and the second
//! is the one that matters: a service tested against the fake bus should not
//! have to construct an AMQP message, and the disposition rules of §5.2 are
//! easier to get right when "the thing being disposed of" is ours.

use crate::metadata::Properties;

/// A message to send.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outgoing {
    /// The payload.
    pub body: Vec<u8>,
    /// The Service Bus message id.
    ///
    /// Set to `MSH-10` at ingress, so duplicate detection keys on the source
    /// system's own identifier (`spec/01-architecture.md` §5.2). Left unset
    /// elsewhere.
    pub message_id: Option<String>,
    /// The session to send under, for a session-enabled queue.
    pub session_id: Option<String>,
    /// The tracking metadata and trace context.
    ///
    /// An **empty map is sent as absent**, not as an empty map
    /// (`spec/03-message-bus.md` §3.1.1).
    pub properties: Properties,
}

impl Outgoing {
    /// A message carrying `body`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_bus::message::Outgoing;
    ///
    /// let message = Outgoing::new("MSH|^~\\&|252").with_message_id("MSG1");
    /// assert_eq!(message.message_id.as_deref(), Some("MSG1"));
    /// ```
    #[must_use]
    pub fn new(body: impl Into<Vec<u8>>) -> Self {
        Self {
            body: body.into(),
            ..Self::default()
        }
    }

    /// Set the Service Bus message id.
    #[must_use]
    pub fn with_message_id(mut self, id: impl Into<String>) -> Self {
        self.message_id = Some(id.into());
        self
    }

    /// Set the session.
    #[must_use]
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session_id = Some(session.into());
        self
    }

    /// Set the session, when there is one.
    #[must_use]
    pub fn with_optional_session(mut self, session: Option<&str>) -> Self {
        self.session_id = session.map(str::to_string);
        self
    }

    /// Attach application properties.
    #[must_use]
    pub fn with_properties(mut self, properties: Properties) -> Self {
        self.properties = properties;
        self
    }

    /// The body as text, when it is valid UTF-8.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_bus::message::Outgoing;
    ///
    /// assert_eq!(Outgoing::new("hello").text().as_deref(), Some("hello"));
    /// assert_eq!(Outgoing::new(vec![0xff]).text(), None);
    /// ```
    #[must_use]
    pub fn text(&self) -> Option<String> {
        String::from_utf8(self.body.clone()).ok()
    }
}

/// A message that was received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// The payload.
    pub body: Vec<u8>,
    /// The Service Bus message id, when the sender set one.
    pub message_id: Option<String>,
    /// The session it arrived on.
    pub session_id: Option<String>,
    /// The application properties, with non-string values already dropped
    /// per `spec/03-message-bus.md` §4.2.
    pub properties: Properties,
    /// How many times this message has been delivered.
    ///
    /// `None` when the broker did not say. A sender treats that as a first
    /// attempt (`spec/10-senders.md` §2.2).
    pub delivery_count: Option<u32>,
    /// An opaque handle, so the message can be completed or abandoned.
    pub token: Token,
}

impl Incoming {
    /// The body as text, when it is valid UTF-8.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        String::from_utf8(self.body.clone()).ok()
    }

    /// Whether this is the first delivery attempt.
    ///
    /// A missing or zero delivery count means yes. Implements
    /// `spec/10-senders.md` §2.2: the store event is emitted once, on the
    /// first attempt, so a retried message is not stored again.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_bus::message::{Incoming, Token};
    /// # use ihb_bus::metadata::Properties;
    ///
    /// let mut message = Incoming {
    ///     body: b"x".to_vec(),
    ///     message_id: None,
    ///     session_id: None,
    ///     properties: Properties::new(),
    ///     delivery_count: None,
    ///     token: Token::fake(1),
    /// };
    ///
    /// // The broker said nothing: treat it as the first attempt.
    /// assert!(message.is_first_delivery());
    ///
    /// message.delivery_count = Some(2);
    /// assert!(!message.is_first_delivery());
    /// ```
    #[must_use]
    pub const fn is_first_delivery(&self) -> bool {
        match self.delivery_count {
            None => true,
            Some(count) => count <= 1,
        }
    }
}

/// A handle to one received message, for completing or abandoning it.
///
/// Opaque on purpose: what it holds differs between the real bus and the
/// fake, and nothing outside this crate should be able to forge one for a
/// real message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token(pub(crate) u64);

impl Token {
    /// A token for a fake message, for tests.
    #[must_use]
    pub const fn fake(id: u64) -> Self {
        Self(id)
    }
}

/// What was decided about a received message.
///
/// The vocabulary of `spec/index.md` §0.9, and the reason it is an enum
/// rather than a boolean: "processed" and "will never succeed" are different
/// outcomes that happen to share a disposition, and a handler that cannot
/// say which is a handler that will eventually get one of them wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Processed and forwarded. Complete it.
    Accepted,
    /// Structurally or semantically invalid; it will never succeed.
    /// Complete it, having logged why.
    Rejected,
    /// A transient failure. Abandon it, and everything behind it.
    Deferred,
}

impl Outcome {
    /// Whether the message is finished with, either way.
    #[must_use]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::Accepted | Self::Rejected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incoming(delivery_count: Option<u32>) -> Incoming {
        Incoming {
            body: b"MSH|^~\\&|252".to_vec(),
            message_id: Some("MSG1".into()),
            session_id: Some("phw-to-mpi".into()),
            properties: Properties::new(),
            delivery_count,
            token: Token::fake(1),
        }
    }

    #[test]
    fn a_missing_delivery_count_is_a_first_attempt() {
        // spec/10-senders.md §2.2 — the safe reading. Storing a message
        // twice is better than never storing it.
        assert!(incoming(None).is_first_delivery());
    }

    #[test]
    fn delivery_counts_of_zero_and_one_are_both_first_attempts() {
        // Measured against the emulator, which reports 1 on first delivery
        // and 2 after an abandon (spec/03-message-bus.md §8).
        assert!(incoming(Some(0)).is_first_delivery());
        assert!(incoming(Some(1)).is_first_delivery());
    }

    #[test]
    fn a_redelivered_message_is_not_a_first_attempt() {
        assert!(!incoming(Some(2)).is_first_delivery());
        assert!(!incoming(Some(10)).is_first_delivery());
    }

    #[test]
    fn a_body_that_is_not_utf8_reads_as_none_rather_than_lossy() {
        // spec/index.md §0.6 — a payload that is not UTF-8 is rejected at
        // the boundary, not silently mangled into something forwardable.
        let mut message = incoming(None);
        message.body = vec![0xff, 0xfe];
        assert_eq!(message.text(), None);
    }

    #[test]
    fn an_outgoing_message_builds_by_parts() {
        let message = Outgoing::new("MSH|^~\\&|252")
            .with_message_id("MSG1")
            .with_session("phw-to-mpi");

        assert_eq!(message.text().as_deref(), Some("MSH|^~\\&|252"));
        assert_eq!(message.message_id.as_deref(), Some("MSG1"));
        assert_eq!(message.session_id.as_deref(), Some("phw-to-mpi"));
    }

    #[test]
    fn an_absent_session_is_absent_rather_than_empty() {
        let message = Outgoing::new("x").with_optional_session(None);
        assert_eq!(message.session_id, None);
    }

    #[test]
    fn both_settled_outcomes_are_settled_and_deferred_is_not() {
        // spec/index.md §0.9
        assert!(Outcome::Accepted.is_settled());
        assert!(Outcome::Rejected.is_settled());
        assert!(!Outcome::Deferred.is_settled());
    }
}
