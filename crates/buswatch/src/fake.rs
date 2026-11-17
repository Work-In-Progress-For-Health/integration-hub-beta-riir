//! A reader that answers from a table.
//!
//! For `spec/14-buswatch.md` §8.4: "every route renders with a fake reader,
//! including the empty-queue and unreachable-queue cases".
//!
//! # Why a fake rather than the emulator
//!
//! The empty case is arrangeable on a real emulator, with effort. The
//! **unreachable** case is not: §6.2 says a queue that cannot be read shows
//! its error while the rest of the page renders, and there is no way to ask
//! a running emulator to fail one queue and serve the others. A fake is the
//! only way to test the clause that matters most.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::reader::{MessageDetail, QueueReader};
use crate::snapshot::{MessageSummary, Snapshot};

/// A queue the fake knows about.
#[derive(Debug, Clone, Default)]
struct Queue {
    /// The messages on it, in order.
    messages: Vec<MessageDetail>,
    /// A reason every read of it fails, for §6.2.
    fails_with: Option<String>,
}

/// A [`QueueReader`] backed by a table.
#[derive(Debug, Default)]
pub struct FakeReader {
    /// The queues, by name.
    queues: Mutex<HashMap<String, Queue>>,
    /// What `taken_at` says, so a rendered page is byte-stable.
    taken_at: String,
}

impl FakeReader {
    /// An empty reader: every queue reads as empty.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queues: Mutex::new(HashMap::new()),
            taken_at: "2026-08-21T09:15:00Z".to_string(),
        }
    }

    /// Put a message on a queue.
    ///
    /// # Examples
    ///
    /// ```
    /// use buswatch::fake::FakeReader;
    ///
    /// let reader = FakeReader::new();
    /// reader.push("inbound", 7, "MSH|^~\\&|252");
    /// ```
    pub fn push(&self, queue: &str, sequence: i64, body: &str) {
        self.push_with(queue, sequence, body, &[]);
    }

    /// Put a message on a queue, with application properties.
    pub fn push_with(&self, queue: &str, sequence: i64, body: &str, properties: &[(&str, &str)]) {
        let detail = MessageDetail {
            queue: queue.to_string(),
            sequence,
            body: body.to_string(),
            properties: properties
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            broker_properties: vec![
                ("SequenceNumber".to_string(), sequence.to_string()),
                ("EnqueuedTimeUtc".to_string(), self.taken_at.clone()),
            ],
        };

        self.entry(queue, |q| q.messages.push(detail.clone()));
    }

    /// Make every read of a queue fail, for §6.2.
    ///
    /// # Examples
    ///
    /// ```
    /// use buswatch::fake::FakeReader;
    ///
    /// let reader = FakeReader::new();
    /// reader.fail("inbound", "connectionsquotaexceeded");
    /// ```
    pub fn fail(&self, queue: &str, reason: &str) {
        let reason = reason.to_string();
        self.entry(queue, |q| q.fails_with = Some(reason.clone()));
    }

    /// How many messages a queue holds now.
    #[must_use]
    pub fn depth(&self, queue: &str) -> usize {
        self.queues
            .lock()
            .map_or_default(|queues| queues.get(queue).map_or(0, |q| q.messages.len()))
    }

    /// Mutate one queue's entry, creating it if absent.
    fn entry(&self, queue: &str, change: impl FnOnce(&mut Queue)) {
        if let Ok(mut queues) = self.queues.lock() {
            change(queues.entry(queue.to_string()).or_default());
        }
    }
}

// Every method here answers from a table, so none of them awaits. The
// trait's methods are `async` because the live reader's are, and
// `std::future::ready` in their place would trade three readable functions
// for three that are not.
#[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
impl QueueReader for FakeReader {
    async fn snapshot(&self, queue: &str, peek: usize) -> Snapshot {
        let Ok(queues) = self.queues.lock() else {
            return Snapshot::failed(queue, &self.taken_at, "the fake is poisoned");
        };

        let Some(found) = queues.get(queue) else {
            // A queue the fake has never heard of is empty, not missing:
            // that is §8.4's empty case, and it is what a configured but
            // idle queue looks like.
            return Snapshot {
                queue: queue.to_string(),
                active_count: Some(0),
                dead_letter_count: None,
                messages: Vec::new(),
                taken_at: self.taken_at.clone(),
                error: None,
            };
        };

        if let Some(reason) = &found.fails_with {
            // §6.2 — this queue carries its error; the page renders.
            return Snapshot::failed(queue, &self.taken_at, reason);
        }

        Snapshot {
            queue: queue.to_string(),
            active_count: Some(found.messages.len().try_into().unwrap_or(i64::MAX)),
            dead_letter_count: None,
            messages: found
                .messages
                .iter()
                .take(peek)
                .map(|message| MessageSummary {
                    sequence: message.sequence,
                    enqueued_at: self.taken_at.clone(),
                    message_id: None,
                    session_id: None,
                    size: message.body.len(),
                    preview: MessageSummary::preview_of(&message.body),
                })
                .collect(),
            taken_at: self.taken_at.clone(),
            error: None,
        }
    }

    async fn detail(&self, queue: &str, sequence: i64, limit: usize) -> Option<MessageDetail> {
        let queues = self.queues.lock().ok()?;
        let found = queues.get(queue)?;

        if found.fails_with.is_some() {
            return None;
        }

        // §5.1 — a bounded scan from the start, so the fake reproduces the
        // "past the limit" case §5.2 turns into a 404.
        let message = found
            .messages
            .iter()
            .take(limit)
            .find(|message| message.sequence == sequence)
            .cloned();
        drop(queues);

        message
    }

    async fn clear(&self, queue: &str) -> Result<usize, String> {
        let mut queues = self.queues.lock().map_err(|_| "poisoned".to_string())?;
        let Some(found) = queues.get_mut(queue) else {
            return Ok(0);
        };

        if let Some(reason) = &found.fails_with {
            return Err(reason.clone());
        }

        let count = found.messages.len();
        found.messages.clear();
        drop(queues);

        Ok(count)
    }
}
