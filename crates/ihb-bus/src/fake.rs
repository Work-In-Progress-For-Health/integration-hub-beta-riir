//! A bus that lives in memory.
//!
//! Implements `spec/03-message-bus.md` §7.1.
//!
//! Every service in the estate is a receive loop around a Service Bus
//! client, so almost every interesting test needs a bus. Requiring a broker
//! for those tests would mean the suite only runs where one is available,
//! which in practice means it stops being run.
//!
//! This is the alternative: the same traits, backed by a `Vec`. It is
//! deliberately not a simulator — it does not model lock expiry, prefetch or
//! partitioning. What it does model is the part services get wrong:
//! **dispositions**. A message completed is gone; a message abandoned comes
//! back with its delivery count raised; and [`FakeBus::in_flight`] is not
//! empty if a handler forgot to settle one.

// Every operation here holds the lock for its whole body on purpose: a
// send, a receive and a settle each have to be atomic against the others, or
// two threads sharing a bus would observe a half-moved message. Clippy's
// `significant_drop_tightening` would have the guard released earlier, which
// is exactly what must not happen.
#![allow(clippy::significant_drop_tightening)]

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

use crate::error::BusError;
use crate::message::{Incoming, Outcome, Outgoing, Token};
use crate::metadata::Properties;
use crate::sink::MessageSink;

/// One queue or topic inside a [`FakeBus`].
#[derive(Debug, Default)]
struct Entity {
    /// Messages waiting to be received, in order.
    pending: VecDeque<Incoming>,
    /// Messages received but not yet settled, by token.
    in_flight: Vec<Incoming>,
    /// Messages that were completed, kept so a test can assert on them.
    completed: Vec<Incoming>,
    /// Messages that were dead-lettered.
    dead_lettered: Vec<Incoming>,
}

/// The shared state of a fake bus.
#[derive(Debug, Default)]
struct State {
    /// Entities by name. A subscription is `"topic/subscription"`.
    entities: std::collections::BTreeMap<String, Entity>,
    /// The next token to hand out.
    next_token: u64,
    /// A failure to inject on the next send, if any.
    fail_next_send: Option<BusError>,
}

/// An in-memory Service Bus, for tests.
///
/// Cheap to clone; every clone shares one bus, which is what lets a test
/// hold a sender and a receiver over the same queue.
///
/// # Examples
///
/// ```
/// use ihb_bus::fake::FakeBus;
/// use ihb_bus::message::{Outcome, Outgoing};
///
/// let bus = FakeBus::new();
/// bus.send("pre-phw-transform", Outgoing::new("MSH|^~\\&|252"))?;
///
/// let received = bus.receive("pre-phw-transform", 10);
/// assert_eq!(received.len(), 1);
///
/// bus.settle(&received[0], Outcome::Accepted)?;
/// assert!(bus.in_flight("pre-phw-transform").is_empty());
/// # Ok::<(), ihb_bus::BusError>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct FakeBus {
    /// Shared state.
    state: Arc<Mutex<State>>,
}

impl FakeBus {
    /// An empty bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Send a message to a queue or topic.
    ///
    /// # Errors
    ///
    /// Whatever was armed by [`FakeBus::fail_next_send`], so a test can
    /// exercise a service's failure path — which for ingress means checking
    /// that **no acknowledgement is returned**
    /// (`spec/07-hl7-server.md` §5.3).
    pub fn send(&self, entity: &str, message: Outgoing) -> Result<(), BusError> {
        let mut state = self.lock();

        if let Some(error) = state.fail_next_send.take() {
            return Err(error);
        }

        // A token that wrapped or held would name two messages; at 2^64 sends
        // that is refused, never reused.
        let Some(next_token) = state.next_token.checked_add(1) else {
            return Err(BusError::Send {
                destination: entity.to_string(),
                attempts: 1,
                reason: "the token counter is exhausted".to_string(),
                disposition: crate::retry::Disposition::Fatal,
            });
        };
        state.next_token = next_token;
        let token = Token(next_token);

        let incoming = Incoming {
            body: message.body,
            message_id: message.message_id,
            session_id: message.session_id,
            properties: message.properties,
            delivery_count: Some(1),
            token,
        };

        state
            .entities
            .entry(entity.to_string())
            .or_default()
            .pending
            .push_back(incoming);

        Ok(())
    }

    /// A [`MessageSink`] onto one entity.
    ///
    /// The live bus hands out a sender per entity
    /// ([`LiveBus::sender`](crate::live::LiveBus::sender)); this is the same
    /// shape, so a service under test is wired up exactly as it is in
    /// production rather than through a test-only side door.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_bus::fake::FakeBus;
    /// use ihb_bus::message::Outgoing;
    /// use ihb_bus::sink::MessageSink;
    ///
    /// # tokio_test::block_on(async {
    /// let bus = FakeBus::new();
    /// let sink = bus.sender("mpi-sender-ingress");
    ///
    /// sink.send(Outgoing::new("MSH|^~\\&|252").with_message_id("M1"))
    ///     .await
    ///     .unwrap();
    ///
    /// assert_eq!(bus.all_sent("mpi-sender-ingress").len(), 1);
    /// # });
    /// ```
    #[must_use]
    pub fn sender(&self, entity: &str) -> FakeSender {
        FakeSender {
            bus: self.clone(),
            entity: entity.to_string(),
        }
    }

    /// Receive up to `limit` messages, moving them in flight.
    ///
    /// Peek-lock, like the real thing: a message returned here is not gone
    /// until it is settled.
    #[must_use]
    pub fn receive(&self, entity: &str, limit: usize) -> Vec<Incoming> {
        let mut state = self.lock();
        let Some(entity) = state.entities.get_mut(entity) else {
            return Vec::new();
        };

        let mut received = Vec::new();
        for _ in 0..limit {
            let Some(message) = entity.pending.pop_front() else {
                break;
            };
            entity.in_flight.push(message.clone());
            received.push(message);
        }
        received
    }

    /// Settle a received message.
    ///
    /// [`Outcome::Accepted`] and [`Outcome::Rejected`] complete it;
    /// [`Outcome::Deferred`] puts it back at the **front** of the queue with
    /// its delivery count raised, because ordering within a session is the
    /// property the whole estate depends on
    /// (`spec/01-architecture.md` §6.2).
    ///
    /// # Errors
    ///
    /// [`BusError::Disposition`] if the message is not in flight — which
    /// means a test settled the same message twice, or forged a token.
    pub fn settle(&self, message: &Incoming, outcome: Outcome) -> Result<(), BusError> {
        let mut state = self.lock();

        let Some((name, entity)) = state
            .entities
            .iter_mut()
            .find(|(_, entity)| entity.in_flight.iter().any(|m| m.token == message.token))
        else {
            return Err(BusError::Disposition {
                action: "settle",
                entity: "<unknown>".to_string(),
                reason: "the message is not in flight".to_string(),
            });
        };
        let name = name.clone();

        let Some(index) = entity
            .in_flight
            .iter()
            .position(|m| m.token == message.token)
        else {
            return Err(BusError::Disposition {
                action: "settle",
                entity: name,
                reason: "the message is not in flight".to_string(),
            });
        };
        let mut held = entity.in_flight.remove(index);

        match outcome {
            Outcome::Accepted | Outcome::Rejected => entity.completed.push(held),
            Outcome::Deferred => {
                // A count that cannot go higher stays where it is: it only
                // reports how often the message has been delivered.
                let delivered = held.delivery_count.unwrap_or(1);
                held.delivery_count = Some(delivered.checked_add(1).unwrap_or(delivered));
                // Front, not back: a session preserves order, and a
                // redelivered message is still the next one.
                entity.pending.push_front(held);
            }
        }

        Ok(())
    }

    /// Dead-letter a received message.
    ///
    /// # Errors
    ///
    /// As [`FakeBus::settle`].
    pub fn dead_letter(&self, message: &Incoming) -> Result<(), BusError> {
        let mut state = self.lock();

        for entity in state.entities.values_mut() {
            if let Some(index) = entity
                .in_flight
                .iter()
                .position(|m| m.token == message.token)
            {
                let held = entity.in_flight.remove(index);
                entity.dead_lettered.push(held);
                return Ok(());
            }
        }

        Err(BusError::Disposition {
            action: "dead-letter",
            entity: "<unknown>".to_string(),
            reason: "the message is not in flight".to_string(),
        })
    }

    /// Messages waiting to be received.
    #[must_use]
    pub fn pending(&self, entity: &str) -> Vec<Incoming> {
        self.snapshot(entity, |e| e.pending.iter().cloned().collect())
    }

    /// Messages received but not settled.
    ///
    /// **Not empty is a defect in the code under test.** A handler that
    /// returns without settling has left a message locked, which in
    /// production means it is redelivered after the lock expires and the
    /// work is done twice.
    #[must_use]
    pub fn in_flight(&self, entity: &str) -> Vec<Incoming> {
        self.snapshot(entity, |e| e.in_flight.clone())
    }

    /// Messages that were completed.
    #[must_use]
    pub fn completed(&self, entity: &str) -> Vec<Incoming> {
        self.snapshot(entity, |e| e.completed.clone())
    }

    /// Messages that were dead-lettered.
    #[must_use]
    pub fn dead_lettered(&self, entity: &str) -> Vec<Incoming> {
        self.snapshot(entity, |e| e.dead_lettered.clone())
    }

    /// Everything that has ever been sent to an entity, settled or not.
    ///
    /// What a test usually wants: "did the service forward the message?"
    #[must_use]
    pub fn all_sent(&self, entity: &str) -> Vec<Incoming> {
        self.snapshot(entity, |e| {
            e.pending
                .iter()
                .cloned()
                .chain(e.in_flight.iter().cloned())
                .chain(e.completed.iter().cloned())
                .chain(e.dead_lettered.iter().cloned())
                .collect()
        })
    }

    /// The properties on the single message sent to an entity.
    ///
    /// For the common assertion "the correlation id was forwarded
    /// unchanged".
    ///
    /// # Panics
    ///
    /// Panics if the entity did not receive exactly one message, because a
    /// test asserting on "the" message when there are three is asserting on
    /// the wrong thing.
    #[must_use]
    pub fn only_properties(&self, entity: &str) -> Properties {
        let sent = self.all_sent(entity);
        assert_eq!(
            sent.len(),
            1,
            "expected exactly one message on {entity}, found {}",
            sent.len()
        );
        // The assertion above is what makes this the only one; `first` keeps
        // the read free of indexing (RCA-LEN-2).
        sent.first()
            .map_or_else(Properties::default, |only| only.properties.clone())
    }

    /// Make the next send fail.
    ///
    /// The failure path is the one that matters at ingress: a service that
    /// cannot forward a message must not acknowledge it
    /// (`spec/07-hl7-server.md` §5.3).
    pub fn fail_next_send(&self, error: BusError) {
        self.lock().fail_next_send = Some(error);
    }

    /// Read something out of an entity, or a default when it has none.
    fn snapshot<T: Default>(&self, entity: &str, read: impl FnOnce(&Entity) -> T) -> T {
        self.lock()
            .entities
            .get(entity)
            .map_or_else(T::default, read)
    }

    /// The shared state.
    ///
    /// A poisoned lock means a test panicked mid-assertion; recovering the
    /// state is more useful than a second panic on top of the first.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sent_message_can_be_received() {
        let bus = FakeBus::new();
        bus.send("q", Outgoing::new("MSH|^~\\&|252")).unwrap();

        let received = bus.receive("q", 10);
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].text().as_deref(), Some("MSH|^~\\&|252"));
    }

    #[test]
    fn receiving_from_an_unknown_entity_is_empty_rather_than_an_error() {
        assert_eq!(FakeBus::new().receive("nothing", 10), Vec::new());
    }

    #[test]
    fn a_received_message_is_in_flight_until_settled() {
        // Peek-lock: the property every service depends on.
        let bus = FakeBus::new();
        bus.send("q", Outgoing::new("x")).unwrap();

        let received = bus.receive("q", 10);
        assert_eq!(bus.in_flight("q").len(), 1);
        assert_eq!(bus.pending("q"), Vec::new());

        bus.settle(&received[0], Outcome::Accepted).unwrap();
        assert_eq!(bus.in_flight("q"), Vec::new());
        assert_eq!(bus.completed("q").len(), 1);
    }

    #[test]
    fn abandoning_returns_the_message_with_a_raised_delivery_count() {
        // spec/10-senders.md §2.2 depends on this: a redelivered message
        // must not be stored twice.
        let bus = FakeBus::new();
        bus.send("q", Outgoing::new("x")).unwrap();

        let first = bus.receive("q", 10);
        assert!(first[0].is_first_delivery());
        bus.settle(&first[0], Outcome::Deferred).unwrap();

        let second = bus.receive("q", 10);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].delivery_count, Some(2));
        assert!(!second[0].is_first_delivery());
    }

    #[test]
    fn an_abandoned_message_goes_back_to_the_front() {
        // spec/01-architecture.md §6.2 — a session preserves order, so a
        // redelivered message is still the next one.
        let bus = FakeBus::new();
        bus.send("q", Outgoing::new("first")).unwrap();
        bus.send("q", Outgoing::new("second")).unwrap();

        let received = bus.receive("q", 1);
        bus.settle(&received[0], Outcome::Deferred).unwrap();

        let next = bus.receive("q", 1);
        assert_eq!(next[0].text().as_deref(), Some("first"));
    }

    #[test]
    fn a_rejected_message_is_completed_rather_than_returned() {
        // spec/index.md §0.9 — a message that can never succeed must not be
        // abandoned, or it loops until MaxDeliveryCount.
        let bus = FakeBus::new();
        bus.send("q", Outgoing::new("poison")).unwrap();

        let received = bus.receive("q", 10);
        bus.settle(&received[0], Outcome::Rejected).unwrap();

        assert_eq!(bus.pending("q"), Vec::new());
        assert_eq!(bus.completed("q").len(), 1);
    }

    #[test]
    fn settling_the_same_message_twice_is_an_error() {
        // Which is how a test finds a handler that settles on two paths.
        let bus = FakeBus::new();
        bus.send("q", Outgoing::new("x")).unwrap();

        let received = bus.receive("q", 10);
        bus.settle(&received[0], Outcome::Accepted).unwrap();
        assert!(bus.settle(&received[0], Outcome::Accepted).is_err());
    }

    #[test]
    fn a_message_can_be_dead_lettered() {
        let bus = FakeBus::new();
        bus.send("q", Outgoing::new("x")).unwrap();

        let received = bus.receive("q", 10);
        bus.dead_letter(&received[0]).unwrap();

        assert_eq!(bus.dead_lettered("q").len(), 1);
        assert_eq!(bus.in_flight("q"), Vec::new());
    }

    #[test]
    fn an_armed_failure_fires_once() {
        // spec/07-hl7-server.md §5.3 — the failure path a test must be able
        // to reach.
        let bus = FakeBus::new();
        bus.fail_next_send(BusError::MessageTooLarge {
            destination: "q".into(),
        });

        assert!(bus.send("q", Outgoing::new("x")).is_err());
        assert!(bus.send("q", Outgoing::new("x")).is_ok());
    }

    #[test]
    fn properties_survive_the_round_trip() {
        // The correlation id crossing a hop unchanged is the property the
        // whole audit trail rests on.
        let mut properties = Properties::new();
        properties.insert("CorrelationId".into(), "3f6d".into());

        let bus = FakeBus::new();
        bus.send("q", Outgoing::new("x").with_properties(properties))
            .unwrap();

        assert_eq!(bus.only_properties("q")["CorrelationId"], "3f6d");
    }

    #[test]
    fn receiving_is_bounded_by_the_limit() {
        let bus = FakeBus::new();
        for i in 0..10 {
            bus.send("q", Outgoing::new(format!("{i}"))).unwrap();
        }
        assert_eq!(bus.receive("q", 3).len(), 3);
        assert_eq!(bus.pending("q").len(), 7);
    }

    #[test]
    fn a_clone_shares_one_bus() {
        // So a test can hold a sender and a receiver over the same queue.
        let bus = FakeBus::new();
        let other = bus.clone();

        bus.send("q", Outgoing::new("x")).unwrap();
        assert_eq!(other.receive("q", 10).len(), 1);
    }

    #[test]
    fn a_subscription_is_just_an_entity_name() {
        // spec/03-message-bus.md §2.2 — "topic/subscription" everywhere.
        let bus = FakeBus::new();
        bus.send(
            "mpihl7input/mpihl7input-test-subscription",
            Outgoing::new("x"),
        )
        .unwrap();
        assert_eq!(
            bus.receive("mpihl7input/mpihl7input-test-subscription", 10)
                .len(),
            1
        );
    }
}

/// A [`MessageSink`] onto one entity of a [`FakeBus`].
///
/// Made by [`FakeBus::sender`]. Cloning it, or making a second one for the
/// same entity, shares the one bus — as two senders on the same queue do.
#[derive(Debug, Clone)]
pub struct FakeSender {
    /// The bus this writes to.
    bus: FakeBus,
    /// The queue or topic.
    entity: String,
}

impl MessageSink for FakeSender {
    /// Put the message on the bus, honouring any armed failure.
    ///
    /// No retry, deliberately: [`FakeBus::fail_next_send`] arms **one**
    /// failure, and a fake that retried would swallow it and quietly test
    /// the opposite of what the test asked for.
    ///
    /// Written as a ready future rather than an `async fn` because there is
    /// nothing here to await — the fake is a `Mutex` and a `Vec`. An `async`
    /// body with no suspension point would be a future that only ever
    /// completes on its first poll, which is what this says directly.
    fn send(&self, message: Outgoing) -> impl Future<Output = Result<(), BusError>> + Send {
        std::future::ready(self.bus.send(&self.entity, message))
    }

    fn entity(&self) -> &str {
        &self.entity
    }
}
