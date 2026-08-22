//! What a queue can fail with, and the one failure worth explaining.
//!
//! Implements `spec/14-buswatch.md` §6.

/// The two substrings that mean the emulator has run out of connections.
///
/// §6.1 names exactly these, because the Python detects exactly these. The
/// emulator's quota is low and easy to reach — every service in the local
/// stack holds connections — and the resulting error is opaque.
pub const QUOTA_MARKERS: [&str; 2] = ["connectionsquotaexceeded", "resource-limit-exceeded"];

/// The hint shown when the quota is hit.
pub const QUOTA_HINT: &str = "The Service Bus emulator has run out of connections. \
     Stop some services in the local stack, or restart the emulator with \
     `podman restart ihb-rs-sb-emulator`, then refresh.";

/// One queue's failure.
///
/// §6.2: a failure to reach one queue MUST NOT fail the page. That queue
/// shows this and the rest render — which matters because the commonest
/// reason a queue is unreachable is the quota below, and losing the whole
/// page would hide the nine queues that are fine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueError {
    /// What went wrong.
    pub reason: String,
    /// An actionable hint, when there is one (§6.1).
    pub hint: Option<&'static str>,
}

impl QueueError {
    /// Classify a failure.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let hint = quota_hint(&reason);
        Self { reason, hint }
    }
}

/// The hint for an error, if it is the emulator's connection quota.
///
/// §6.1 — "the difference between a five-minute and a fifty-minute
/// debugging session". The error the emulator returns names a quota but not
/// what to do about it, and the thing to do is not obvious.
///
/// # Examples
///
/// ```
/// use buswatch::error::quota_hint;
///
/// assert!(quota_hint("ConnectionsQuotaExceeded: too many").is_some());
/// assert!(quota_hint("amqp:resource-limit-exceeded").is_some());
///
/// assert!(quota_hint("connection refused").is_none());
/// ```
#[must_use]
pub fn quota_hint(reason: &str) -> Option<&'static str> {
    let lowered = reason.to_ascii_lowercase();
    QUOTA_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
        .then_some(QUOTA_HINT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_markers_are_recognised_case_insensitively() {
        // spec/14-buswatch.md §6.1 and §8.2 — exactly these two, because
        // exactly these two are what the Python detects.
        assert!(quota_hint("ConnectionsQuotaExceeded").is_some());
        assert!(quota_hint("connectionsquotaexceeded").is_some());
        assert!(quota_hint("Error: amqp:resource-limit-exceeded on link").is_some());
        assert!(quota_hint("RESOURCE-LIMIT-EXCEEDED").is_some());
    }

    #[test]
    fn an_unrelated_error_gets_no_hint() {
        // §8.2's negative case. A hint attached to every error is a hint
        // nobody reads.
        for reason in [
            "connection refused",
            "the queue does not exist",
            "timed out",
            "",
        ] {
            assert!(quota_hint(reason).is_none(), "{reason:?}");
        }
    }

    #[test]
    fn a_queue_error_carries_its_hint() {
        let quota = QueueError::new("ConnectionsQuotaExceeded: 100 of 100");
        assert_eq!(quota.hint, Some(QUOTA_HINT));
        assert!(quota.reason.contains("100 of 100"), "the original is kept");

        let other = QueueError::new("connection refused");
        assert_eq!(other.hint, None);
    }
}
