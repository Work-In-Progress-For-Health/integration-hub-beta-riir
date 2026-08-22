//! Azure Service Bus, as the Integration Hub uses it.
//!
//! Implements `spec/03-message-bus.md`.
//!
//! Every process in the estate is a loop around this crate: ingress servers
//! send, transformers receive and send, senders receive. Nothing calls
//! anything else directly, so what this crate guarantees is what the
//! architecture guarantees.
//!
//! # Three things worth knowing before reading further
//!
//! **A message that has been acknowledged must have been forwarded.** Every
//! receive is peek-lock, and a message is completed only after the work it
//! represents has succeeded. [`message::Outcome`] is how a handler says
//! which of those happened.
//!
//! **Plaintext is reachable, and is fenced off.** `azservicebus` ignores
//! `UseDevelopmentEmulator` and needs its `unsecured` feature to talk to the
//! local emulator at all — a feature that, used by mistake, would put
//! clinical messages on the wire unencrypted. [`connection::Endpoint`] makes
//! that choice once, from the connection string, and nothing else can.
//!
//! **There is no `AutoLockRenewer`.** The Python's had one; `azservicebus`
//! does not, so renewal is ours to run. See `spec/03-message-bus.md` §8.1.2.
//!
//! # Testing
//!
//! [`fake::FakeBus`] is an in-memory bus behind the same shapes, so a
//! service's tests need no broker. It models the part services get wrong —
//! dispositions — and will tell a test when a handler left a message
//! unsettled.
//!
//! # Examples
//!
//! ```
//! use ihb_bus::fake::FakeBus;
//! use ihb_bus::message::{Outcome, Outgoing};
//! use ihb_bus::metadata::Tracking;
//!
//! let bus = FakeBus::new();
//!
//! let tracking = Tracking {
//!     correlation_id: "3f6d".into(),
//!     received_at: "2026-08-20T09:15:00+00:00".into(),
//!     source_system: "252".into(),
//!     workflow_id: "phw-to-mpi".into(),
//! };
//!
//! bus.send(
//!     "pre-phw-transform",
//!     Outgoing::new("MSH|^~\\&|252")
//!         .with_message_id("MSG1")
//!         .with_session("phw-to-mpi")
//!         .with_properties(tracking.to_properties()),
//! )?;
//!
//! let received = bus.receive("pre-phw-transform", 10);
//! assert_eq!(received[0].properties["CorrelationId"], "3f6d");
//!
//! bus.settle(&received[0], Outcome::Accepted)?;
//! assert!(bus.in_flight("pre-phw-transform").is_empty());
//! # Ok::<(), ihb_bus::BusError>(())
//! ```

#![forbid(unsafe_code)]
// A test asserts; asserting is what `unwrap` and `panic` are for. The lints
// stay on for everything the container actually runs.
// See spec/17-testing-and-quality.md §2.3.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod connection;
pub mod error;
pub mod fake;
pub mod live;
pub mod message;
pub mod metadata;
pub mod renew;
pub mod retry;
pub mod sink;
pub mod store;

pub use connection::Endpoint;
pub use error::BusError;
pub use fake::FakeBus;
pub use live::{LiveBus, LiveReceiver, LiveSender};
pub use message::{Incoming, Outcome, Outgoing, Token};
pub use metadata::{LogValues, Properties, Tracking};
pub use renew::LockRenewer;
pub use store::StoreEvent;
