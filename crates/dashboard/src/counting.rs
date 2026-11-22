//! Counting and numbering without `+`.
//!
//! Implements `spec/15-dashboard.md` §7 (checked arithmetic). The counts here
//! are tallies for a summary, and row numbers for a message; neither can
//! reach `usize::MAX` (a file with that many rows does not fit in memory), so
//! the declared fallback of holding there is unreachable and never a wrong
//! answer.

/// One more than `count`, holding at `usize::MAX` (`§7`).
#[must_use]
pub const fn next(count: usize) -> usize {
    match count.checked_add(1) {
        Some(next) => next,
        None => usize::MAX, // §7
    }
}

/// The spreadsheet row of the 0-based `index`: row 1 is the header.
#[must_use]
pub const fn row_number(index: usize) -> usize {
    next(next(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_step_by_one_and_hold_at_the_top() {
        assert_eq!(next(0), 1);
        assert_eq!(next(usize::MAX - 1), usize::MAX);
        assert_eq!(next(usize::MAX), usize::MAX);
    }

    #[test]
    fn the_first_data_row_is_row_two() {
        assert_eq!(row_number(0), 2);
        assert_eq!(row_number(usize::MAX), usize::MAX);
    }
}
