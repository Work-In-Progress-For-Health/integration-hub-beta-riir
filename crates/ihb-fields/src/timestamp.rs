//! `MSH-7` for a message this workspace generates.
//!
//! Implements the `MSH-7` row of `spec/07-hl7-server.md` §4, and through
//! `spec/11-mock-receivers.md` §2.4 the mock receiver's too.
//!
//! It lives here rather than in either service because both build
//! acknowledgements and both need this exact format. One normative clause
//! implemented twice is the shape that drifts — the same reason
//! `pipe_wrap` ended up in this crate rather than in two.

use std::sync::OnceLock;

use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

/// The process's local UTC offset, captured before any thread is spawned.
///
/// See [`capture_local_offset`] for why this exists at all.
static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Determine and remember the local UTC offset. Call once, from `main`,
/// **before starting the async runtime**.
///
/// # Why this is not just `OffsetDateTime::now_local()`
///
/// Reading the system timezone on Unix goes through `localtime_r`, which
/// other threads can race by calling `setenv("TZ")`. The `time` crate will
/// not take that risk on a caller's behalf, so
/// `UtcOffset::current_local_offset` returns `Err(IndeterminateOffset)` once
/// the process is multi-threaded — which a Tokio worker pool always is.
/// Calling `now_local()` from a request handler therefore does not give
/// local time; it gives an error that quietly degrades to UTC on every
/// single message.
///
/// So the offset is read at the one moment it can be read safely — process
/// start, single-threaded — and reused. That is the standard remedy, and it
/// is also *correct* for this use: a server does not want its ACK timestamps
/// to shift halfway through a run because someone changed the container's
/// timezone.
///
/// Returns whether an offset was determined. `false` means UTC will be used,
/// which is the right answer in the estate's containers anyway — they set no
/// `TZ`, so local *is* UTC — but a caller may want to log the distinction.
pub fn capture_local_offset() -> bool {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    LOCAL_OFFSET.set(offset).is_ok() && offset != UtcOffset::UTC
}

/// `MSH-7` for a message generated now.
///
/// **Local wall-clock time, not UTC**, formatted `%Y%m%d%H%M%S`
/// (`spec/index.md` §0.7). That is deliberate, and it is not the rule
/// elsewhere in this workspace: §0.7 requires UTC for telemetry, the message
/// store and bus metadata, and this is none of those. It is a field inside
/// an HL7 message, and the Python writes a naive `datetime.now()`
/// (`hl7_ack_builder.py`). A source system comparing this against its own
/// clock would see an offset appear if it were changed to UTC.
///
/// "Local" means the offset [`capture_local_offset`] found at startup, or
/// UTC if it was never called — as it is not in tests. In the estate's
/// containers the two are the same, because no `TZ` is set.
#[must_use]
pub fn timestamp() -> String {
    format_hl7(
        OffsetDateTime::now_utc().to_offset(LOCAL_OFFSET.get().copied().unwrap_or(UtcOffset::UTC)),
    )
}

/// A moment as HL7 writes one: fourteen digits, no separators, no zone.
fn format_hl7(at: OffsetDateTime) -> String {
    const HL7: &[time::format_description::FormatItem<'_>] =
        format_description!("[year][month][day][hour][minute][second]");

    // The format has no conditional parts and every component is present, so
    // this cannot fail. The fallback is here rather than an `expect` because
    // §4.3 gives "no reply" a specific meaning — "we did not take your
    // message" — and a panic on this line would send exactly that signal
    // about a message that was, in fact, accepted and forwarded.
    at.format(&HL7)
        .unwrap_or_else(|_| "19700101000000".to_string())
}
