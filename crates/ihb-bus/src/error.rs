//! What can go wrong with the bus.
//!
//! Implements `spec/03-message-bus.md` §3.2 and §5.3.
//!
//! As everywhere in this workspace, an error carries **where and why**, never
//! a message payload (`spec/index.md` §0.8.3). A queue name and a
//! disposition are operational; the message that failed to send is not, and
//! it is already in the message store.

use crate::retry::Disposition;

/// Why a bus operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BusError {
    /// Neither a connection string nor a namespace was configured.
    #[error(
        "no Service Bus endpoint configured: set SERVICE_BUS_CONNECTION_STRING or SERVICE_BUS_NAMESPACE"
    )]
    NotConfigured,

    /// The connection could not be established.
    #[error("could not connect to Service Bus namespace {namespace}: {reason}")]
    Connect {
        /// The namespace that was being reached.
        namespace: String,
        /// Why, structurally.
        reason: String,
    },

    /// A send failed, after whatever retries its classification allowed.
    #[error("could not send to {destination} after {attempts} attempt(s): {reason}")]
    Send {
        /// The queue or topic.
        destination: String,
        /// How many attempts were made.
        attempts: usize,
        /// Why, structurally. Never a payload.
        reason: String,
        /// What the failure was classified as.
        disposition: Disposition,
    },

    /// A single message could not fit an empty batch.
    ///
    /// Unrecoverable by construction, and called out separately because the
    /// remedy is different: the message has to be split or rejected, not
    /// retried (`spec/03-message-bus.md` §3.3.2).
    #[error("a single message exceeds the maximum batch size for {destination}")]
    MessageTooLarge {
        /// The queue or topic.
        destination: String,
    },

    /// A receive failed.
    #[error("could not receive from {entity}: {reason}")]
    Receive {
        /// The queue, or `topic/subscription`. Named `entity` rather than
        /// `source` because `thiserror` reads a field called `source` as an
        /// error cause.
        entity: String,
        /// Why, structurally.
        reason: String,
    },

    /// A message could not be completed, abandoned or dead-lettered.
    #[error("could not {action} a message from {entity}: {reason}")]
    Disposition {
        /// What was attempted: `complete`, `abandon`, `dead-letter`.
        action: &'static str,
        /// The queue, or `topic/subscription`.
        entity: String,
        /// Why, structurally.
        reason: String,
    },
}

impl BusError {
    /// Whether another attempt is worth making.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_bus::{BusError, retry::Disposition};
    ///
    /// let fatal = BusError::MessageTooLarge { destination: "q".into() };
    /// assert!(!fatal.is_retryable());
    ///
    /// let transient = BusError::Receive { entity: "q".into(), reason: "timeout".into() };
    /// assert!(transient.is_retryable());
    /// ```
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            // Configuration, not weather.
            Self::NotConfigured | Self::MessageTooLarge { .. } => false,
            Self::Send { disposition, .. } => disposition.is_retryable(),
            // A connection or receive failure is the normal transient case;
            // the receive loop's backoff is what bounds it.
            Self::Connect { .. } | Self::Receive { .. } | Self::Disposition { .. } => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_endpoint_says_which_variables_to_set() {
        // The first thing anyone does with this error is go looking for the
        // variable name.
        let rendered = BusError::NotConfigured.to_string();
        assert!(rendered.contains("SERVICE_BUS_CONNECTION_STRING"));
        assert!(rendered.contains("SERVICE_BUS_NAMESPACE"));
    }

    #[test]
    fn a_send_failure_names_the_destination_and_the_attempts() {
        let error = BusError::Send {
            destination: "local-inthub-mpi-sender-ingress".into(),
            attempts: 3,
            reason: "operation timed out".into(),
            disposition: Disposition::Retry,
        };
        let rendered = error.to_string();
        assert!(rendered.contains("local-inthub-mpi-sender-ingress"));
        assert!(rendered.contains("3 attempt"));
    }

    #[test]
    fn an_oversize_message_is_not_retryable() {
        // spec/03-message-bus.md §3.3.2
        assert!(
            !BusError::MessageTooLarge {
                destination: "q".into()
            }
            .is_retryable()
        );
    }

    #[test]
    fn a_fatal_send_is_not_retryable_but_a_transient_one_is() {
        let fatal = BusError::Send {
            destination: "q".into(),
            attempts: 1,
            reason: "too large".into(),
            disposition: Disposition::Fatal,
        };
        assert!(!fatal.is_retryable());

        let transient = BusError::Send {
            destination: "q".into(),
            attempts: 3,
            reason: "timeout".into(),
            disposition: Disposition::Retry,
        };
        assert!(transient.is_retryable());
    }

    #[test]
    fn no_error_variant_can_hold_a_payload() {
        // spec/index.md §0.8.3, enforced by the type rather than by care:
        // every field is a name, a count, a reason or a disposition.
        //
        // If a variant ever gains a payload field this test will not catch
        // it — but the reviewer reading this comment will.
        let errors = [
            BusError::NotConfigured,
            BusError::Connect {
                namespace: "n".into(),
                reason: "r".into(),
            },
            BusError::MessageTooLarge {
                destination: "d".into(),
            },
            BusError::Receive {
                entity: "s".into(),
                reason: "r".into(),
            },
        ];
        for error in errors {
            let rendered = error.to_string();
            assert!(!rendered.contains("MSH|"), "{rendered}");
        }
    }
}
