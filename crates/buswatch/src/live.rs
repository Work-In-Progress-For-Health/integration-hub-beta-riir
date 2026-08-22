//! Reading a real Service Bus.
//!
//! Implements `spec/14-buswatch.md` §4.3, §4.4, §5.1 and §7.3.
//!
//! # Counts
//!
//! §4.3 says counts come from the management API. There is no Rust client
//! for that API in this workspace, and the Python reference implementation
//! does not use one either — its `get_queue_runtime` returns `None` for
//! every count in emulator mode. So counts here are the number of messages
//! **peeked**, marked as a lower bound when the peek filled its limit. That
//! is a real answer to "is anything on this queue", which absent counts are
//! not.
//!
//! # Sessions
//!
//! §4.4: a session-enabled queue cannot be read with a plain receiver. Which
//! queues those are is discovered from the emulator configuration, and a
//! plain peek that fails for a session reason falls back to a session one
//! anyway — the configuration file is not present inside a container.
//!
//! What a session peek returns is **one session's** messages, not the
//! queue's, because the broker hands out one session lock at a time.

// Every call here reaches `azservicebus` through `ihb-bus`, whose futures
// are ~32 KB. `ihb_bus::live` records why that is allowed rather than boxed
// away -- and `ihb-e2e` records the one place it is not: a dozen of them in
// one stack frame overflows, which is why nothing here nests them.
#![allow(clippy::large_futures)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use ihb_bus::connection::Endpoint;
use ihb_bus::error::BusError;
use ihb_bus::live::{LiveBus, LiveReceiver};
use ihb_bus::message::{Outcome, Peeked};
use tokio::sync::Mutex;

use crate::reader::{MessageDetail, QueueReader};
use crate::snapshot::{MessageSummary, Snapshot};

/// How many messages one clear pass receives at a time.
const CLEAR_BATCH: u32 = 100;

/// How long a clear pass waits for a batch.
///
/// Not `ihb_bus::live::MAX_WAIT`, which is a minute. A developer is waiting
/// on the HTTP response (§3), and the last pass of a clear always finds an
/// empty queue -- so a minute is what BusWatch would charge for every
/// successful clear.
const CLEAR_WAIT: Duration = Duration::from_secs(2);

/// How long any single broker operation is allowed to take.
///
/// The same reasoning as [`CLEAR_WAIT`], one layer down: asking an idle
/// session queue for its next session is the normal case here, and
/// `azservicebus` would spend its default sixty seconds per attempt on it.
const TRY_TIMEOUT: Duration = Duration::from_secs(5);

/// How many sessions a clear drains before giving up.
///
/// A session-enabled queue holds one lock at a time, so clearing it means
/// accepting sessions until none is left. Bounded for the reason
/// [`CLEAR_PASSES`] is.
const CLEAR_SESSIONS: usize = 50;

/// How many passes a clear makes before giving up.
///
/// A bound rather than a `loop`: a queue being written to while it is being
/// cleared would otherwise never finish, and a page that never loads is a
/// worse answer than "cleared 5000, some may remain".
const CLEAR_PASSES: usize = 50;

/// Substrings that mean "this queue needs a session receiver".
///
/// Matched case-insensitively against the broker's own text. The Python
/// matches its client library's validation message instead
/// (`next_available_session`/`max_wait_time`), which is a Python-side string
/// and says nothing about a Rust client — so this looks at what the broker
/// says rather than at what a particular SDK says about it.
const SESSION_MARKERS: [&str; 3] = ["sessionful", "requires session", "sessionrequired"];

/// A reader backed by a real broker.
pub struct LiveReader {
    /// The connection. Behind an `Arc` so §7.3's shutdown can close it.
    bus: Arc<LiveBus>,
    /// The session-enabled queues of §4.4.
    session_queues: BTreeSet<String>,
    /// Serialises reads.
    ///
    /// The emulator has a hard connection quota (§6.1), and every receiver
    /// opened here is a link on the one connection. The parallelism bound
    /// lives in `web::refresh_all`; this is the guard on a single queue
    /// being read by two requests at once.
    lock: Mutex<()>,
}

impl std::fmt::Debug for LiveReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveReader")
            .field("session_queues", &self.session_queues)
            .finish_non_exhaustive()
    }
}

impl LiveReader {
    /// Connect.
    ///
    /// # Errors
    ///
    /// [`BusError::Connect`] when the namespace cannot be reached.
    pub async fn connect(
        endpoint: &Endpoint,
        session_queues: BTreeSet<String>,
    ) -> Result<Self, BusError> {
        Ok(Self {
            // §3 and §6.1 -- see `TRY_TIMEOUT`. A page is not a receive loop.
            bus: Arc::new(LiveBus::connect_within(endpoint, Some(TRY_TIMEOUT)).await?),
            session_queues,
            lock: Mutex::new(()),
        })
    }

    /// Close the connection (§7.3).
    ///
    /// # Errors
    ///
    /// Whatever the broker says on the way out, which is worth logging and
    /// not worth acting on: the process is stopping either way.
    pub async fn close(self) -> Result<(), BusError> {
        match Arc::try_unwrap(self.bus) {
            Ok(bus) => bus.close().await,
            // Something still holds it, so it will close on its own drop.
            Err(_) => Ok(()),
        }
    }

    /// A receiver able to read this queue (§4.4).
    ///
    /// Session queues get a next-available-session receiver; everything else
    /// gets a plain one, with a fallback if the broker says otherwise.
    async fn receiver_for(&self, queue: &str) -> Result<LiveReceiver, BusError> {
        if self.session_queues.contains(queue) {
            return self.bus.next_session_receiver(queue).await;
        }

        match self.bus.queue_receiver(queue).await {
            Ok(receiver) => Ok(receiver),
            Err(error) if needs_a_session(&error.to_string()) => {
                // The configuration was absent or stale. Not a warning worth
                // raising: the answer is the same either way.
                self.bus.next_session_receiver(queue).await
            }
            Err(error) => Err(error),
        }
    }

    /// Peek `limit` messages, with the session handling of §4.4.
    async fn peek(&self, queue: &str, limit: u32) -> Result<Vec<Peeked>, BusError> {
        let receiver = self.receiver_for(queue).await?;
        let peeked = receiver.peek(limit, None).await;
        // Close before returning the result, so a failed peek does not leak
        // a link against §6.1's quota.
        let closed = receiver.close().await;

        match (peeked, closed) {
            (Ok(messages), _) => Ok(messages),
            (Err(error), _) => Err(error),
        }
    }
}

impl QueueReader for LiveReader {
    async fn snapshot(&self, queue: &str, peek: usize) -> Snapshot {
        let _guard = self.lock.lock().await;
        let taken_at = now();

        let limit = u32::try_from(peek).unwrap_or(u32::MAX);

        match self.peek(queue, limit).await {
            Ok(messages) => Snapshot {
                queue: queue.to_string(),
                // See this module's documentation: the count is what was
                // peeked, which is a lower bound when the peek filled up.
                active_count: Some(messages.len().try_into().unwrap_or(i64::MAX)),
                dead_letter_count: None,
                messages: messages.iter().map(summarise).collect(),
                taken_at,
                error: None,
            },
            // §6.2 — this queue carries its error and the page renders.
            // §6.1's hint is attached by `Snapshot::failed`.
            Err(error) => Snapshot::failed(queue, taken_at, &error.to_string()),
        }
    }

    async fn detail(&self, queue: &str, sequence: i64, limit: usize) -> Option<MessageDetail> {
        let _guard = self.lock.lock().await;

        // §5.1 — a bounded scan from the start of the queue. The broker
        // offers no "get by sequence number", so this is the only way, and
        // §5.2 turns "not within the limit" into a 404 that says so rather
        // than into "the message is gone".
        let peeked = self
            .peek(queue, u32::try_from(limit).unwrap_or(u32::MAX))
            .await
            .ok()?;

        let found = peeked
            .into_iter()
            .find(|message| message.sequence == sequence)?;

        Some(detail_of(queue, &found))
    }

    async fn clear(&self, queue: &str) -> Result<usize, String> {
        let _guard = self.lock.lock().await;

        if self.session_queues.contains(queue) {
            return self.clear_sessions(queue).await;
        }

        let receiver = self
            .receiver_for(queue)
            .await
            .map_err(|error| error.to_string())?;

        let cleared = drain(&receiver).await;
        // Best effort: the messages are already settled, so a failure to
        // close cleanly does not change what the developer is told.
        drop(receiver.close().await);

        cleared
    }
}

impl LiveReader {
    /// Clear a session-enabled queue (§4.4).
    ///
    /// The broker hands out one session lock at a time, so one receiver
    /// clears one session. Clearing the queue means accepting sessions
    /// until there is no next one -- at which point the acceptance fails,
    /// and that failure is the **success** condition rather than an error.
    async fn clear_sessions(&self, queue: &str) -> Result<usize, String> {
        let mut cleared = 0;

        for _ in 0..CLEAR_SESSIONS {
            let Ok(receiver) = self.bus.next_session_receiver(queue).await else {
                // No session left, which is what an empty queue looks like.
                break;
            };

            let drained = drain(&receiver).await;
            drop(receiver.close().await);

            match drained {
                Ok(0) => break,
                Ok(count) => cleared += count,
                // A real failure, after some sessions may already have been
                // cleared. Saying how many went is more use than saying none
                // did, but the failure is what the developer needs.
                Err(reason) => return Err(format!("{reason} (after clearing {cleared})")),
            }
        }

        Ok(cleared)
    }
}

/// Receive and complete everything a receiver can reach.
async fn drain(receiver: &LiveReceiver) -> Result<usize, String> {
    let mut cleared = 0;

    for _ in 0..CLEAR_PASSES {
        let batch = receiver
            .receive_within(CLEAR_BATCH, CLEAR_WAIT)
            .await
            .map_err(|error| error.to_string())?;

        if batch.is_empty() {
            break;
        }

        for message in &batch {
            // Accepted, so the message is completed rather than abandoned
            // back onto the queue.
            receiver
                .settle(message, Outcome::Accepted)
                .await
                .map_err(|error| error.to_string())?;
            cleared += 1;
        }
    }

    Ok(cleared)
}

/// Whether an error means the queue needs a session receiver.
fn needs_a_session(reason: &str) -> bool {
    let lowered = reason.to_ascii_lowercase();
    SESSION_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

/// A peeked message as §4.2's summary.
fn summarise(message: &Peeked) -> MessageSummary {
    MessageSummary {
        sequence: message.sequence,
        enqueued_at: message.enqueued_at.clone(),
        message_id: message.message_id.clone(),
        session_id: message.session_id.clone(),
        size: message.body.len(),
        preview: MessageSummary::preview_of(&text_of(message)),
    }
}

/// A peeked message as §5.3's detail.
fn detail_of(queue: &str, message: &Peeked) -> MessageDetail {
    // Sorted, so two renders of the same message read the same way and a
    // developer comparing two messages is comparing the values rather than
    // hunting for the row.
    let mut properties: Vec<(String, String)> = message
        .properties
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    properties.sort_by(|left, right| left.0.cmp(&right.0));

    let mut broker_properties = vec![
        ("SequenceNumber".to_string(), message.sequence.to_string()),
        ("EnqueuedTimeUtc".to_string(), message.enqueued_at.clone()),
        ("Size".to_string(), format!("{} bytes", message.body.len())),
    ];
    for (name, value) in [
        ("MessageId", message.message_id.as_deref()),
        ("SessionId", message.session_id.as_deref()),
        ("CorrelationId", message.correlation_id.as_deref()),
        ("ContentType", message.content_type.as_deref()),
        ("Subject", message.subject.as_deref()),
    ] {
        if let Some(value) = value {
            broker_properties.push((name.to_string(), value.to_string()));
        }
    }
    if let Some(count) = message.delivery_count {
        broker_properties.push(("DeliveryCount".to_string(), count.to_string()));
    }

    MessageDetail {
        queue: queue.to_string(),
        sequence: message.sequence,
        body: text_of(message),
        properties,
        broker_properties,
    }
}

/// A body as text, or a note saying it is not.
///
/// §5.4 will show whatever this returns raw, which is the right answer for
/// a body that is not UTF-8: BusWatch is a developer tool, and "not text"
/// is information.
fn text_of(message: &Peeked) -> String {
    message
        .text()
        .unwrap_or_else(|| format!("[{} bytes, not valid UTF-8]", message.body.len()))
}

/// Now, in RFC 3339.
fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_error_is_recognised_whatever_its_case() {
        // §4.4's fallback. The configuration file is not present inside a
        // container, so this is the only signal there.
        for reason in [
            "The queue is Sessionful",
            "InvalidOperation: requires session",
            "amqp:precondition-failed SessionRequired",
        ] {
            assert!(needs_a_session(reason), "{reason}");
        }
    }

    #[test]
    fn an_ordinary_error_is_not_a_session_error() {
        // A false positive here costs a second failed connection against
        // §6.1's quota, on a queue that was never going to work.
        for reason in [
            "connection refused",
            "connectionsquotaexceeded",
            "The queue does not exist",
        ] {
            assert!(!needs_a_session(reason), "{reason}");
        }
    }

    /// A peeked message, for the shaping tests.
    fn peeked(body: &[u8]) -> Peeked {
        Peeked {
            body: body.to_vec(),
            sequence: 42,
            enqueued_at: "2026-08-21T09:15:00Z".to_string(),
            message_id: Some("m-1".to_string()),
            session_id: Some("phw-to-mpi".to_string()),
            properties: ihb_bus::metadata::Properties::from([(
                "CorrelationId".to_string(),
                "3f6d".to_string(),
            )]),
            delivery_count: Some(2),
            correlation_id: None,
            content_type: Some("text/plain".to_string()),
            subject: None,
        }
    }

    #[test]
    fn a_body_that_is_not_text_says_so_rather_than_vanishing() {
        // §5.4 shows this raw, and "not text" is information a developer
        // debugging an encoding problem needs.
        let rendered = text_of(&peeked(&[0xff, 0xfe, 0x00]));

        assert_eq!(rendered, "[3 bytes, not valid UTF-8]");
    }

    #[test]
    fn the_detail_carries_the_broker_properties_of_section_five_three() {
        let detail = detail_of("inbound", &peeked(b"MSH|^~\\&|252"));

        let names: Vec<&str> = detail
            .broker_properties
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();

        assert!(names.contains(&"SequenceNumber"));
        assert!(names.contains(&"EnqueuedTimeUtc"));
        assert!(names.contains(&"SessionId"));
        assert!(names.contains(&"DeliveryCount"));
        assert!(
            !names.contains(&"Subject"),
            "an absent property is absent, not empty"
        );

        // §5.3's application properties, separately from the broker's.
        assert_eq!(
            detail.properties,
            vec![("CorrelationId".to_string(), "3f6d".to_string())]
        );
    }

    #[test]
    fn a_summary_previews_the_body() {
        let summary = summarise(&peeked(b"MSH|^~\\&|252|252|100"));

        assert_eq!(summary.sequence, 42);
        assert_eq!(summary.size, 20);
        assert!(summary.preview.starts_with("MSH|"), "{}", summary.preview);
    }
}
