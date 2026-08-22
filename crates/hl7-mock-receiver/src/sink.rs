//! Where a received message goes after it has been forwarded.
//!
//! Implements `spec/11-mock-receivers.md` §2.2 and §2.6.
//!
//! The mock used to carry its own `MessageSink` trait and its own
//! `azservicebus` client. Both are gone: it uses
//! [`ihb_bus::sink::MessageSink`] like every other service here, so the
//! retry classification, the plaintext fence of
//! `spec/03-message-bus.md` §8.1.1, and the connection handling are the
//! ones already tested rather than a second implementation that could
//! disagree.

use ihb_bus::error::BusError;
use ihb_bus::message::Outgoing;
use ihb_bus::sink::MessageSink;

/// No broker: log what would have been forwarded and carry on.
///
/// `spec/11-mock-receivers.md` §2.6 — this is what the receiver falls back
/// to when neither a connection string nor a namespace is configured, so
/// `cargo run` works with nothing but a queue name.
///
/// It reports success, which is the right answer: the mock's contract is
/// that it acknowledges what it takes, and with no broker configured
/// nothing was asked to be forwarded. A failure here would make every
/// laptop run answer `AE`.
#[derive(Debug, Clone)]
pub struct LoggingSink {
    /// The queue this stands in for.
    entity: String,
}

impl LoggingSink {
    /// A sink that names the queue it is standing in for.
    #[must_use]
    pub fn new(entity: impl Into<String>) -> Self {
        Self {
            entity: entity.into(),
        }
    }
}

impl MessageSink for LoggingSink {
    /// Log and succeed.
    ///
    /// Written as a ready future rather than an `async fn`: there is
    /// nothing to await, and an `async` body with no suspension point would
    /// only obscure that.
    fn send(
        &self,
        message: Outgoing,
    ) -> impl std::future::Future<Output = Result<(), BusError>> + Send {
        tracing::info!(
            queue = %self.entity,
            message_id = ?message.message_id,
            bytes = message.body.len(),
            "not forwarding: no Service Bus configured"
        );
        std::future::ready(Ok(()))
    }

    fn entity(&self) -> &str {
        &self.entity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_logging_sink_accepts_everything() {
        // spec/11-mock-receivers.md §2.6 — with no broker the mock must
        // still answer `AA`, so its sink must not fail.
        let sink = LoggingSink::new("mpi-mock-receiver");

        assert!(sink.send(Outgoing::new("anything")).await.is_ok());
        assert_eq!(sink.entity(), "mpi-mock-receiver");
    }
}
