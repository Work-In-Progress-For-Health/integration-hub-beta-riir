//! What to do when a send fails.
//!
//! Implements `spec/03-message-bus.md` §3.2 and §5.3.
//!
//! The classification matters more than the retry count. A message that is
//! too large will still be too large on the third attempt, and retrying it
//! costs three round trips and delays every message behind it in the
//! session. A dropped AMQP link, by contrast, is fixed by rebuilding the
//! sender and trying again.
//!
//! Getting this wrong in the permissive direction turns a poison message
//! into an infinite loop; getting it wrong in the strict direction drops a
//! clinical message that would have gone through a moment later.

/// How many times a send is attempted in total.
///
/// From `message_sender_client.py :: MAX_SERVICE_BUS_RETRIES`.
pub const MAX_SEND_ATTEMPTS: usize = 3;

/// What to do about a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Try again as-is.
    Retry,
    /// Rebuild the client first, then try again.
    ///
    /// For a link or session that has gone stale underneath us: the retry
    /// cannot succeed on the same connection.
    Rebuild,
    /// Give up. This will not succeed however many times it is tried.
    Fatal,
}

impl Disposition {
    /// Whether another attempt is worth making.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Retry | Self::Rebuild)
    }
}

/// Classify a failure from its rendered text.
///
/// Text rather than a typed error, deliberately: `azservicebus` surfaces
/// several of these as `azure_core::Error` with the distinguishing detail
/// only in the message, and the alternative is matching on a string
/// *somewhere* — better here, in one place, with the reasoning next to it,
/// than scattered through the senders.
///
/// # Examples
///
/// ```
/// use ihb_bus::retry::{classify, Disposition};
///
/// // Too large now is too large later.
/// assert_eq!(classify("MessageSizeExceeded: the message is too large"), Disposition::Fatal);
///
/// // A timeout might not be.
/// assert_eq!(classify("operation timed out"), Disposition::Retry);
///
/// // A stale link needs the client rebuilt first.
/// assert_eq!(classify("no attribute create_receiver_link"), Disposition::Rebuild);
/// ```
#[must_use]
pub fn classify(error: &str) -> Disposition {
    let lowered = error.to_ascii_lowercase();

    // Fatal first: a message that cannot fit will not start fitting, and
    // retrying it blocks the session behind it.
    // spec/03-message-bus.md §3.2.2.
    if lowered.contains("messagesizeexceeded")
        || lowered.contains("message size")
        || lowered.contains("too large")
    {
        return Disposition::Fatal;
    }

    // Authorisation and entity-not-found are configuration, not weather.
    // Retrying cannot fix a queue that does not exist, and the retries hide
    // the real problem behind a timeout.
    if lowered.contains("unauthorized")
        || lowered.contains("unauthorised")
        || lowered.contains("messagingentitynotfound")
        || lowered.contains("entity not found")
    {
        return Disposition::Fatal;
    }

    // A link or session that has gone stale underneath us. The Python found
    // these surfacing as an AttributeError deep inside the AMQP transport;
    // the shape differs here but the remedy is the same — rebuild, then
    // retry. spec/03-message-bus.md §3.2.2, §5.3.
    if lowered.contains("create_receiver_link")
        || lowered.contains("client_ready")
        || lowered.contains("link is closed")
        || lowered.contains("connection is closed")
        || lowered.contains("session is closed")
        || lowered.contains("detached")
    {
        return Disposition::Rebuild;
    }

    // A session held by another receiver. Not an error in any real sense —
    // the emulator does not release a session lock the instant `dispose`
    // returns, so this happens on every reconnect.
    // spec/03-message-bus.md §8.1.3.
    if lowered.contains("session-cannot-be-locked") || lowered.contains("sessioncannotbelocked") {
        return Disposition::Retry;
    }

    // Everything else: assume it is weather. A send that cannot succeed will
    // exhaust its attempts in a few seconds and surface then.
    Disposition::Retry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_three_attempts() {
        // spec/03-message-bus.md §3.2.1
        assert_eq!(MAX_SEND_ATTEMPTS, 3);
    }

    #[test]
    fn a_message_that_is_too_large_is_never_retried() {
        // spec/03-message-bus.md §3.2.2 — the row that matters most. A
        // poison message must not block its session for three round trips.
        for error in [
            "MessageSizeExceededError",
            "the message size exceeds the limit",
            "payload too large",
        ] {
            assert_eq!(classify(error), Disposition::Fatal, "{error}");
            assert!(!classify(error).is_retryable(), "{error}");
        }
    }

    #[test]
    fn a_configuration_failure_is_not_retried() {
        // Retrying a queue that does not exist hides the real problem.
        for error in [
            "Unauthorized access",
            "MessagingEntityNotFoundError",
            "the entity not found",
        ] {
            assert_eq!(classify(error), Disposition::Fatal, "{error}");
        }
    }

    #[test]
    fn a_timeout_is_retried() {
        // spec/03-message-bus.md §3.2.2
        assert_eq!(classify("operation timed out"), Disposition::Retry);
        assert_eq!(classify("OperationTimeoutError"), Disposition::Retry);
    }

    #[test]
    fn a_stale_link_is_rebuilt_before_retrying() {
        // spec/03-message-bus.md §3.2.2, §5.3 — a retry on the same
        // connection cannot succeed.
        for error in [
            "'NoneType' object has no attribute 'create_receiver_link'",
            "client_ready",
            "the link is closed",
            "connection is closed",
            "link detached by peer",
        ] {
            assert_eq!(classify(error), Disposition::Rebuild, "{error}");
            assert!(classify(error).is_retryable(), "{error}");
        }
    }

    #[test]
    fn a_held_session_is_retried() {
        // spec/03-message-bus.md §8.1.3 — measured against the emulator: a
        // session lock outlives `dispose`, so this happens on reconnect and
        // must not be fatal.
        let error = "com.microsoft:session-cannot-be-locked: The requested session \
                     'phw-to-mpi' cannot be accepted.";
        assert_eq!(classify(error), Disposition::Retry);
    }

    #[test]
    fn an_unrecognised_failure_is_treated_as_weather() {
        // The safe default: a send that truly cannot succeed exhausts its
        // attempts in seconds and surfaces then. Defaulting to fatal would
        // drop clinical messages on any error nobody had seen before.
        assert_eq!(classify("something nobody has seen"), Disposition::Retry);
        assert_eq!(classify(""), Disposition::Retry);
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(classify("MESSAGESIZEEXCEEDED"), Disposition::Fatal);
        assert_eq!(classify("Link Is Closed"), Disposition::Rebuild);
    }

    #[test]
    fn fatal_beats_retryable_when_a_message_matches_both() {
        // A size failure reported on a closed link is still a size failure:
        // rebuilding and retrying would not make the message smaller.
        let error = "MessageSizeExceeded while the link is closed";
        assert_eq!(classify(error), Disposition::Fatal);
    }
}
