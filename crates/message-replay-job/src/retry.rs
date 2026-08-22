//! Classifying a send failure.
//!
//! Implements `spec/13-message-replay.md` §5.2.

use ihb_bus::error::BusError;

/// What to do about a first send failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFailure {
    /// A message too large for the entity. **Not retried** — it will not
    /// become smaller, and retrying costs a round trip to reach the same
    /// answer.
    TooLarge,

    /// A timeout. Retried, **and the operator is warned that the retry may
    /// produce duplicates** (§5.3): the broker may already have accepted
    /// the timed-out send, and there is no way from here to find out.
    Timeout,

    /// Anything else. Retried.
    Other,
}

impl SendFailure {
    /// Whether a second attempt is worth making.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        !matches!(self, Self::TooLarge)
    }

    /// Whether the retry could double-deliver.
    ///
    /// §5.3 makes this the operator's business rather than an
    /// implementation detail: a replay that double-delivers puts the same
    /// patient record into a clinical system twice, and whoever ordered the
    /// replay needs to know it might have happened.
    #[must_use]
    pub const fn may_duplicate(self) -> bool {
        matches!(self, Self::Timeout)
    }
}

/// Classify a send failure (§5.2).
///
/// # Examples
///
/// ```
/// use ihb_bus::error::BusError;
/// use message_replay_job::retry::{classify, SendFailure};
///
/// let too_large = BusError::MessageTooLarge {
///     destination: "priority".to_string(),
/// };
/// assert_eq!(classify(&too_large), SendFailure::TooLarge);
/// assert!(!classify(&too_large).is_retryable());
/// ```
#[must_use]
pub fn classify(error: &BusError) -> SendFailure {
    match error {
        BusError::MessageTooLarge { .. } => SendFailure::TooLarge,
        other => {
            // A timeout is recognised from the reason text, because that is
            // where `azservicebus` puts it — the same route
            // `ihb_bus::retry::classify` takes.
            let text = other.to_string().to_ascii_lowercase();
            if text.contains("timed out") || text.contains("timeout") {
                SendFailure::Timeout
            } else {
                SendFailure::Other
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ihb_bus::retry::Disposition;

    fn send_error(reason: &str) -> BusError {
        BusError::Send {
            destination: "local-inthub-priority-messagequeue".to_string(),
            attempts: 1,
            reason: reason.to_string(),
            disposition: Disposition::Retry,
        }
    }

    #[test]
    fn every_row_of_the_classification_table() {
        // spec/13-message-replay.md §5.2 and §8.2.
        let too_large = BusError::MessageTooLarge {
            destination: "q".to_string(),
        };
        assert_eq!(classify(&too_large), SendFailure::TooLarge);

        assert_eq!(
            classify(&send_error("the operation timed out")),
            SendFailure::Timeout
        );
        assert_eq!(
            classify(&send_error("Timeout waiting for the link")),
            SendFailure::Timeout
        );

        assert_eq!(
            classify(&send_error("the link was detached")),
            SendFailure::Other
        );
    }

    #[test]
    fn an_oversize_message_is_not_retried() {
        // §5.2's first row, and the reason: it will not become smaller.
        let too_large = BusError::MessageTooLarge {
            destination: "q".to_string(),
        };

        assert!(!classify(&too_large).is_retryable());
        assert!(!classify(&too_large).may_duplicate());
    }

    #[test]
    fn a_timeout_is_retried_and_flagged_as_possibly_duplicating() {
        // §5.2's second row and §5.3 — the warning the operator needs.
        let timeout = classify(&send_error("the operation timed out"));

        assert!(timeout.is_retryable());
        assert!(
            timeout.may_duplicate(),
            "a timed-out send may already have reached the broker"
        );
    }

    #[test]
    fn anything_else_is_retried_without_the_duplicate_warning() {
        // §5.2's third row. The warning is reserved for the case that
        // genuinely could double-deliver; attaching it to every failure
        // would train operators to ignore it.
        let other = classify(&send_error("the link was detached"));

        assert!(other.is_retryable());
        assert!(!other.may_duplicate());
    }
}
