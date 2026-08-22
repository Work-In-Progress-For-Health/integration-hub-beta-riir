//! Splitting a batch to fit inside TDS's parameter limit.
//!
//! Implements `spec/12-message-store.md` §5.6.

/// The most parameters TDS accepts in one statement.
pub const MAX_PARAMETERS: usize = 2100;

/// Parameters per row of `monitoring.Message`
/// (`spec/12-message-store.md` §2.1).
pub const PARAMETERS_PER_ROW: usize = 9;

/// Rows per statement: `2100 / 9`, floored.
pub const ROWS_PER_STATEMENT: usize = MAX_PARAMETERS / PARAMETERS_PER_ROW;

/// Split `rows` into runs that each fit one statement.
///
/// All the statements share one transaction, so the split is invisible to
/// the caller's all-or-nothing semantics (§1.1).
///
/// # Why this exists when it never triggers
///
/// `MAX_BATCH_SIZE` defaults to 100 and the limit is 233, so a default
/// deployment never reaches it. §5.6 requires the limit be **enforced
/// rather than assumed**, because `MAX_BATCH_SIZE` is an environment
/// variable: an operator raising it to 500 to drain a backlog would
/// otherwise get a TDS error with no obvious cause, at the worst possible
/// moment. See [`plan.md` §9.6](../plan.md).
///
/// # Examples
///
/// ```
/// use ihb_sql::chunk::{chunks, ROWS_PER_STATEMENT};
///
/// assert_eq!(ROWS_PER_STATEMENT, 233);
///
/// let rows: Vec<u32> = (0..500).collect();
/// let split: Vec<usize> = chunks(&rows).map(<[u32]>::len).collect();
///
/// assert_eq!(split, [233, 233, 34]);
/// ```
///
/// A batch that fits is one chunk, and an empty batch is none:
///
/// ```
/// use ihb_sql::chunk::chunks;
///
/// let rows: Vec<u32> = (0..100).collect();
/// assert_eq!(chunks(&rows).count(), 1);
///
/// assert_eq!(chunks(&[] as &[u32]).count(), 0);
/// ```
pub fn chunks<T>(rows: &[T]) -> impl Iterator<Item = &[T]> {
    rows.chunks(ROWS_PER_STATEMENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arithmetic_is_the_specified_one() {
        // spec/12-message-store.md §5.6.
        assert_eq!(MAX_PARAMETERS, 2100);
        assert_eq!(PARAMETERS_PER_ROW, 9);
        assert_eq!(ROWS_PER_STATEMENT, 233);

        // And the reason for the floor: 234 rows would need 2106. A const
        // block, because these are constants — the assertion is a
        // compile-time claim about the arithmetic rather than a runtime
        // check, and clippy is right to say so.
        const {
            assert!(ROWS_PER_STATEMENT * PARAMETERS_PER_ROW <= MAX_PARAMETERS);
            assert!((ROWS_PER_STATEMENT + 1) * PARAMETERS_PER_ROW > MAX_PARAMETERS);
        }
    }

    #[test]
    fn the_boundary_rows_of_section_six_three() {
        // §6.3 asks for exactly these three.
        let at_232: Vec<u32> = (0..232).collect();
        assert_eq!(chunks(&at_232).count(), 1);

        let at_233: Vec<u32> = (0..233).collect();
        assert_eq!(chunks(&at_233).count(), 1, "233 still fits one statement");

        let at_234: Vec<u32> = (0..234).collect();
        let split: Vec<usize> = chunks(&at_234).map(<[u32]>::len).collect();
        assert_eq!(split, [233, 1], "234 does not");
    }

    #[test]
    fn every_row_appears_exactly_once() {
        // A split that dropped or duplicated a row would lose a patient
        // record, or store one twice, and neither is visible from the
        // chunk count.
        let rows: Vec<u32> = (0..1000).collect();

        let rejoined: Vec<u32> = chunks(&rows).flatten().copied().collect();
        assert_eq!(rejoined, rows);
    }

    #[test]
    fn an_empty_batch_produces_no_statements() {
        // Rather than one statement with no values, which is a syntax
        // error.
        assert_eq!(chunks(&[] as &[u32]).count(), 0);
    }
}
