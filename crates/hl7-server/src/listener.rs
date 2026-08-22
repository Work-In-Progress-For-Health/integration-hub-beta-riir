//! The MLLP socket.
//!
//! Implements `spec/07-hl7-server.md` §2 and the shutdown of §8.
//!
//! Framing is [`hl7_2_mllp::Framer`]; everything here is what to do with
//! what it produces, and when to hang up.
//!
//! # Closing is how failure is reported
//!
//! MLLP has no error frame. §4.3 makes the absence of a reply the signal
//! that a message was not taken, so nearly every failure path in this
//! module ends by returning and letting the connection drop. That is not
//! giving up — it is the protocol's only way to say "send it again".

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use hl7_2_mllp::{Error as FramingError, Framer};
use ihb_bus::sink::MessageSink;
use ihb_runtime::shutdown::Watcher;
use ihb_telemetry::events::EventLogger;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::pipeline::{Mint, Pipeline};

/// How long a connection may be silent before it is closed
/// (`spec/07-hl7-server.md` §2.6, `size_limited_mllp_server.py`,
/// `timeout=10`).
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of an oversized message goes into the `MESSAGE_FAILED` event.
///
/// Normative: `size_limited_mllp_request_handler.py ::
/// MAX_PARTIAL_MESSAGE_LOG_SIZE` (`spec/07-hl7-server.md` §2.3). Enough to
/// identify the sender and the message type, bounded so that a peer cannot
/// make this process log a gigabyte.
pub const MAX_PARTIAL_MESSAGE_LOG_SIZE: usize = 1000;

/// The `validation_result` on the oversize event (§2.3).
const SIZE_LIMIT_EXCEEDED: &str = "Message size limit exceeded";

/// The `validation_result` when a frame is not UTF-8 (§2.2).
const INVALID_ENCODING: &str = "Invalid message encoding";

/// Accept connections until the watcher says stop.
///
/// Each connection is served on its own task, so a slow sender does not
/// hold up the rest. `spec/07-hl7-server.md` §8.3 requires in-flight
/// messages to complete, so shutdown stops *accepting* and then waits;
/// this function returns once the accept loop is done, and the caller
/// decides how long to wait for the tasks it spawned.
///
/// # Errors
///
/// [`io::Error`] if the address cannot be bound. A failure to bind is a
/// startup failure: there is no useful degraded mode for a server that
/// cannot listen.
pub async fn serve<E, S, M>(
    address: SocketAddr,
    pipeline: std::sync::Arc<Pipeline<E, S, M>>,
    watcher: Watcher,
) -> io::Result<()>
where
    E: MessageSink + 'static,
    S: MessageSink + 'static,
    M: Mint + 'static,
{
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr().unwrap_or(address);
    tracing::info!(%bound, "MLLP listener started");

    let mut connections = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            // Biased so that a pending shutdown wins over a ready
            // connection. Without it a busy port could keep accepting
            // indefinitely while the process is trying to stop.
            biased;

            () = watcher.wait() => break,

            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let pipeline = std::sync::Arc::clone(&pipeline);
                    connections.spawn(async move {
                        if let Err(error) = handle_connection(stream, &pipeline).await {
                            // An I/O error on one connection is that
                            // connection's problem. The peer will retry.
                            tracing::warn!(%peer, %error, "connection ended with an error");
                        }
                    });
                }
                Err(error) => {
                    // A failed accept is usually a per-connection problem
                    // (the peer vanished between the SYN and here), not a
                    // reason to stop serving.
                    tracing::warn!(%error, "could not accept a connection");
                }
            },
        }
    }

    tracing::info!("MLLP listener stopped accepting; draining in-flight messages");
    // §8.3 — a message that has been acknowledged must have been forwarded,
    // so in-flight work is awaited rather than aborted. Dropping the
    // `JoinSet` here would cancel exactly the messages most at risk.
    while connections.join_next().await.is_some() {}
    tracing::info!("MLLP listener drained");

    Ok(())
}

/// Serve one connection until it closes or misbehaves.
async fn handle_connection<E, S, M>(
    mut stream: TcpStream,
    pipeline: &Pipeline<E, S, M>,
) -> io::Result<()>
where
    E: MessageSink,
    S: MessageSink,
    M: Mint,
{
    let limit = pipeline.config().max_message_size;
    let events = EventLogger::new(
        &pipeline.config().workflow_id,
        &pipeline.config().microservice_id,
    )
    .with_redaction(pipeline.config().redaction_enabled);

    let mut framer = Framer::new().with_limit(limit);
    // The framer's buffer is not readable, and §2.3 needs the bytes that
    // accumulated in order to log the first thousand of them. This mirrors
    // it: after every extraction it is trimmed to whatever the framer still
    // holds, so the two stay in step without guessing at frame widths.
    let mut accumulated: Vec<u8> = Vec::new();
    let mut chunk = vec![0_u8; 8192];

    loop {
        let read = match tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk)).await {
            Ok(Ok(0)) => {
                // §2.7 — the peer closed. If a frame was half-arrived, say
                // so: that message is lost and nobody else will report it.
                if !framer.is_empty() {
                    tracing::warn!(
                        buffered = framer.buffered(),
                        "peer closed mid-frame; the partial message is lost"
                    );
                }
                return Ok(());
            }
            Ok(Ok(read)) => read,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                // §2.6 — a silent connection is closed.
                tracing::debug!(seconds = READ_TIMEOUT.as_secs(), "read timed out; closing");
                return Ok(());
            }
        };

        accumulated.extend_from_slice(&chunk[..read]);

        // §2.3 — checked before framing, so the limit is this server's
        // rather than the framer's, and so the accumulated bytes are still
        // here to be logged.
        if accumulated.len() > limit {
            report_oversize(&events, &accumulated, limit);
            return Ok(());
        }

        framer.push(&chunk[..read]);

        // §2.5 — every frame in this read gets its own reply. The Python
        // read until its buffer *ended* with <FS><CR> and so answered two
        // frames once; see plan.md §9.1.
        loop {
            match framer.next_frame() {
                Ok(Some(frame)) => {
                    // Keep `accumulated` aligned with what the framer still
                    // holds, which is exactly its tail.
                    let consumed = accumulated.len().saturating_sub(framer.buffered());
                    accumulated.drain(..consumed);

                    if !serve_frame(&mut stream, pipeline, &events, &frame).await? {
                        return Ok(());
                    }
                }
                Ok(None) => break,
                Err(FramingError::LeadingBytes(count)) => {
                    // §2.4 — closed without a reply **and without an
                    // event**. This is a peer speaking something that is
                    // not MLLP, not a message that failed; logging it as a
                    // message failure would fill the event stream with
                    // port scans.
                    tracing::debug!(count, "bytes outside a frame; closing");
                    return Ok(());
                }
                Err(error) => {
                    tracing::error!(%error, "MLLP framing failed; closing");
                    return Ok(());
                }
            }
        }
    }
}

/// Run one frame through the pipeline and write whatever comes back.
///
/// Returns whether the connection should continue.
async fn serve_frame<E, S, M>(
    stream: &mut TcpStream,
    pipeline: &Pipeline<E, S, M>,
    events: &EventLogger,
    frame: &[u8],
) -> io::Result<bool>
where
    E: MessageSink,
    S: MessageSink,
    M: Mint,
{
    // §2.2 — invalid UTF-8 is a rejection, and the connection closes.
    let Ok(text) = std::str::from_utf8(frame) else {
        // Lossily, because the point of the event is to show what arrived,
        // and what arrived is by definition not decodable.
        let lossy = String::from_utf8_lossy(frame);
        tracing::error!("message is not valid UTF-8; closing the connection");
        events.log_message_failed(
            &lossy,
            "message is not valid UTF-8",
            Some(INVALID_ENCODING),
            None,
        );
        return Ok(false);
    };

    // §0.6 — a `\n` in received text is normalised to `\r` before parsing.
    // Some senders write `\r\n`; collapsing the pair first stops that
    // becoming an empty segment.
    let normalised = text.replace("\r\n", "\r").replace('\n', "\r");

    match pipeline.handle(&normalised).await {
        Ok(acknowledgement) => {
            stream
                .write_all(&hl7_2_mllp::encode(acknowledgement.as_bytes()))
                .await?;
            stream.flush().await?;
            Ok(true)
        }
        Err(rejection) => {
            // §4.3 — no reply at all. The pipeline has already logged and
            // emitted; there is nothing to add and nothing to send.
            tracing::debug!(reason = %rejection.reason, "no acknowledgement sent");
            Ok(false)
        }
    }
}

/// §2.3 — the log line and the event for an oversized message.
fn report_oversize(events: &EventLogger, accumulated: &[u8], limit: usize) {
    let size = accumulated.len();

    // The wording is normative — operators match on it.
    tracing::error!(
        "Message size ({size} bytes) exceeds maximum allowed size ({limit} bytes). \
         Connection will be closed."
    );

    let partial = partial_for_log(accumulated);

    events.log_message_failed(
        &partial,
        &format!("Message size ({size} bytes) exceeds maximum allowed size ({limit} bytes)"),
        Some(SIZE_LIMIT_EXCEEDED),
        None,
    );
}

/// The first [`MAX_PARTIAL_MESSAGE_LOG_SIZE`] **bytes**, decoded lossily.
///
/// `spec/07-hl7-server.md` §2.3 says bytes, not characters, and the count
/// is normative. Cutting a byte slice can land in the middle of a
/// multi-byte character; `from_utf8_lossy` renders the fragment as U+FFFD
/// rather than failing, which is the "decoded lossily" the clause asks for.
///
/// Extracted from [`report_oversize`] so the cap can be tested without a
/// tracing subscriber. It is also the shape of a bug found by fuzzing
/// elsewhere in this workspace (`spec/04-observability.md` §2.5a): slicing
/// a byte buffer at a fixed offset and handing the result to something that
/// expects a `str`. Doing it through `from_utf8_lossy` cannot panic for any
/// input, which is the property the tests below pin.
///
/// # The result can be longer than the cap
///
/// The cap is on the **input**, which is what §2.3 says. Decoding lossily
/// can make the output grow: every byte that is not valid UTF-8 becomes
/// U+FFFD, which is three bytes. A thousand invalid bytes therefore render
/// as three thousand. That is the clause working as written rather than a
/// defect, but anyone sizing a log budget from `MAX_PARTIAL_MESSAGE_LOG_SIZE`
/// should know the real ceiling is three times it.
fn partial_for_log(accumulated: &[u8]) -> String {
    let cut = accumulated.len().min(MAX_PARTIAL_MESSAGE_LOG_SIZE);
    String::from_utf8_lossy(&accumulated[..cut]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_partial_log_is_capped_at_a_thousand_bytes() {
        // spec/07-hl7-server.md §2.3 — the cap bounds what a peer can make
        // this process log, so it is asserted rather than assumed.
        let huge = vec![b'A'; 100_000];
        assert_eq!(partial_for_log(&huge).len(), MAX_PARTIAL_MESSAGE_LOG_SIZE);
    }

    #[test]
    fn a_short_message_is_not_padded_or_truncated() {
        assert_eq!(partial_for_log(b"MSH|^~\\&|252"), "MSH|^~\\&|252");
        assert_eq!(partial_for_log(b""), "");
    }

    #[test]
    fn cutting_through_a_multi_byte_character_does_not_panic() {
        // The same shape as the defect fuzzing found in
        // `ihb_telemetry::redact` (`spec/04-observability.md` §2.5a): a
        // fixed byte offset into text that is not ASCII. Here the cut
        // always lands mid-character, for every alignment.
        for offset in 0..4 {
            let mut bytes = vec![b'A'; MAX_PARTIAL_MESSAGE_LOG_SIZE - offset];
            // A four-byte character, straddling the thousandth byte.
            bytes.extend_from_slice("🏥".as_bytes());
            bytes.extend_from_slice(b"trailing");

            let partial = partial_for_log(&bytes);

            // The cap is on the input, so the *output* may be longer: the
            // truncated fragment of the emoji becomes U+FFFD, three bytes
            // in place of one or two. Asserting the output length would be
            // asserting something §2.3 does not say — the real guarantees
            // are that no more than the cap was read, and that the result
            // is valid text.
            assert!(
                partial.starts_with(&"A".repeat(MAX_PARTIAL_MESSAGE_LOG_SIZE - offset)),
                "offset {offset}: everything before the cut survives"
            );
            assert!(
                !partial.contains("trailing"),
                "offset {offset}: nothing past the cap is read"
            );
        }
    }

    #[test]
    fn the_output_is_bounded_even_for_the_worst_input() {
        // Worst case: every byte invalid, so every one becomes a three-byte
        // U+FFFD. This is the actual ceiling on how much a peer can make a
        // single log line carry, and it is three times the cap rather than
        // the cap. Documented on `partial_for_log`.
        let all_invalid = vec![0xFF_u8; 100_000];
        let partial = partial_for_log(&all_invalid);

        assert_eq!(partial.chars().count(), MAX_PARTIAL_MESSAGE_LOG_SIZE);
        assert_eq!(partial.len(), MAX_PARTIAL_MESSAGE_LOG_SIZE * 3);
    }

    #[test]
    fn arbitrary_bytes_are_never_a_panic() {
        // A peer can send anything at all before the size limit trips, and
        // this runs on the failure path — the worst place for a second
        // failure.
        for byte in 0..=255_u8 {
            let bytes = vec![byte; MAX_PARTIAL_MESSAGE_LOG_SIZE + 7];
            let _ = partial_for_log(&bytes);
        }
    }

    #[test]
    fn the_partial_log_size_is_the_pythons() {
        // spec/07-hl7-server.md §2.3 — normative, from
        // `size_limited_mllp_request_handler.py`.
        assert_eq!(MAX_PARTIAL_MESSAGE_LOG_SIZE, 1000);
    }

    #[test]
    fn the_read_timeout_is_the_pythons() {
        // spec/07-hl7-server.md §2.6 — `timeout=10`.
        assert_eq!(READ_TIMEOUT, Duration::from_secs(10));
    }
}
