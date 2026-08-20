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
//! background worker in the estate carried an HTTP stack for the sake of one
//! endpoint. This is the cheaper answer and it is the one the platform is
//! configured for.
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
// A test asserts; asserting is what `unwrap` and `panic` are for. The lints
// stay on for everything the container actually runs.
// See spec/17-testing-and-quality.md §2.3.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// The address to bind when `HEALTH_CHECK_HOST` is not set.
///
/// `127.0.0.1` rather than `0.0.0.0`, because that is the safe default on a
/// laptop. Every container definition overrides it (`spec/05-health-check.md`
/// §2.2).
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// The port to bind when `HEALTH_CHECK_PORT` is not set.
pub const DEFAULT_PORT: u16 = 9000;

/// How long [`HealthCheck::stop`] waits for the accept task to finish.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Why a health-check listener could not start.
///
/// Binding is the only thing that can fail, and it is reported rather than
/// swallowed: a container that cannot bind its health check will be killed
/// by the platform, and it should say why first
/// (`spec/05-health-check.md` §3.2).
#[derive(Debug, thiserror::Error)]
#[error("could not bind the health check listener to {address}: {source}")]
pub struct BindError {
    /// What was being bound, as `host:port`.
    pub address: String,
    /// The underlying failure.
    #[source]
    pub source: io::Error,
}

/// A running health-check listener.
///
/// Started with [`HealthCheck::start`] and stopped with
/// [`HealthCheck::stop`], or on drop. Dropping it aborts the accept task
/// rather than waiting for it, so a service that wants an orderly shutdown
/// calls `stop`.
///
/// # Examples
///
/// ```
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ihb_health::HealthCheck;
///
/// let health = HealthCheck::start("127.0.0.1", 0).await?;
/// let address = health.local_address();
///
/// assert!(tokio::net::TcpStream::connect(address).await.is_ok());
/// health.stop().await;
/// assert!(tokio::net::TcpStream::connect(address).await.is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct HealthCheck {
    /// Where the listener actually bound, which differs from what was asked
    /// for when port 0 was requested.
    local_address: SocketAddr,
    /// The accept loop. Taken by `stop`, aborted by `drop`.
    accepting: Option<JoinHandle<()>>,
}

impl HealthCheck {
    /// Bind and begin accepting.
    ///
    /// Returns as soon as the socket is bound, so a service that starts its
    /// health check before its main loop reports healthy immediately
    /// (`spec/05-health-check.md` §3.1).
    ///
    /// `SO_REUSEADDR` is set by tokio's listener, so a restart does not fail
    /// on a socket still in `TIME_WAIT` (§2.3).
    ///
    /// # Errors
    ///
    /// [`BindError`] when the address cannot be bound — in practice, a port
    /// already in use or one the process may not have.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use ihb_health::HealthCheck;
    ///
    /// let health = HealthCheck::start("127.0.0.1", 0).await?;
    /// assert_eq!(health.local_address().ip().to_string(), "127.0.0.1");
    /// health.stop().await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn start(host: &str, port: u16) -> Result<Self, BindError> {
        let address = format!("{host}:{port}");
        let listener = TcpListener::bind(&address)
            .await
            .map_err(|source| BindError {
                address: address.clone(),
                source,
            })?;

        let local_address = listener
            .local_addr()
            .map_err(|source| BindError { address, source })?;

        tracing::info!(%local_address, "health check listening");

        let accepting = tokio::spawn(async move {
            accept_loop(listener).await;
        });

        Ok(Self {
            local_address,
            accepting: Some(accepting),
        })
    }

    /// Bind using the defaults for anything not configured.
    ///
    /// An absent host or port resolves to [`DEFAULT_HOST`] and
    /// [`DEFAULT_PORT`] rather than disabling the listener — the Python
    /// passes `None` and its constructor substitutes the same values
    /// (`spec/05-health-check.md` §2.1).
    ///
    /// # Errors
    ///
    /// [`BindError`], as [`HealthCheck::start`].
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use ihb_health::HealthCheck;
    ///
    /// // Nothing configured, but a port is still chosen; 0 here only so the
    /// // example does not fight whatever is on 9000.
    /// let health = HealthCheck::start_or_default(None, Some(0)).await?;
    /// assert_eq!(health.local_address().ip().to_string(), "127.0.0.1");
    /// health.stop().await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn start_or_default(
        host: Option<&str>,
        port: Option<u16>,
    ) -> Result<Self, BindError> {
        Self::start(host.unwrap_or(DEFAULT_HOST), port.unwrap_or(DEFAULT_PORT)).await
    }

    /// Where the listener is bound.
    ///
    /// Worth having because `start` may have been asked for port 0, and a
    /// test needs to know what it got.
    #[must_use]
    pub const fn local_address(&self) -> SocketAddr {
        self.local_address
    }

    /// Stop accepting and wait for the accept task to finish.
    ///
    /// Waits at most one second; a task that has not finished by
    /// then is abandoned rather than holding up a shutdown that the platform
    /// is already counting down (`spec/05-health-check.md` §3.3).
    pub async fn stop(mut self) {
        let Some(accepting) = self.accepting.take() else {
            return;
        };

        accepting.abort();

        match tokio::time::timeout(SHUTDOWN_TIMEOUT, accepting).await {
            // The expected path: aborted, so the join reports cancellation.
            Ok(Err(error)) if error.is_cancelled() => {}
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, "the health check accept task ended badly");
            }
            Err(_) => {
                tracing::warn!(
                    "the health check accept task did not stop within {:?}",
                    SHUTDOWN_TIMEOUT
                );
            }
        }

        tracing::debug!(local_address = %self.local_address, "health check stopped");
    }
}

impl Drop for HealthCheck {
    fn drop(&mut self) {
        if let Some(accepting) = self.accepting.take() {
            accepting.abort();
        }
    }
}

/// Accept connections and close them, until the task is cancelled.
///
/// An accept error ends the loop rather than spinning on it
/// (`spec/05-health-check.md` §3.4): the errors that reach here are the ones
/// that will keep happening — the listener closed, the file-descriptor limit
/// reached — and a tight loop over them would burn a core.
async fn accept_loop(listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                tracing::trace!(%peer, "health check probed");
                // Dropping the stream closes it. Nothing is written, and
                // nothing is read: a probe that never writes must not be
                // kept waiting (spec/05-health-check.md §1.1, §1.2).
                drop(stream);
            }
            Err(error) => {
                tracing::warn!(%error, "the health check listener stopped accepting");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    use super::*;

    /// Start a listener on an operating-system-chosen port.
    async fn started() -> HealthCheck {
        HealthCheck::start("127.0.0.1", 0)
            .await
            .expect("binding to an ephemeral port")
    }

    #[tokio::test]
    async fn a_probe_connects() {
        // spec/05-health-check.md §4.1
        let health = started().await;
        let stream = TcpStream::connect(health.local_address()).await;
        assert!(stream.is_ok());
        health.stop().await;
    }

    #[tokio::test]
    async fn a_probe_sees_end_of_file_and_no_payload() {
        // spec/05-health-check.md §4.1, §4.2
        let health = started().await;
        let mut stream = TcpStream::connect(health.local_address()).await.unwrap();

        let mut buffer = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut buffer))
            .await
            .expect("the connection is closed promptly")
            .expect("reading to end of file");

        assert_eq!(read, 0, "the listener must write nothing");
        assert!(buffer.is_empty());
        health.stop().await;
    }

    #[tokio::test]
    async fn stopping_refuses_later_probes() {
        // spec/05-health-check.md §4.3
        let health = started().await;
        let address = health.local_address();
        health.stop().await;

        assert!(TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn binding_a_taken_port_is_an_error_not_a_panic() {
        // spec/05-health-check.md §4.4
        let held = started().await;
        let address = held.local_address();

        let error = HealthCheck::start(&address.ip().to_string(), address.port())
            .await
            .expect_err("the port is already bound");

        assert!(error.to_string().contains(&address.port().to_string()));
        held.stop().await;
    }

    #[tokio::test]
    async fn a_port_can_be_rebound_after_stopping() {
        // spec/05-health-check.md §4.5 — what SO_REUSEADDR is for.
        let first = started().await;
        let address = first.local_address();
        first.stop().await;

        let second = HealthCheck::start(&address.ip().to_string(), address.port()).await;
        assert!(second.is_ok(), "{:?}", second.err());
        second.unwrap().stop().await;
    }

    #[tokio::test]
    async fn defaults_apply_when_nothing_is_configured() {
        // spec/05-health-check.md §2.1 — absent means the default, not off.
        let health = HealthCheck::start_or_default(None, Some(0)).await.unwrap();
        assert_eq!(health.local_address().ip().to_string(), DEFAULT_HOST);
        health.stop().await;
    }

    #[tokio::test]
    async fn several_probes_are_served() {
        // spec/05-health-check.md §1.1 — the platform probes repeatedly.
        let health = started().await;
        for _ in 0..5 {
            assert!(TcpStream::connect(health.local_address()).await.is_ok());
        }
        health.stop().await;
    }

    #[tokio::test]
    async fn dropping_without_stopping_releases_the_port() {
        // spec/05-health-check.md §3.5 — a service cannot forget to stop it.
        let address = {
            let health = started().await;
            health.local_address()
        };

        // The accept task is aborted on drop; give the runtime a moment to
        // run the cancellation before asserting the socket has gone.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(TcpStream::connect(address).await.is_err());
    }
}
