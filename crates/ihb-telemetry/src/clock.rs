//! Where the current time comes from, and how it is written.
//!
//! Implements `spec/index.md` §0.7 and `spec/17-testing-and-quality.md` §5.
//!
//! Time is injected rather than read, for one reason: a golden test cannot
//! compare output that changes every run. Every timestamp in this workspace
//! comes through a [`Clock`], and a test supplies a fixed one.

use std::sync::Arc;

use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

/// The timestamp format when the microsecond component is non-zero.
///
/// The offset is written literally rather than through `[offset_hour]`: this
/// workspace only ever emits UTC, and Python writes `+00:00` rather than
/// `Z`. See `spec/index.md` §0.7.
const WITH_MICROSECONDS: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]+00:00");

/// The timestamp format when the microsecond component is exactly zero.
///
/// Python's `isoformat` drops the fractional part entirely in that case, so
/// this does too.
const WITHOUT_MICROSECONDS: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]+00:00");

/// A source of the current time.
///
/// # Examples
///
/// ```
/// use ihb_telemetry::clock::{Clock, FixedClock};
/// use time::macros::datetime;
///
/// let clock = FixedClock::at(datetime!(2026-08-20 16:09:01.991442 UTC));
/// assert_eq!(clock.now_iso8601(), "2026-08-20T16:09:01.991442+00:00");
/// ```
pub trait Clock: Send + Sync {
    /// The current moment, in UTC.
    fn now(&self) -> OffsetDateTime;

    /// The current moment, formatted as `spec/index.md` §0.7 requires.
    ///
    /// Provided rather than required, so an implementation only has to
    /// supply the instant and every caller gets the same rendering.
    fn now_iso8601(&self) -> String {
        format_iso8601(self.now())
    }
}

/// Format an instant as `spec/index.md` §0.7 requires.
///
/// # Examples
///
/// ```
/// use ihb_telemetry::clock::format_iso8601;
/// use time::macros::datetime;
///
/// assert_eq!(
///     format_iso8601(datetime!(2026-08-20 09:15:00 UTC)),
///     "2026-08-20T09:15:00+00:00",
///     "the fraction is omitted when it is zero"
/// );
/// ```
///
/// # Panics
///
/// Does not panic. A formatting failure — which the fixed format
/// descriptions above make unreachable — falls back to the same shape with
/// the fields it could render, because a telemetry timestamp is never worth
/// taking a clinical message path down.
#[must_use]
pub fn format_iso8601(moment: OffsetDateTime) -> String {
    let utc = moment.to_offset(time::UtcOffset::UTC);
    let description = if utc.microsecond() == 0 {
        WITHOUT_MICROSECONDS
    } else {
        WITH_MICROSECONDS
    };

    utc.format(description).unwrap_or_else(|_| {
        // Unreachable with the constant descriptions above; a timestamp is
        // not worth propagating an error for, so this degrades instead.
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
            utc.year(),
            u8::from(utc.month()),
            utc.day(),
            utc.hour(),
            utc.minute(),
            utc.second()
        )
    })
}

/// The real clock.
///
/// # Examples
///
/// ```
/// use ihb_telemetry::clock::{Clock, SystemClock};
///
/// let stamped = SystemClock.now_iso8601();
/// assert!(stamped.ends_with("+00:00"));
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// A clock stuck at one moment, for tests.
///
/// # Examples
///
/// ```
/// use ihb_telemetry::clock::{Clock, FixedClock};
/// use time::macros::datetime;
///
/// let clock = FixedClock::at(datetime!(2026-08-20 09:15:00 UTC));
/// assert_eq!(clock.now_iso8601(), clock.now_iso8601(), "it does not move");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    /// The moment this clock always reports.
    moment: OffsetDateTime,
}

impl FixedClock {
    /// A clock fixed at `moment`.
    #[must_use]
    pub const fn at(moment: OffsetDateTime) -> Self {
        Self { moment }
    }

    /// A clock fixed at a number of microseconds since the Unix epoch.
    ///
    /// # Panics
    ///
    /// Panics if the value is outside the range `OffsetDateTime` can
    /// represent, which is roughly the years -9999 to 9999. Test-only
    /// construction, so a panic on nonsense input is the right answer.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_telemetry::clock::{Clock, FixedClock};
    ///
    /// let clock = FixedClock::from_unix_micros(0);
    /// assert_eq!(clock.now_iso8601(), "1970-01-01T00:00:00+00:00");
    /// ```
    #[must_use]
    pub fn from_unix_micros(micros: i128) -> Self {
        let moment = OffsetDateTime::from_unix_timestamp_nanos(micros * 1_000)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        Self { moment }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.moment
    }
}

/// A clock a service can hold without caring which kind it is.
pub type SharedClock = Arc<dyn Clock>;

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn microseconds_are_six_digits_when_present() {
        // spec/index.md §0.7.2
        let moment = datetime!(2026-08-20 16:09:01.991442 UTC);
        assert_eq!(format_iso8601(moment), "2026-08-20T16:09:01.991442+00:00");
    }

    #[test]
    fn microseconds_are_omitted_when_zero() {
        // spec/index.md §0.7.3 — what Python's isoformat does.
        let moment = datetime!(2026-08-20 09:15:00 UTC);
        assert_eq!(format_iso8601(moment), "2026-08-20T09:15:00+00:00");
    }

    #[test]
    fn a_leading_zero_in_the_fraction_survives() {
        // The case a naive "trim trailing zeros" implementation gets wrong.
        let moment = datetime!(2026-08-20 09:15:00.000042 UTC);
        assert_eq!(format_iso8601(moment), "2026-08-20T09:15:00.000042+00:00");
    }

    #[test]
    fn a_trailing_zero_in_the_fraction_survives() {
        // Python pads to six digits; 1_000 nanoseconds is 1 microsecond and
        // renders as .000001, not .1 or .000001000.
        let moment = datetime!(2026-08-20 09:15:00.100000 UTC);
        assert_eq!(format_iso8601(moment), "2026-08-20T09:15:00.100000+00:00");
    }

    #[test]
    fn the_offset_is_written_out_not_abbreviated() {
        // spec/index.md §0.7.1 — `+00:00`, never `Z`.
        let rendered = format_iso8601(datetime!(2026-08-20 09:15:00 UTC));
        assert!(rendered.ends_with("+00:00"));
        assert!(!rendered.contains('Z'));
    }

    #[test]
    fn a_non_utc_instant_is_converted_rather_than_relabelled() {
        // An instant that arrives on another offset is the same moment; it
        // must be shifted, not have its offset overwritten.
        let moment = datetime!(2026-08-20 10:15:00 +1);
        assert_eq!(format_iso8601(moment), "2026-08-20T09:15:00+00:00");
    }

    #[test]
    fn a_fixed_clock_does_not_move() {
        // spec/17-testing-and-quality.md §5 — the whole reason for the trait.
        let clock = FixedClock::at(datetime!(2026-08-20 09:15:00 UTC));
        assert_eq!(clock.now_iso8601(), "2026-08-20T09:15:00+00:00");
        assert_eq!(clock.now_iso8601(), "2026-08-20T09:15:00+00:00");
    }

    #[test]
    fn the_system_clock_reports_utc() {
        assert_eq!(SystemClock.now().offset(), time::UtcOffset::UTC);
        assert!(SystemClock.now_iso8601().ends_with("+00:00"));
    }
}
