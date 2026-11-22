//! Day, Evening and Weekend.
//!
//! Implements `spec/15-dashboard.md` §5.1.1.
//!
//! # Why this needs a timezone database
//!
//! The windows are **wall-clock** rules — "Friday 17:00", "Monday 09:00" —
//! stated in UK local time. A fixed offset would be an hour wrong for the
//! seven months of British Summer Time, so an alarm configured to be quiet
//! after 17:00 would escalate at 16:00 every summer and an operator would
//! spend a long time looking for a bug in the alarm rather than in the
//! clock.
//!
//! `time-tz` carries an IANA snapshot, so BST and GMT come from the zone.

use time::{OffsetDateTime, Weekday};
use time_tz::{OffsetDateTimeExt, Tz};

/// The zone every period boundary is stated in (§5.1.1).
///
/// Not the *host's* local zone: a container in Azure runs in UTC, and the
/// rule is about the working day in Wales either way.
fn london() -> &'static Tz {
    time_tz::timezones::db::europe::LONDON
}

/// The start of the working day as (hour, minute): 09:00. Pairs compare in
/// clock order, so no minutes-past-midnight arithmetic is needed.
const DAY_START: (u8, u8) = (9, 0);

/// The end of the working day as (hour, minute): 17:00.
const DAY_END: (u8, u8) = (17, 0);

/// Which of §5.1.1's three windows a moment falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Period {
    /// Monday–Friday 09:00–17:00.
    Day,
    /// Monday–Friday outside the day window, excluding the weekend window.
    Evening,
    /// Friday 17:00 → Monday 09:00.
    Weekend,
}

impl Period {
    /// The label the Python shows, for a page and an e-mail.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Day => "Day (Mon–Fri 09:00–17:00 UK time)",
            Self::Evening => "Evening (Mon–Fri 17:00–09:00 UK time; excluding Weekend)",
            Self::Weekend => "Weekend (Fri 17:00–Mon 09:00 UK time)",
        }
    }

    /// The short label, for a table cell.
    #[must_use]
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Day => "Day",
            Self::Evening => "Evening",
            Self::Weekend => "Weekend",
        }
    }

    /// Whether e-mail escalation is out of hours in this period (§5.2.1).
    ///
    /// Day is in hours; Evening and Weekend are not.
    #[must_use]
    pub const fn is_out_of_hours(self) -> bool {
        !matches!(self, Self::Day)
    }
}

/// Which period `at` falls in (§5.1.1).
///
/// # Examples
///
/// A Wednesday morning in winter, and the same clock time in summer — both
/// are Day, which is the whole reason this reads the zone rather than an
/// offset:
///
/// ```
/// use dashboard::period::{period_at, Period};
/// use time::macros::datetime;
///
/// // 10:00 UTC in January is 10:00 GMT.
/// assert_eq!(period_at(datetime!(2026-01-14 10:00 UTC)), Period::Day);
///
/// // 10:00 UTC in July is 11:00 BST — still inside the working day.
/// assert_eq!(period_at(datetime!(2026-07-15 10:00 UTC)), Period::Day);
///
/// // But 16:30 UTC in July is 17:30 BST, which is Evening.
/// assert_eq!(period_at(datetime!(2026-07-15 16:30 UTC)), Period::Evening);
/// ```
///
/// The weekend runs from Friday evening to Monday morning:
///
/// ```
/// use dashboard::period::{period_at, Period};
/// use time::macros::datetime;
///
/// assert_eq!(period_at(datetime!(2026-01-16 16:59 UTC)), Period::Day);     // Fri
/// assert_eq!(period_at(datetime!(2026-01-16 17:00 UTC)), Period::Weekend); // Fri
/// assert_eq!(period_at(datetime!(2026-01-19 08:59 UTC)), Period::Weekend); // Mon
/// assert_eq!(period_at(datetime!(2026-01-19 09:00 UTC)), Period::Day);     // Mon
/// ```
#[must_use]
pub fn period_at(at: OffsetDateTime) -> Period {
    let local = at.to_timezone(london());
    let weekday = local.weekday();
    let minutes = (local.hour(), local.minute());

    // §5.1.1's weekend window: Friday 17:00 → Monday 09:00. Tested before
    // the day window, because Friday 17:00 belongs to both descriptions and
    // the weekend is the one that wins.
    let weekend = match weekday {
        Weekday::Friday => minutes >= DAY_END,
        Weekday::Saturday | Weekday::Sunday => true,
        Weekday::Monday => minutes < DAY_START,
        _ => false,
    };
    if weekend {
        return Period::Weekend;
    }

    if (DAY_START..DAY_END).contains(&minutes) {
        Period::Day
    } else {
        Period::Evening
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn the_weekend_runs_from_friday_evening_to_monday_morning() {
        // spec/15-dashboard.md §5.1.1 and §8.7 — both sides of each edge.
        // January, so UTC and London agree and the boundary is unambiguous.

        // Friday.
        assert_eq!(period_at(datetime!(2026-01-16 16:59 UTC)), Period::Day);
        assert_eq!(period_at(datetime!(2026-01-16 17:00 UTC)), Period::Weekend);

        // Straight through the weekend.
        assert_eq!(period_at(datetime!(2026-01-17 12:00 UTC)), Period::Weekend);
        assert_eq!(period_at(datetime!(2026-01-18 12:00 UTC)), Period::Weekend);

        // Monday.
        assert_eq!(period_at(datetime!(2026-01-19 08:59 UTC)), Period::Weekend);
        assert_eq!(period_at(datetime!(2026-01-19 09:00 UTC)), Period::Day);
    }

    #[test]
    fn the_working_day_runs_nine_to_five() {
        // §8.7 — both sides, on a Wednesday so no weekend rule applies.
        assert_eq!(period_at(datetime!(2026-01-14 08:59 UTC)), Period::Evening);
        assert_eq!(period_at(datetime!(2026-01-14 09:00 UTC)), Period::Day);
        assert_eq!(period_at(datetime!(2026-01-14 16:59 UTC)), Period::Day);
        assert_eq!(period_at(datetime!(2026-01-14 17:00 UTC)), Period::Evening);
    }

    #[test]
    fn midnight_on_a_weekday_is_evening() {
        // The window that has no name of its own: Mon–Fri 17:00–09:00 wraps
        // midnight, so a Tuesday at 03:00 is the *Monday* evening window.
        assert_eq!(period_at(datetime!(2026-01-13 00:00 UTC)), Period::Evening);
        assert_eq!(period_at(datetime!(2026-01-13 03:00 UTC)), Period::Evening);
    }

    #[test]
    fn british_summer_time_moves_the_boundary_in_utc() {
        // §8.7 — THE test of this file. The rule is a wall-clock rule, so
        // in BST every boundary sits an hour earlier in UTC. An
        // implementation using a fixed offset passes every test above and
        // fails every one of these, for seven months of the year.

        // A Wednesday in July. 16:00 UTC is 17:00 BST: the day has ended.
        assert_eq!(period_at(datetime!(2026-07-15 15:59 UTC)), Period::Day);
        assert_eq!(period_at(datetime!(2026-07-15 16:00 UTC)), Period::Evening);

        // And the morning boundary moves with it: 08:00 UTC is 09:00 BST.
        assert_eq!(period_at(datetime!(2026-07-15 07:59 UTC)), Period::Evening);
        assert_eq!(period_at(datetime!(2026-07-15 08:00 UTC)), Period::Day);

        // The same Friday-evening edge, an hour earlier in UTC.
        assert_eq!(period_at(datetime!(2026-07-17 15:59 UTC)), Period::Day);
        assert_eq!(period_at(datetime!(2026-07-17 16:00 UTC)), Period::Weekend);
    }

    #[test]
    fn the_transitions_themselves_are_classified() {
        // The two Sundays a year the clocks change. Both are Weekend
        // whatever the offset does, which makes them a safe check that the
        // conversion does not panic or skip on a discontinuity.

        // 2026-03-29 01:00 UTC: GMT → BST, the hour that does not exist.
        assert_eq!(period_at(datetime!(2026-03-29 00:59 UTC)), Period::Weekend);
        assert_eq!(period_at(datetime!(2026-03-29 01:00 UTC)), Period::Weekend);
        assert_eq!(period_at(datetime!(2026-03-29 02:00 UTC)), Period::Weekend);

        // 2026-10-25 01:00 UTC: BST → GMT, the hour that happens twice.
        assert_eq!(period_at(datetime!(2026-10-25 00:59 UTC)), Period::Weekend);
        assert_eq!(period_at(datetime!(2026-10-25 01:00 UTC)), Period::Weekend);
    }

    #[test]
    fn a_monday_in_bst_still_starts_at_nine_local() {
        // The Monday-morning edge and the BST shift together, which is the
        // combination an off-by-one hour would slip through.
        assert_eq!(period_at(datetime!(2026-07-20 07:59 UTC)), Period::Weekend);
        assert_eq!(period_at(datetime!(2026-07-20 08:00 UTC)), Period::Day);
    }

    #[test]
    fn only_the_day_is_in_hours() {
        // §5.2.1 — the guard suppresses e-mail in Evening and Weekend.
        assert!(!Period::Day.is_out_of_hours());
        assert!(Period::Evening.is_out_of_hours());
        assert!(Period::Weekend.is_out_of_hours());
    }

    #[test]
    fn every_period_has_both_labels() {
        for period in [Period::Day, Period::Evening, Period::Weekend] {
            assert!(!period.label().is_empty());
            assert!(!period.short_label().is_empty());
            assert!(
                period.label().starts_with(period.short_label()),
                "{period:?}"
            );
        }
    }

    #[test]
    fn a_non_utc_input_is_converted_rather_than_read_as_local() {
        // A timestamp arriving from Log Analytics may carry any offset. It
        // is a moment in time, not a wall clock, so it has to be converted.
        //
        // 2026-01-14 20:00 +11:00 is 09:00 UTC, a Wednesday morning: Day.
        let far_east = datetime!(2026-01-14 20:00 +11:00);
        assert_eq!(period_at(far_east), Period::Day);
    }
}
