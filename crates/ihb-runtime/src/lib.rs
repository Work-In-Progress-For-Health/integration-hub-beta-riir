//! Signals, graceful shutdown, and the retry schedule every worker shares.
//!
//! Implements `spec/01-architecture.md` §1.3, `spec/03-message-bus.md` §5.4,
//! and `spec/04-observability.md` §4.7.
//!
//! Every background worker in the Integration Hub has the same skeleton:
//! receive a batch, process it, dispose of it, repeat until told to stop,
//! and wait a bit longer each time something goes wrong. This crate is that
//! skeleton, minus the bus itself — which lives in `ihb-bus`, so that the
//! parts a test needs to drive are separable from the parts that need a
//! broker.
//!
//! # Modules
//!
//! - [`shutdown`] — noticing `SIGTERM` and stopping between messages rather
//!   than during one.
//! - [`backoff`] — how long a failing receive waits before trying again.
//!
//! # The rule these two exist to protect
//!
//! **A message that has been acknowledged must have been forwarded.** Both
//! modules are shaped by it: shutdown is cooperative so a stop cannot land
//! mid-message, and backoff sleeps in slices so waiting for a retry cannot
//! outlast the platform's grace period and turn a stop into a kill.
//!
//! # Examples
//!
//! ```
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! use ihb_runtime::{backoff::Backoff, shutdown::Shutdown};
//!
//! let shutdown = Shutdown::new();
//! shutdown.listen_for_signals();
//!
//! let watcher = shutdown.watcher();
//! let mut backoff = Backoff::new();
//!
//! # shutdown.stop();
//! while watcher.is_running() {
//!     // ... receive and process a batch ...
//!     # break;
//! }
//! # let _ = backoff.failed();
//! # }
//! ```

#![forbid(unsafe_code)]
// A test asserts; asserting is what `unwrap` and `panic` are for. The lints
// stay on for everything the container actually runs.
// See spec/17-testing-and-quality.md §2.3.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod backoff;
pub mod shutdown;

pub use backoff::Backoff;
pub use shutdown::{Shutdown, Watcher};
