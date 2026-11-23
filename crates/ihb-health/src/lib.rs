//! The TCP health-check listener every Integration Hub service runs.
//!
//! Implements `spec/05-health-check.md`.
//!
//! Azure Container Apps probes a container by opening a TCP connection. A
//! successful connect means the process is alive and its runtime is
//! scheduling work; that is the whole contract. This listener accepts a
//! connection and immediately closes it, writing nothing and reading
//! nothing.
//!
//! An HTTP probe would be more informative and would also mean every
//! background worker in the estate carried an HTTP stack for the sake of
//! one endpoint. This is the cheaper answer and it is the one the platform
//! is configured for.
//!
//! # This crate is a facade
//!
//! The implementation lives in [`tcp_health_check`], which was extracted
//! from this workspace and published: nothing about "accept a connection
//! and close it" is specific to the Integration Hub, and the probe binary
//! that goes with it solves a problem every distroless image has. What
//! stays here is which clauses it implements, and the defaults
//! `spec/05-health-check.md` §2.2 fixes.
//!
//! # Examples
//!
//! ```
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use ihb_health::HealthCheck;
//!
//! // Port 0 asks the operating system for a free one, which is what a test
//! // wants; a service passes HEALTH_CHECK_PORT.
//! let health = HealthCheck::start("127.0.0.1", 0).await?;
//!
//! // The probe: connect, and be closed.
//! let _probe = tokio::net::TcpStream::connect(health.local_address()).await?;
//!
//! health.stop().await;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable
    )
)]

/// The address to bind when `HEALTH_CHECK_HOST` is not set.
///
/// `127.0.0.1` rather than `0.0.0.0`, because that is the safe default on a
/// laptop. Every container definition overrides it
/// (`spec/05-health-check.md` §2.2).
pub const DEFAULT_HOST: &str = tcp_health_check::DEFAULT_HOST;

/// The port to bind when `HEALTH_CHECK_PORT` is not set.
pub const DEFAULT_PORT: u16 = tcp_health_check::DEFAULT_PORT;

pub use tcp_health_check::{BindError, HealthCheck};

#[cfg(test)]
mod tests {
    use tokio::net::TcpStream;

    use super::*;

    #[test]
    fn the_defaults_are_the_ones_the_specification_fixes() {
        // spec/05-health-check.md §2.2. The upstream crate lets a caller
        // choose; this workspace does not, and this is what notices if an
        // upstream default ever moves.
        assert_eq!(DEFAULT_HOST, "127.0.0.1");
        assert_eq!(DEFAULT_PORT, 9000);
    }

    #[tokio::test]
    async fn a_probe_connects_and_is_closed() {
        // spec/05-health-check.md §4.1 — the contract, asserted here
        // because it is this workspace's clause.
        let health = HealthCheck::start("127.0.0.1", 0).await.unwrap();
        assert!(TcpStream::connect(health.local_address()).await.is_ok());
        health.stop().await;
    }

    #[tokio::test]
    async fn stopping_refuses_later_probes() {
        // spec/05-health-check.md §4.3
        let health = HealthCheck::start("127.0.0.1", 0).await.unwrap();
        let address = health.local_address();
        health.stop().await;

        assert!(TcpStream::connect(address).await.is_err());
    }
}
