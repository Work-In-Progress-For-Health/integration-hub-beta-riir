//! A TCP health probe that runs inside a distroless image.
//!
//! Implements `spec/18-deployment.md` §3.1 for the local stack.
//!
//! # Why this exists
//!
//! §3.1's probe is a TCP connect. Azure Container Apps performs it from
//! the platform, so a production image needs nothing. **Compose does not**:
//! its `healthcheck` runs a command *inside* the container, and the Python
//! stack uses `python -c "…socket…"` for exactly this.
//!
//! A distroless image has no Python and no shell (§2.3), so that command
//! cannot work — and §5.4 requires a profile to reach a healthy state with
//! no manual step. This is the smallest thing that closes the gap: one
//! static binary, already built by the same toolchain, copied in beside
//! the service.
//!
//! # Usage
//!
//! ```text
//! health-probe [port]
//! ```
//!
//! With no argument it reads `HEALTH_CHECK_PORT`, then falls back to
//! [`ihb_health::DEFAULT_PORT`]. It connects to `127.0.0.1` — the probe
//! runs inside the container, so loopback is right here even though the
//! listener binds `0.0.0.0` (§2.8).
//!
//! Exits 0 when the connection succeeds and 1 otherwise, which is what a
//! Compose healthcheck reads.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

/// How long to wait for the connection.
///
/// Shorter than the two seconds the Python's probe allows, because a
/// health check that takes longer than its own interval is a health check
/// that overlaps itself.
const TIMEOUT: Duration = Duration::from_millis(1500);

fn main() -> ExitCode {
    let port = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("HEALTH_CHECK_PORT").ok())
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(ihb_health::DEFAULT_PORT);

    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    match TcpStream::connect_timeout(&address, TIMEOUT) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            // To stderr, so `podman inspect`'s health log carries the
            // reason rather than an empty failure.
            eprintln!("health-probe: {address} is not accepting connections: {error}");
            ExitCode::FAILURE
        }
    }
}
