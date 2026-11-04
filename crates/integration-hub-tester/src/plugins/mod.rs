//! The plugins, one component each.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §2.5 and §4.

pub mod proms;
pub mod senders;
pub mod servers;
pub mod transformers;

use std::future::Future;

/// Drive one future on a private single-threaded runtime.
///
/// # Errors
///
/// A message when the runtime itself cannot be built; the future's own
/// outcome is returned inside `Ok`.
///
/// The components that have an `async` entry point (the REST and SOAP
/// processors, the SOAP post) are called from a synchronous `run`, and a
/// current-thread runtime is the lightest thing that satisfies them. A test
/// MUST NOT call this from inside another runtime; `block_on` panics there.
pub(crate) fn block_on<F: Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|runtime| runtime.block_on(future))
        .map_err(|error| format!("could not start the async runtime: {error}"))
}
