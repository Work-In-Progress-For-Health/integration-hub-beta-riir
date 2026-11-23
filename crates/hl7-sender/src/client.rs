//! The MLLP connection to the downstream system.
//!
//! Implements `spec/10-senders.md` §3.
//!
//! # One connection, rebuilt when it dies
//!
//! §3.1 — a long-lived TCP connection, not one per message. MPI and the
//! other receivers expect it, and opening a socket per message against a
//! system taking thousands an hour is a different load profile than the one
//! they were sized for.
//!
//! The cost is that a connection can go stale between messages without
//! anyone noticing, so §3.2 checks before each send and §3.4 retries once
//! on a timeout. Both are here.

use std::io;
use std::time::Duration;

use ihb_sender::{Delivery, DeliveryError, Verdict};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::ack::evaluate;

/// Where to send, and how long to wait.
#[derive(Debug, Clone)]
pub struct Destination {
    /// `RECEIVER_MLLP_HOST`.
    pub host: String,
    /// `RECEIVER_MLLP_PORT`.
    pub port: u16,
    /// `ACK_TIMEOUT_SECONDS`.
    pub ack_timeout: Duration,
}

impl Destination {
    /// `host:port`, for connecting and for log lines.
    #[must_use]
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// An MLLP client that keeps its connection between messages.
#[derive(Debug)]
pub struct MllpClient {
    /// Where it sends.
    destination: Destination,
    /// The connection, when there is one.
    stream: Option<TcpStream>,
}

impl MllpClient {
    /// A client that will connect on its first send.
    #[must_use]
    pub const fn new(destination: Destination) -> Self {
        Self {
            destination,
            stream: None,
        }
    }

    /// Where this sends.
    #[must_use]
    pub const fn destination(&self) -> &Destination {
        &self.destination
    }

    /// Send once over the current connection, opening one if needed.
    ///
    /// Returns the reply's payload, MLLP framing already stripped (§3.6).
    async fn exchange(&mut self, message: &str) -> Result<String, Attempt> {
        // §3.2 — check before sending, and rebuild if the peer has gone.
        if self.is_stale() {
            tracing::info!(
                address = %self.destination.address(),
                "the connection was closed by the peer; rebuilding"
            );
            self.stream = None;
        }

        if self.stream.is_none() {
            let stream = TcpStream::connect(self.destination.address())
                .await
                .map_err(|error| Attempt::Connection(error.to_string()))?;
            tracing::info!(address = %self.destination.address(), "connected");
            self.stream = Some(stream);
        }

        let Some(stream) = self.stream.as_mut() else {
            return Err(Attempt::Connection("no connection".to_string()));
        };

        stream
            .write_all(&hl7_2_mllp::encode(message.as_bytes()))
            .await
            .map_err(|error| Attempt::Connection(error.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|error| Attempt::Connection(error.to_string()))?;

        read_frame(stream, self.destination.ack_timeout).await
    }

    /// Whether the peer has closed the connection.
    ///
    /// A zero-byte read on a readable socket means the far end is gone.
    /// Checking without consuming is what `poll_peek` is for: a byte
    /// already waiting is the start of a reply we have not asked for yet,
    /// and must not be swallowed.
    fn is_stale(&self) -> bool {
        let Some(stream) = self.stream.as_ref() else {
            return false;
        };

        let mut probe = [0_u8; 1];
        let mut buffer = tokio::io::ReadBuf::new(&mut probe);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);

        match stream.poll_peek(&mut context, &mut buffer) {
            // Readable and empty means the peer closed; an error on the
            // socket is as good as closed. Either way the next send needs a
            // new connection.
            std::task::Poll::Ready(Ok(0) | Err(_)) => true,
            // Data waiting, or nothing to say yet: the connection is alive.
            std::task::Poll::Ready(Ok(_)) | std::task::Poll::Pending => false,
        }
    }

    /// Drop the connection, so the next send opens a new one.
    fn disconnect(&mut self) {
        self.stream = None;
    }
}

/// What one attempt produced.
#[derive(Debug)]
enum Attempt {
    /// No reply within the timeout. Retryable once (§3.4).
    Timeout,
    /// A socket error. Not retried here (§3.5).
    Connection(String),
}

impl Delivery for MllpClient {
    fn endpoint(&self) -> String {
        self.destination.address()
    }

    /// §3.3, §3.4 and §3.5.
    async fn deliver(&mut self, message: &str) -> Result<Verdict, DeliveryError> {
        match self.exchange(message).await {
            Ok(reply) => return Ok(evaluate(&reply)),

            Err(Attempt::Timeout) => {
                // §3.4 — close, reopen, and retry **exactly once**. A
                // receiver that was restarting when the first attempt went
                // out will answer the second.
                let seconds = self.destination.ack_timeout.as_secs();
                tracing::warn!(
                    seconds,
                    "no ACK within the timeout; retrying once on a new connection"
                );
                self.disconnect();
            }

            Err(Attempt::Connection(reason)) => {
                // §3.5 — rebuild for next time, and fail this message now.
                self.disconnect();
                return Err(DeliveryError::Connection(format!(
                    "Connection error while sending message: {reason}"
                )));
            }
        }

        match self.exchange(message).await {
            Ok(reply) => Ok(evaluate(&reply)),
            Err(Attempt::Timeout) => {
                self.disconnect();
                // §3.4's exact wording.
                Err(DeliveryError::Connection(format!(
                    "No ACK received within {} seconds",
                    self.destination.ack_timeout.as_secs()
                )))
            }
            Err(Attempt::Connection(reason)) => {
                self.disconnect();
                Err(DeliveryError::Connection(format!(
                    "Connection error while sending message: {reason}"
                )))
            }
        }
    }
}

/// Read one MLLP frame, or time out.
///
/// §3.6 — the framing is stripped before the reply is evaluated.
async fn read_frame(stream: &mut TcpStream, timeout: Duration) -> Result<String, Attempt> {
    let mut framer = hl7_2_mllp::Framer::new();
    let mut chunk = vec![0_u8; 8192];

    loop {
        let read = match tokio::time::timeout(timeout, stream.read(&mut chunk)).await {
            Ok(Ok(0)) => {
                // The peer closed mid-reply. Not a timeout: retrying on a
                // fresh connection is right, but so is saying what
                // happened.
                return Err(Attempt::Connection(
                    "the receiver closed the connection before replying".to_string(),
                ));
            }
            Ok(Ok(read)) => read,
            Ok(Err(error)) => return Err(Attempt::Connection(error.to_string())),
            Err(_) => return Err(Attempt::Timeout),
        };

        // `read` never exceeds the buffer; one that does is a connection
        // failure, not a panic (RCA-LEN-2).
        let Some(received) = chunk.get(..read) else {
            return Err(Attempt::Connection(
                "a read reported more than the buffer".to_string(),
            ));
        };
        framer.push(received);

        match framer.next_frame() {
            Ok(Some(frame)) => {
                return String::from_utf8(frame).map_err(|_| {
                    Attempt::Connection("the acknowledgement is not valid UTF-8".to_string())
                });
            }
            Ok(None) => {}
            Err(error) => return Err(Attempt::Connection(error.to_string())),
        }
    }
}

/// An `io::Error` is a connection failure by default.
impl From<io::Error> for Attempt {
    fn from(error: io::Error) -> Self {
        Self::Connection(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_destination_renders_as_host_and_port() {
        let destination = Destination {
            host: "mpi.example".to_string(),
            port: 2575,
            ack_timeout: Duration::from_secs(30),
        };

        assert_eq!(destination.address(), "mpi.example:2575");
    }

    #[test]
    fn a_new_client_holds_no_connection() {
        // §3.1 — the connection is opened on the first send, not at
        // construction, so a receiver that is down does not stop the sender
        // from starting and reporting healthy.
        let client = MllpClient::new(Destination {
            host: "127.0.0.1".to_string(),
            port: 1,
            ack_timeout: Duration::from_secs(1),
        });

        assert!(client.stream.is_none());
        assert!(!client.is_stale(), "no connection is not a stale one");
    }
}
