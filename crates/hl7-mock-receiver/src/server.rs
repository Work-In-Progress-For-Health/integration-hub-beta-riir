//! The MLLP listener.
//!
//! Implements `spec/11-mock-receivers.md` §2.1, §2.2 and §2.3.
//!
//! One task per connection, each holding a [`Framer`] so a message split
//! across reads — or several arriving in one — is reassembled correctly.
//! The Python could not do the second: it read until the buffer *ended*
//! with `<FS><CR>`, so two pipelined frames were decoded as one blob
//! (`plan.md` §9.1).

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use hl7_2_mllp::{AckCode, Framer};
use ihb_bus::message::Outgoing;
use ihb_bus::sink::MessageSink;
use ihb_runtime::shutdown::Watcher;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::ack;

/// How much to read at once. Messages are small; this only bounds syscalls.
const READ_BUFFER: usize = 8 * 1024;

/// What a connection needs to answer a frame.
///
/// Grouped into one type so `serve` takes a handful of arguments rather
/// than threading four of them through every function.
pub struct Receiver<S> {
    /// Where a received message is forwarded.
    sink: S,
    /// `EGRESS_SESSION_ID`, when the queue is session-enabled.
    session_id: Option<String>,
    /// The framer's limit.
    max_message_size: usize,
}

impl<S: MessageSink> Receiver<S> {
    /// A receiver forwarding to `sink`.
    pub const fn new(sink: S, session_id: Option<String>, max_message_size: usize) -> Self {
        Self {
            sink,
            session_id,
            max_message_size,
        }
    }
}

/// Accept connections until the watcher says stop.
///
/// # Errors
///
/// [`io::Error`] if the address cannot be bound.
pub async fn serve<S>(
    address: SocketAddr,
    receiver: Arc<Receiver<S>>,
    watcher: Watcher,
) -> io::Result<()>
where
    S: MessageSink + 'static,
{
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr().unwrap_or(address);
    tracing::info!(%bound, queue = %receiver.sink.entity(), "mock receiver listening");

    let mut connections = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            biased;

            () = watcher.wait() => break,

            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let receiver = Arc::clone(&receiver);
                    connections.spawn(async move {
                        tracing::debug!(%peer, "connection opened");
                        if let Err(error) = handle(stream, &receiver).await {
                            tracing::warn!(%peer, %error, "connection ended with an error");
                        }
                        tracing::debug!(%peer, "connection closed");
                    });
                }
                Err(error) => tracing::warn!(%error, "could not accept a connection"),
            },
        }
    }

    tracing::info!("no longer accepting connections");
    // A message that has been acknowledged was forwarded first, so
    // in-flight work is awaited rather than cancelled.
    while connections.join_next().await.is_some() {}

    Ok(())
}

/// Read frames from one connection and answer each in turn.
async fn handle<S: MessageSink>(mut stream: TcpStream, receiver: &Receiver<S>) -> io::Result<()> {
    let mut framer = Framer::new().with_limit(receiver.max_message_size);
    let mut buffer = vec![0_u8; READ_BUFFER];

    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        framer.push(&buffer[..read]);

        loop {
            match framer.next_frame() {
                Ok(Some(payload)) => {
                    let reply = respond(&payload, receiver).await;
                    stream.write_all(&reply).await?;
                    stream.flush().await?;
                }
                Ok(None) => break,
                Err(error) => {
                    // Framing is broken for the rest of this stream: the
                    // receiver can no longer tell where a message starts,
                    // so the only safe move is to hang up.
                    tracing::warn!(%error, "framing error; closing the connection");
                    return Ok(());
                }
            }
        }
    }
}

/// Forward one message and build its reply.
///
/// Forwarding happens **first**, deliberately. `spec/11-mock-receivers.md`
/// §2.3.1: an acknowledgement that says `AA` when nothing was forwarded is
/// a lie, and a test that then waits on the queue hangs rather than fails.
/// That is the same rule `hl7-server` follows at `spec/07` §5.3, for the
/// same reason.
async fn respond<S: MessageSink>(payload: &[u8], receiver: &Receiver<S>) -> Vec<u8> {
    // The same `MSH-7` `hl7-server` writes, from the same function:
    // spec/11-mock-receivers.md §2.4 makes this field spec/07 §4's.
    let timestamp = ihb_fields::timestamp::timestamp();
    let control_id = control_id(payload);
    tracing::info!(control_id = %control_id, bytes = payload.len(), "message received");

    // §1.1 — the `fail` trigger, checked before forwarding so a test can
    // drive the unhappy path without a broker refusing anything.
    if ack::should_reject(payload) {
        tracing::info!(control_id = %control_id, "rejecting on request");
        return ack::reply(payload, AckCode::Error, &timestamp);
    }

    let message = Outgoing::new(payload.to_vec())
        .with_message_id(control_id.clone())
        .with_optional_session(receiver.session_id.as_deref());

    match receiver.sink.send(message).await {
        Ok(()) => {
            tracing::info!(control_id = %control_id, "forwarded");
            ack::reply(payload, AckCode::Accept, &timestamp)
        }
        Err(error) => {
            tracing::error!(control_id = %control_id, %error, "could not forward; rejecting");
            ack::reply(payload, AckCode::Error, &timestamp)
        }
    }
}

/// `MSH-10`, or empty when the payload is not a message this can read.
fn control_id(payload: &[u8]) -> String {
    hl7_2_mllp::ack::parse(payload)
        .ok()
        .and_then(|message| message.get("MSH-10").ok().flatten())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_control_id_out_of_a_message() {
        let message = b"MSH|^~\\&|A|B|C|D|20250101||ADT^A01|CTRL9|P|2.5\r";
        assert_eq!(control_id(message), "CTRL9");
        assert_eq!(control_id(b"nonsense"), "");
    }
}
