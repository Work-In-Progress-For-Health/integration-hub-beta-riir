//! The real Service Bus, over AMQP.
//!
//! Implements `spec/03-message-bus.md` §1, §2, §3 and §5.
//!
//! Everything here is a thin layer over `azservicebus` that adds the four
//! things this estate needs and the crate does not provide:
//!
//! 1. **TLS or plaintext, decided once** — see [`crate::connection`], and
//!    §8.1.1 for why that is not a detail.
//! 2. **Retry with classification** — a message that is too large will not
//!    become smaller, and retrying it blocks the session behind it (§3.2.2).
//! 3. **Session lock renewal**, which `azservicebus` has no equivalent of
//!    (§8.1.2).
//! 4. **The shapes the fake bus also implements**, so a service's tests need
//!    no broker.
//!
//! # The receiver is shared, on purpose
//!
//! A session receiver lives behind a [`tokio::sync::Mutex`]. The receive loop
//! locks it to receive and to settle, and lets go while a handler runs; the
//! renewal task locks it briefly every twenty seconds. That is what lets a
//! slow message be processed without losing the session under it — and it is
//! the reason renewal is an async closure rather than a borrowed receiver.
//!
//! # On `clippy::large_futures`
//!
//! Every `azservicebus` operation produces a future of roughly 32 KB —
//! twice the lint's 16 KB threshold — because the crate's AMQP state
//! machines are held inline. The size belongs to the dependency; there is no
//! version of this module that calls it and stays under the threshold.
//!
//! The lint is allowed for the module rather than silenced call by call,
//! and the trade is recorded here as `spec/index.md` §0.1 requires. Taking
//! the suggestion would mean `Box::pin` at fourteen call sites, adding a
//! heap allocation to every send, receive and settle. It would buy nothing:
//! these futures are awaited inside `tokio::spawn`ed tasks, whose futures
//! are already boxed once on the heap, so the 32 KB never sits on a stack
//! and never nests. Fourteen allocations per message to move bytes that are
//! already where the lint wants them is a worse program.
//!
//! If a future here is ever awaited outside a spawned task — in a deeply
//! nested combinator, say — that reasoning stops holding and this allow
//! should go.
//!
//! That caveat has since been exercised. `ihb-e2e`'s end-to-end test awaits
//! a dozen of these in one linear function body, and overflows a 2 MB
//! thread stack doing it. The fix there is a thread with a larger stack,
//! and it is worth knowing that the limit is real rather than theoretical:
//! roughly sixty of these futures nested in one frame is all it takes. No
//! service does that — each of its calls is one step of a loop — but a test
//! that writes a whole flow as one function does.

// See the module documentation above for why this is a considered trade
// rather than an inconvenience.
#![allow(clippy::large_futures)]

use std::sync::Arc;
use std::time::Duration;

use azservicebus::core::BasicRetryPolicy;
use azservicebus::{
    ServiceBusClient, ServiceBusClientOptions, ServiceBusMessage, ServiceBusPeekedMessage,
    ServiceBusReceivedMessage, ServiceBusReceiver, ServiceBusReceiverOptions, ServiceBusSender,
    ServiceBusSenderOptions, ServiceBusSessionReceiver, ServiceBusSessionReceiverOptions,
    ServiceBusTransportType,
};
use fe2o3_amqp_types::messaging::ApplicationProperties;
use tokio::sync::Mutex;

use crate::connection::Endpoint;
use crate::error::BusError;
use crate::message::{Incoming, Outcome, Outgoing, Peeked, Token};
use crate::metadata::Properties;
use crate::renew::{Ended, LockRenewer};
use crate::retry::{Disposition, MAX_SEND_ATTEMPTS, classify};
use crate::sink::MessageSink;

/// How long a receive waits for a message before returning empty.
///
/// From `message_receiver_client.py :: MAX_WAIT_TIME_SECONDS`
/// (`spec/03-message-bus.md` §5.1.2).
pub const MAX_WAIT: Duration = Duration::from_secs(60);

/// A connected Service Bus client.
///
/// One per process. Senders and receivers are created from it and are cheap
/// relative to the connection.
pub struct LiveBus {
    /// The underlying client.
    client: Mutex<ServiceBusClient<BasicRetryPolicy>>,
    /// What it connected to, for error messages.
    namespace: String,
}

impl std::fmt::Debug for LiveBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveBus")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl LiveBus {
    /// Connect.
    ///
    /// The endpoint decides whether this is TLS or plaintext, and
    /// [`Endpoint::is_plaintext`] is the only thing that can choose the
    /// latter (`spec/03-message-bus.md` §8.1.1).
    ///
    /// # Errors
    ///
    /// [`BusError::Connect`] when the namespace cannot be reached, and
    /// [`BusError::NotConfigured`] for a namespace-only endpoint, which needs
    /// an Azure credential this crate does not yet wire up.
    pub async fn connect(endpoint: &Endpoint) -> Result<Self, BusError> {
        Self::connect_within(endpoint, None).await
    }

    /// Connect, with a shorter per-attempt timeout.
    ///
    /// [`LiveBus::connect`] leaves `azservicebus` on its default 60 seconds
    /// per attempt, which is right for a service: a receive loop that waits
    /// a minute for a message that is not there has lost nothing.
    ///
    /// It is wrong for an interactive tool. `spec/14-buswatch.md` §3 has a
    /// developer waiting on an HTTP response, and an operation against an
    /// **empty** queue is the normal case there — clearing a queue that is
    /// already clear, or asking for the next session on an idle queue. A
    /// minute of that per queue is a page that never loads.
    ///
    /// Shortening the timeout rather than wrapping the call in
    /// [`tokio::time::timeout`] is deliberate: cancelling a session
    /// acceptance mid-flight can leave the session locked, and a developer
    /// tool must not be able to block the service it is watching.
    ///
    /// # Errors
    ///
    /// As [`LiveBus::connect`].
    pub async fn connect_within(
        endpoint: &Endpoint,
        try_timeout: Option<Duration>,
    ) -> Result<Self, BusError> {
        let namespace = endpoint.namespace();

        let mut options = ServiceBusClientOptions {
            transport_type: ServiceBusTransportType::AmqpTcp,
            ..Default::default()
        };
        if let Some(try_timeout) = try_timeout {
            options.retry_options.try_timeout = try_timeout;
        }

        let Some(connection_string) = endpoint.expose_connection_string() else {
            // A namespace on its own needs `DefaultAzureCredential`, which
            // arrives with the managed-identity work. Saying so is better
            // than a connection error three layers down.
            return Err(BusError::NotConfigured);
        };

        let client = if endpoint.is_plaintext() {
            // The local emulator only. See spec/03-message-bus.md §8.1.1.
            tracing::warn!(
                %namespace,
                "connecting over PLAINTEXT AMQP; this must only ever be the local emulator"
            );
            azservicebus::client::service_bus_client::WithCustomRetryPolicy::<BasicRetryPolicy>::unsecured()
                .new_from_connection_string(connection_string, options)
                .await
        } else {
            ServiceBusClient::new_from_connection_string(connection_string, options).await
        };

        let client = client.map_err(|error| BusError::Connect {
            namespace: namespace.clone(),
            reason: error.to_string(),
        })?;

        tracing::info!(%namespace, "connected to Service Bus");

        Ok(Self {
            client: Mutex::new(client),
            namespace,
        })
    }

    /// A sender for a queue or a topic.
    ///
    /// # Errors
    ///
    /// [`BusError::Connect`] when the link cannot be established.
    pub async fn sender(&self, entity: &str) -> Result<LiveSender, BusError> {
        let sender = self
            .client
            .lock()
            .await
            .create_sender(entity, ServiceBusSenderOptions::default())
            .await
            .map_err(|error| BusError::Connect {
                namespace: self.namespace.clone(),
                reason: error.to_string(),
            })?;

        Ok(LiveSender {
            sender: Mutex::new(sender),
            entity: entity.to_string(),
        })
    }

    /// A receiver for a queue, with no session.
    ///
    /// The message-store queue is the only one in the estate like this
    /// (`spec/01-architecture.md` §2.3.3).
    ///
    /// # Errors
    ///
    /// [`BusError::Connect`] when the link cannot be established.
    pub async fn queue_receiver(&self, queue: &str) -> Result<LiveReceiver, BusError> {
        let receiver = self
            .client
            .lock()
            .await
            .create_receiver_for_queue(queue, ServiceBusReceiverOptions::default())
            .await
            .map_err(|error| BusError::Connect {
                namespace: self.namespace.clone(),
                reason: error.to_string(),
            })?;

        Ok(LiveReceiver::new(Inner::Queue(receiver), queue.to_string()))
    }

    /// A receiver for one session of a queue.
    ///
    /// # Errors
    ///
    /// [`BusError::Receive`] when the session cannot be locked. That is
    /// **retryable**: a session lock outlives the `dispose` of whoever held
    /// it, so this happens on every reconnect
    /// (`spec/03-message-bus.md` §8.1.3).
    pub async fn session_receiver(
        &self,
        queue: &str,
        session: &str,
    ) -> Result<LiveReceiver, BusError> {
        let receiver = self
            .client
            .lock()
            .await
            .accept_session_for_queue(queue, session, ServiceBusSessionReceiverOptions::default())
            .await
            .map_err(|error| BusError::Receive {
                entity: queue.to_string(),
                reason: error.to_string(),
            })?;

        Ok(LiveReceiver::new(
            Inner::Session(receiver),
            queue.to_string(),
        ))
    }

    /// Close the connection.
    ///
    /// Every service here runs until the process stops, so this exists for
    /// the one that does not: `spec/14-buswatch.md` §7.3 requires BusWatch
    /// to release its AMQP resources on shutdown, because a developer
    /// restarting it repeatedly against the emulator's hard connection
    /// quota (§6.1) will otherwise run out.
    ///
    /// # Errors
    ///
    /// [`BusError::Connect`] carrying whatever the broker said on the way
    /// out — worth logging, not worth acting on: the process is stopping.
    pub async fn close(self) -> Result<(), BusError> {
        // `dispose` consumes the client, and the client lives behind a
        // mutex shared with every receiver. Taking `self` by value proves
        // no receiver can be handed out afterwards, so unwrapping the
        // `Mutex` here is sound rather than a race.
        self.client
            .into_inner()
            .dispose()
            .await
            .map_err(|error| BusError::Connect {
                namespace: self.namespace,
                reason: error.to_string(),
            })
    }

    /// A receiver for whichever session of a queue is free next.
    ///
    /// A session-enabled queue cannot be read without holding a session
    /// lock, so an inspector that does not know the session ids has to ask
    /// for the next available one (`spec/14-buswatch.md` §4.4). What comes
    /// back is one session's messages, not the queue's.
    ///
    /// # Errors
    ///
    /// [`BusError::Receive`] when no session is available, which on an idle
    /// queue is the normal case rather than a fault.
    pub async fn next_session_receiver(&self, queue: &str) -> Result<LiveReceiver, BusError> {
        let receiver = self
            .client
            .lock()
            .await
            .accept_next_session_for_queue(queue, ServiceBusSessionReceiverOptions::default())
            .await
            .map_err(|error| BusError::Receive {
                entity: queue.to_string(),
                reason: error.to_string(),
            })?;

        Ok(LiveReceiver::new(
            Inner::Session(receiver),
            queue.to_string(),
        ))
    }

    /// A receiver for one session of a topic subscription.
    ///
    /// # Errors
    ///
    /// As [`LiveBus::session_receiver`].
    pub async fn subscription_receiver(
        &self,
        topic: &str,
        subscription: &str,
        session: &str,
    ) -> Result<LiveReceiver, BusError> {
        // spec/03-message-bus.md §2.2 — a subscription names itself
        // `topic/subscription` everywhere it is reported.
        let entity = format!("{topic}/{subscription}");

        let receiver = self
            .client
            .lock()
            .await
            .accept_session_for_subscription(
                topic,
                subscription,
                session,
                ServiceBusSessionReceiverOptions::default(),
            )
            .await
            .map_err(|error| BusError::Receive {
                entity: entity.clone(),
                reason: error.to_string(),
            })?;

        Ok(LiveReceiver::new(Inner::Session(receiver), entity))
    }
}

/// Sends to one queue or topic.
pub struct LiveSender {
    /// The link. Behind a mutex so one sender can be shared.
    sender: Mutex<ServiceBusSender>,
    /// The queue or topic, for errors and logs.
    entity: String,
}

impl std::fmt::Debug for LiveSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSender")
            .field("entity", &self.entity)
            .finish_non_exhaustive()
    }
}

impl LiveSender {
    /// Send one message, retrying per its classification.
    ///
    /// At most [`MAX_SEND_ATTEMPTS`] attempts, and **fewer** when the failure
    /// is one that will not improve: a message too large for the entity is
    /// returned immediately rather than blocking the session for three round
    /// trips (`spec/03-message-bus.md` §3.2.2).
    ///
    /// # Errors
    ///
    /// [`BusError::Send`], carrying the classification so a caller can tell
    /// "try later" from "never".
    pub async fn send(&self, message: Outgoing) -> Result<(), BusError> {
        let mut last = String::new();
        let mut disposition = Disposition::Retry;

        for attempt in 1..=MAX_SEND_ATTEMPTS {
            let outgoing = to_service_bus(&message)?;

            match self.sender.lock().await.send_message(outgoing).await {
                Ok(()) => {
                    tracing::debug!(entity = %self.entity, "message sent");
                    return Ok(());
                }
                Err(error) => {
                    last = error.to_string();
                    disposition = classify(&last);

                    if disposition == Disposition::Fatal {
                        break;
                    }

                    tracing::warn!(
                        entity = %self.entity,
                        attempt,
                        ?disposition,
                        "a send failed; retrying"
                    );
                }
            }
        }

        Err(BusError::Send {
            destination: self.entity.clone(),
            attempts: MAX_SEND_ATTEMPTS,
            reason: last,
            disposition,
        })
    }

    /// Send several messages as one or more batches.
    ///
    /// The batch is split when a message would not fit, and a message that
    /// does not fit an **empty** batch is [`BusError::MessageTooLarge`] —
    /// unrecoverable, and not retried (`spec/03-message-bus.md` §3.3).
    ///
    /// Returns how many were sent.
    ///
    /// # Errors
    ///
    /// [`BusError::MessageTooLarge`] or [`BusError::Send`].
    pub async fn send_batch(&self, messages: Vec<Outgoing>) -> Result<usize, BusError> {
        if messages.is_empty() {
            return Ok(0);
        }

        let mut sender = self.sender.lock().await;
        let mut sent = 0;
        let mut batch = sender
            .create_message_batch(azservicebus::CreateMessageBatchOptions::default())
            .map_err(|error| BusError::Send {
                destination: self.entity.clone(),
                attempts: 1,
                reason: error.to_string(),
                disposition: Disposition::Retry,
            })?;
        let mut in_batch = 0;

        for message in messages {
            let outgoing = to_service_bus(&message)?;

            if batch.try_add_message(outgoing).is_err() {
                if in_batch == 0 {
                    // A single message that will not fit an empty batch
                    // cannot be sent at all; splitting further is not
                    // possible. spec/03-message-bus.md §3.3.2.
                    return Err(BusError::MessageTooLarge {
                        destination: self.entity.clone(),
                    });
                }

                // Flush and start again with this message.
                send_batch_inner(&mut sender, batch, &self.entity).await?;
                sent += in_batch;

                batch = sender
                    .create_message_batch(azservicebus::CreateMessageBatchOptions::default())
                    .map_err(|error| BusError::Send {
                        destination: self.entity.clone(),
                        attempts: 1,
                        reason: error.to_string(),
                        disposition: Disposition::Retry,
                    })?;
                in_batch = 0;

                let retry = to_service_bus(&message)?;
                if batch.try_add_message(retry).is_err() {
                    return Err(BusError::MessageTooLarge {
                        destination: self.entity.clone(),
                    });
                }
            }
            in_batch += 1;
        }

        if in_batch > 0 {
            send_batch_inner(&mut sender, batch, &self.entity).await?;
            sent += in_batch;
        }
        drop(sender);

        Ok(sent)
    }
}

/// Send one assembled batch.
async fn send_batch_inner(
    sender: &mut ServiceBusSender,
    batch: azservicebus::ServiceBusMessageBatch,
    entity: &str,
) -> Result<(), BusError> {
    sender
        .send_message_batch(batch)
        .await
        .map_err(|error| BusError::Send {
            destination: entity.to_string(),
            attempts: 1,
            reason: error.to_string(),
            disposition: classify(&error.to_string()),
        })
}

/// The three receiver shapes, which `azservicebus` gives distinct types.
enum Inner {
    /// A queue, with no session.
    Queue(ServiceBusReceiver),
    /// A queue or subscription session.
    Session(ServiceBusSessionReceiver),
}

/// Receives from a queue, a session, or a subscription.
///
/// Behind a [`tokio::sync::Mutex`] so the lock renewer can share it; see the
/// module documentation.
pub struct LiveReceiver {
    /// The receiver, and the messages currently in flight.
    state: Arc<Mutex<ReceiverState>>,
    /// The queue, or `topic/subscription`.
    entity: String,
}

/// A receiver and what it is currently holding.
struct ReceiverState {
    /// The receiver itself.
    inner: Inner,
    /// Messages received but not settled, by token.
    ///
    /// The broker's own message is what settles it, so it has to be kept —
    /// unlike the fake, where a token is enough.
    in_flight: Vec<(Token, ServiceBusReceivedMessage)>,
    /// The next token.
    next_token: u64,
}

impl std::fmt::Debug for LiveReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveReceiver")
            .field("entity", &self.entity)
            .finish_non_exhaustive()
    }
}

impl LiveReceiver {
    /// Wrap a receiver.
    fn new(inner: Inner, entity: String) -> Self {
        Self {
            state: Arc::new(Mutex::new(ReceiverState {
                inner,
                in_flight: Vec::new(),
                next_token: 0,
            })),
            entity,
        }
    }

    /// The queue, or `topic/subscription`.
    #[must_use]
    pub fn entity(&self) -> &str {
        &self.entity
    }

    /// Receive up to `limit` messages, waiting at most [`MAX_WAIT`].
    ///
    /// An empty result is not an error: it means the queue is empty
    /// (`spec/03-message-bus.md` §5.1.2).
    ///
    /// # Errors
    ///
    /// [`BusError::Receive`].
    pub async fn receive(&self, limit: u32) -> Result<Vec<Incoming>, BusError> {
        self.receive_within(limit, MAX_WAIT).await
    }

    /// Receive up to `limit` messages, waiting at most `wait`.
    ///
    /// [`LiveReceiver::receive`] waits [`MAX_WAIT`], which is right for a
    /// service: a receive loop that waits a minute for a message that is
    /// not there has lost nothing.
    ///
    /// It is wrong for an interactive tool, where an empty queue is the
    /// normal case and a developer is waiting on the answer
    /// (`spec/14-buswatch.md` §3).
    ///
    /// Shortening the wait rather than wrapping [`LiveReceiver::receive`] in
    /// [`tokio::time::timeout`] is deliberate, and the difference is not
    /// cosmetic: cancelling a receive mid-flight **loses messages the
    /// broker has already handed over** — see `spec/03-message-bus.md` §5.5,
    /// which was written after two queues with messages on them read as
    /// empty through fifteen attempts.
    ///
    /// # Errors
    ///
    /// [`BusError::Receive`].
    pub async fn receive_within(
        &self,
        limit: u32,
        wait: Duration,
    ) -> Result<Vec<Incoming>, BusError> {
        let mut state = self.state.lock().await;

        let received = match &mut state.inner {
            Inner::Queue(receiver) => {
                receiver
                    .receive_messages_with_max_wait_time(limit, wait)
                    .await
            }
            Inner::Session(receiver) => {
                receiver
                    .receive_messages_with_max_wait_time(limit, wait)
                    .await
            }
        }
        .map_err(|error| BusError::Receive {
            entity: self.entity.clone(),
            reason: error.to_string(),
        })?;

        let mut out = Vec::with_capacity(received.len());
        for message in received {
            state.next_token += 1;
            let token = Token(state.next_token);
            out.push(to_incoming(&message, token));
            state.in_flight.push((token, message));
        }
        // The lock renewer waits on this same mutex (§5.1.2), so hand it
        // back the moment the receiver is no longer needed rather than at
        // the end of the scope. A renewal that misses its window because a
        // caller was formatting a return value would lose the session.
        drop(state);

        Ok(out)
    }

    /// Look at up to `limit` messages without locking or consuming them.
    ///
    /// The read `spec/14-buswatch.md` §4.3 requires: a peek leaves the
    /// message where it is, so a developer can see what a service is about
    /// to process without taking it away from that service. `from` starts
    /// the scan at a sequence number; `None` continues from wherever the
    /// last peek on this receiver stopped.
    ///
    /// Peeked messages carry no lock token, so they come back as
    /// [`Peeked`] rather than [`Incoming`] and cannot be settled.
    ///
    /// # Errors
    ///
    /// [`BusError::Receive`].
    pub async fn peek(&self, limit: u32, from: Option<i64>) -> Result<Vec<Peeked>, BusError> {
        let mut state = self.state.lock().await;

        let peeked = match &mut state.inner {
            Inner::Queue(receiver) => receiver.peek_messages(limit, from).await,
            Inner::Session(receiver) => receiver.peek_messages(limit, from).await,
        }
        .map_err(|error| BusError::Receive {
            entity: self.entity.clone(),
            reason: error.to_string(),
        })?;
        drop(state);

        Ok(peeked.iter().map(to_peeked).collect())
    }

    /// Settle a received message.
    ///
    /// [`Outcome::Accepted`] and [`Outcome::Rejected`] complete it;
    /// [`Outcome::Deferred`] abandons it, so it is redelivered
    /// (`spec/index.md` §0.9).
    ///
    /// # Errors
    ///
    /// [`BusError::Disposition`].
    pub async fn settle(&self, message: &Incoming, outcome: Outcome) -> Result<(), BusError> {
        let mut state = self.state.lock().await;

        let Some(index) = state
            .in_flight
            .iter()
            .position(|(token, _)| *token == message.token)
        else {
            return Err(BusError::Disposition {
                action: "settle",
                entity: self.entity.clone(),
                reason: "the message is not in flight".to_string(),
            });
        };
        let (_, held) = state.in_flight.remove(index);

        let action = if outcome.is_settled() {
            "complete"
        } else {
            "abandon"
        };

        let result = match (&mut state.inner, outcome) {
            (Inner::Queue(receiver), Outcome::Accepted | Outcome::Rejected) => {
                receiver.complete_message(&held).await
            }
            (Inner::Queue(receiver), Outcome::Deferred) => {
                receiver.abandon_message(&held, None).await
            }
            (Inner::Session(receiver), Outcome::Accepted | Outcome::Rejected) => {
                receiver.complete_message(&held).await
            }
            (Inner::Session(receiver), Outcome::Deferred) => {
                receiver.abandon_message(&held, None).await
            }
        };

        drop(state);

        result.map_err(|error| BusError::Disposition {
            action,
            entity: self.entity.clone(),
            reason: error.to_string(),
        })
    }

    /// Start renewing this receiver's session lock.
    ///
    /// Returns `None` for a queue receiver, which has no session to renew.
    ///
    /// **Stop the renewer before dropping the receiver**
    /// (`spec/03-message-bus.md` §5.1.4). The renewer locks the receiver
    /// briefly, so a handler that holds no lock does not block it — which is
    /// the arrangement that lets a slow message keep its session.
    #[must_use]
    pub fn renew_session_lock(&self) -> Option<LockRenewer> {
        let state = Arc::clone(&self.state);

        Some(LockRenewer::start(move || {
            let state = Arc::clone(&state);
            async move {
                let mut state = state.lock().await;
                match &mut state.inner {
                    Inner::Session(receiver) => receiver.renew_session_lock().await.is_ok(),
                    // A queue receiver has no session; reporting success
                    // keeps the renewer quiet rather than counting failures.
                    Inner::Queue(_) => true,
                }
            }
        }))
    }

    /// Close the receiver.
    ///
    /// # Errors
    ///
    /// [`BusError::Receive`] when disposal fails, which is worth logging and
    /// not worth acting on.
    pub async fn close(self) -> Result<(), BusError> {
        let entity = self.entity.clone();
        let Ok(state) = Arc::try_unwrap(self.state) else {
            // The renewer still holds a reference, which means §5.1.4 was
            // not honoured: it should have been stopped first.
            return Err(BusError::Receive {
                entity,
                reason: "the receiver is still shared; stop the lock renewer first".to_string(),
            });
        };

        let state = state.into_inner();
        let result = match state.inner {
            Inner::Queue(receiver) => receiver.dispose().await,
            Inner::Session(receiver) => receiver.dispose().await,
        };

        result.map_err(|error| BusError::Receive {
            entity,
            reason: error.to_string(),
        })
    }
}

/// Turn one of ours into one of theirs.
fn to_service_bus(message: &Outgoing) -> Result<ServiceBusMessage, BusError> {
    let mut outgoing = ServiceBusMessage::new(message.body.clone());

    if let Some(id) = &message.message_id {
        outgoing
            .set_message_id(id.clone())
            .map_err(|error| BusError::Send {
                destination: String::new(),
                attempts: 1,
                reason: error.to_string(),
                disposition: Disposition::Fatal,
            })?;
    }

    if let Some(session) = &message.session_id {
        outgoing
            .set_session_id(session.clone())
            .map_err(|error| BusError::Send {
                destination: String::new(),
                attempts: 1,
                reason: error.to_string(),
                disposition: Disposition::Fatal,
            })?;
    }

    // spec/03-message-bus.md §3.1.1 — an empty map is sent as absent.
    if !message.properties.is_empty() {
        let mut builder = ApplicationProperties::builder();
        for (key, value) in &message.properties {
            builder = builder.insert(key.clone(), value.clone());
        }
        *outgoing.application_properties_mut() = Some(builder.build());
    }

    Ok(outgoing)
}

/// Turn one of theirs into one of ours.
fn to_incoming(message: &ServiceBusReceivedMessage, token: Token) -> Incoming {
    let mut properties = Properties::new();

    if let Some(raw) = message.application_properties() {
        for (key, value) in &raw.0 {
            // spec/03-message-bus.md §4.2 — only string values. An integer
            // or a boolean is a valid Service Bus property and not a valid
            // propagation header; handing one to the W3C propagator is how
            // the Python got a TypeError out of `re.search`.
            if let fe2o3_amqp_types::primitives::SimpleValue::String(text) = value {
                properties.insert(key.clone(), text.clone());
            }
        }
    }

    Incoming {
        body: message.body().map(<[u8]>::to_vec).unwrap_or_default(),
        message_id: message.message_id().map(|id| id.to_string()),
        session_id: message.session_id().map(str::to_string),
        properties,
        delivery_count: message.delivery_count(),
        token,
    }
}

/// Convert a peeked message into the type of `spec/14-buswatch.md` §4.2.
///
/// Same string-only property filter as [`to_incoming`], for the reason in
/// `spec/03-message-bus.md` §4.2.
fn to_peeked(message: &ServiceBusPeekedMessage) -> Peeked {
    let mut properties = Properties::new();

    if let Some(raw) = message.application_properties() {
        for (key, value) in &raw.0 {
            if let fe2o3_amqp_types::primitives::SimpleValue::String(text) = value {
                properties.insert(key.clone(), text.clone());
            }
        }
    }

    Peeked {
        body: message.body().map(<[u8]>::to_vec).unwrap_or_default(),
        sequence: message.sequence_number(),
        enqueued_at: message
            .enqueued_time()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| message.enqueued_time().to_string()),
        message_id: message.message_id().map(|id| id.to_string()),
        session_id: message.session_id().map(str::to_string),
        properties,
        delivery_count: message.delivery_count(),
        correlation_id: message.correlation_id().map(|id| id.to_string()),
        content_type: message.content_type().map(str::to_string),
        subject: message.subject().map(str::to_string),
    }
}

/// Whether renewal ended cleanly.
///
/// A convenience for the receive loop, which wants to log the difference
/// between "the batch finished" and "we lost the session".
#[must_use]
pub const fn renewal_was_clean(ended: Ended) -> bool {
    matches!(ended, Ended::Stopped)
}

impl MessageSink for LiveSender {
    /// [`LiveSender::send`], with its retry and its classification.
    async fn send(&self, message: Outgoing) -> Result<(), BusError> {
        Self::send(self, message).await
    }

    fn entity(&self) -> &str {
        &self.entity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_receive_wait_matches_the_specification() {
        // spec/03-message-bus.md §5.1.2
        assert_eq!(MAX_WAIT, Duration::from_secs(60));
    }

    #[test]
    fn an_outgoing_message_carries_its_identity_and_properties() {
        let mut properties = Properties::new();
        properties.insert("CorrelationId".into(), "3f6d".into());

        let outgoing = to_service_bus(
            &Outgoing::new("MSH|^~\\&|252")
                .with_message_id("MSG1")
                .with_session("phw-to-mpi")
                .with_properties(properties),
        )
        .unwrap();

        assert_eq!(outgoing.message_id().as_deref(), Some("MSG1"));
        assert_eq!(outgoing.session_id(), Some("phw-to-mpi"));
        assert!(outgoing.application_properties().is_some());
    }

    #[test]
    fn an_empty_property_map_is_sent_as_absent() {
        // spec/03-message-bus.md §3.1.1
        let outgoing = to_service_bus(&Outgoing::new("x")).unwrap();
        assert!(outgoing.application_properties().is_none());
    }

    #[test]
    fn a_message_without_a_session_has_none() {
        let outgoing = to_service_bus(&Outgoing::new("x")).unwrap();
        assert_eq!(outgoing.session_id(), None);
        assert_eq!(outgoing.message_id(), None);
    }

    #[test]
    fn a_clean_renewal_is_only_a_stop() {
        // Anything else means the session was lost, which the receive loop
        // logs differently.
        assert!(renewal_was_clean(Ended::Stopped));
        assert!(!renewal_was_clean(Ended::Expired));
        assert!(!renewal_was_clean(Ended::Failed));
    }
}
