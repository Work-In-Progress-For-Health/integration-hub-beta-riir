//! One place to send a message to.
//!
//! Implements `spec/03-message-bus.md` §7.2.
//!
//! A service sends to a queue or a topic it was configured with, and does
//! not care which — nor whether the thing on the other side is Azure or a
//! test double. [`MessageSink`] is that narrowing: an entity name, and a
//! way to put one message on it.
//!
//! # Why a trait and not an enum
//!
//! The alternative is a `enum AnySink { Live(..), Fake(..) }` that every
//! service matches on. That works, but it means the production type and the
//! test type are welded together in one definition, and adding a third — a
//! recording sink, a sink that drops everything — edits a type that
//! production code depends on. A trait lets a test define its own sink
//! without touching this file, and `spec/07-hl7-server.md` §9.3 needs
//! exactly that: a sink that records the *order* calls arrive in.
//!
//! # Why it is not object safe, and why that is fine
//!
//! `async fn` in a trait is not dyn-compatible, so there is no
//! `Box<dyn MessageSink>`. Nothing needs one. A service picks its sinks
//! once at startup and holds them for its lifetime, so the type can be a
//! generic parameter; the two call sites in `hl7-server` are the egress
//! sink and the store sink, and both are fixed the moment configuration is
//! read. Generics also mean the send path has no vtable dispatch on the
//! per-message hot path, which the boxed version would.

use std::future::Future;

use crate::error::BusError;
use crate::message::Outgoing;

/// Somewhere a message can be sent.
///
/// Implemented by [`crate::live::LiveSender`] for the real broker and by
/// [`crate::fake::FakeSender`] for tests.
///
/// # Examples
///
/// Writing a function once, against both:
///
/// ```
/// use ihb_bus::fake::FakeBus;
/// use ihb_bus::message::Outgoing;
/// use ihb_bus::sink::MessageSink;
///
/// /// Forward a message, naming its destination if it fails.
/// async fn forward<S: MessageSink>(sink: &S, body: &str) -> Result<(), String> {
///     sink.send(Outgoing::new(body))
///         .await
///         .map_err(|error| format!("{}: {error}", sink.entity()))
/// }
///
/// # tokio_test::block_on(async {
/// let bus = FakeBus::new();
/// let sink = bus.sender("pre-phw-transform");
///
/// forward(&sink, "MSH|^~\\&|252").await.unwrap();
/// assert_eq!(bus.all_sent("pre-phw-transform").len(), 1);
/// # });
/// ```
pub trait MessageSink: Send + Sync {
    /// Send one message.
    ///
    /// Whether this retries, and how many times, is the implementation's
    /// business — [`crate::live::LiveSender`] retries per
    /// `spec/03-message-bus.md` §3.2 and the fake does not. A caller sees
    /// only the outcome, and MUST treat an error as "not sent": for an
    /// ingress server that means no acknowledgement (§5.3 of
    /// `spec/07-hl7-server.md`).
    ///
    /// # Errors
    ///
    /// [`BusError::Send`] when the message could not be handed over, or
    /// [`BusError::MessageTooLarge`] when it never could be.
    fn send(&self, message: Outgoing) -> impl Future<Output = Result<(), BusError>> + Send;

    /// The queue or topic this sends to.
    ///
    /// For error messages and log lines. A failure that does not say where
    /// it was going is not much of a failure report.
    fn entity(&self) -> &str;
}
