//! The statements, and the chunking the mark needs.
//!
//! Implements `spec/13-message-replay.md` §3, §6.

/// The fetch, exactly as §3 gives it.
///
/// Normative, and two details carry weight:
///
/// - **`READPAST`** (§3.1) skips rows another worker has locked, so two
///   workers on the same replay batch do not block each other. Without it a
///   second worker waits on the first's transaction for no benefit.
/// - **`ORDER BY ReplayId` throughout** (§3.3) — in the CTE *and* the outer
///   select. Replay preserves the order messages were stored in, and a
///   clinical feed replayed out of order can apply an older update over a
///   newer one.
pub const FETCH_SQL: &str = "WITH Batch AS (
    SELECT TOP (@P1) ReplayId, MessageId
    FROM monitoring.MessageReplayQueue WITH (READPAST)
    WHERE Status IN ('Failed', 'Pending')
    AND ReplayBatchId = @P2
    ORDER BY ReplayId
)
SELECT b.ReplayId, m.Id AS MessageId, m.RawPayload, m.CorrelationId, m.SessionId
FROM Batch b
JOIN monitoring.Message m ON m.Id = b.MessageId
ORDER BY b.ReplayId;";

/// The status of a replay row (§2.1).
///
/// Exactly these three, case-sensitive. A `Failed` row is picked up again
/// by a later run (§2.2), which makes re-running the job the whole recovery
/// procedure — there is nothing else to do and nothing else to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not yet attempted.
    Pending,
    /// Attempted and failed. Picked up again by the next run.
    Failed,
    /// Sent.
    Loaded,
}

impl Status {
    /// The literal the column holds.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Failed => "Failed",
            Self::Loaded => "Loaded",
        }
    }
}

/// The most identifiers one mark statement may carry (§6.2).
///
/// The TDS limit is 2100 parameters
/// (`spec/12-message-store.md` §5.6); one is the status, leaving 2099 for
/// identifiers.
pub const MAX_IDENTIFIERS_PER_STATEMENT: usize = ihb_sql::MAX_PARAMETERS - 1;

/// The mark statement for `count` identifiers (§6.1).
///
/// # The `IN` list is placeholders, never values
///
/// §6.2 says so explicitly, and it is the one place in this workspace where
/// a SQL string is built at all. The identifiers are integers read from the
/// database a moment earlier, so interpolating them would be *safe* — and
/// it would also be the habit that stops being safe the first time someone
/// interpolates something else. Placeholders cost nothing here.
///
/// # Examples
///
/// ```
/// use message_replay_job::sql::mark_sql;
///
/// let sql = mark_sql(3);
///
/// assert!(sql.contains("SET Status = @P1"));
/// assert!(sql.contains("WHERE ReplayId IN (@P2, @P3, @P4)"), "{sql}");
/// ```
#[must_use]
pub fn mark_sql(count: usize) -> String {
    // @P1 is the status; identifiers start at @P2.
    let placeholders: Vec<String> = (2..)
        .take(count)
        .map(|number| format!("@P{number}"))
        .collect();

    format!(
        "UPDATE monitoring.MessageReplayQueue \
         SET Status = @P1, ProcessedAt = SYSUTCDATETIME() \
         WHERE ReplayId IN ({});",
        placeholders.join(", ")
    )
}

/// Split identifiers into runs that fit one statement (§6.2).
///
/// # Examples
///
/// ```
/// use message_replay_job::sql::{identifier_chunks, MAX_IDENTIFIERS_PER_STATEMENT};
///
/// assert_eq!(MAX_IDENTIFIERS_PER_STATEMENT, 2099);
///
/// let ids: Vec<i64> = (0..5000).collect();
/// let split: Vec<usize> = identifier_chunks(&ids).map(<[i64]>::len).collect();
///
/// assert_eq!(split, [2099, 2099, 802]);
/// ```
pub fn identifier_chunks(identifiers: &[i64]) -> impl Iterator<Item = &[i64]> {
    identifiers.chunks(MAX_IDENTIFIERS_PER_STATEMENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fetch_carries_readpast_and_orders_twice() {
        // spec/13-message-replay.md §3.1 and §3.3 — both orderings, because
        // a clinical feed replayed out of order can apply an older update
        // over a newer one.
        assert!(FETCH_SQL.contains("WITH (READPAST)"));
        assert_eq!(FETCH_SQL.matches("ORDER BY").count(), 2);
        assert!(FETCH_SQL.contains("ORDER BY ReplayId"));
        assert!(FETCH_SQL.contains("ORDER BY b.ReplayId"));
    }

    #[test]
    fn the_fetch_selects_both_recoverable_statuses() {
        // §2.2 — re-running the job is the recovery procedure, and that
        // only works because `Failed` rows are selected again.
        assert!(FETCH_SQL.contains("Status IN ('Failed', 'Pending')"));
    }

    #[test]
    fn the_status_literals_are_exact_and_case_sensitive() {
        // §2.1.
        assert_eq!(Status::Pending.as_str(), "Pending");
        assert_eq!(Status::Failed.as_str(), "Failed");
        assert_eq!(Status::Loaded.as_str(), "Loaded");
    }

    #[test]
    fn the_mark_statement_uses_placeholders_for_every_identifier() {
        // §6.2 and §8.3.
        let sql = mark_sql(3);

        assert!(sql.contains("SET Status = @P1"), "{sql}");
        assert!(sql.contains("IN (@P2, @P3, @P4)"), "{sql}");
        assert_eq!(sql.matches("@P").count(), 4, "the status plus three ids");
        assert!(sql.contains("SYSUTCDATETIME()"), "{sql}");
    }

    #[test]
    fn the_identifier_limit_leaves_room_for_the_status() {
        // §6.2 — one parameter for the status plus one per identifier.
        assert_eq!(MAX_IDENTIFIERS_PER_STATEMENT, 2099);
        assert_eq!(MAX_IDENTIFIERS_PER_STATEMENT + 1, ihb_sql::MAX_PARAMETERS);

        // And a full statement is exactly at the limit, not over it.
        let full = mark_sql(MAX_IDENTIFIERS_PER_STATEMENT);
        assert_eq!(full.matches("@P").count(), ihb_sql::MAX_PARAMETERS);
    }

    #[test]
    fn the_chunking_boundaries() {
        // §8.3.
        let at_limit: Vec<i64> = (0..2099).collect();
        assert_eq!(identifier_chunks(&at_limit).count(), 1);

        let over: Vec<i64> = (0..2100).collect();
        let split: Vec<usize> = identifier_chunks(&over).map(<[i64]>::len).collect();
        assert_eq!(split, [2099, 1]);
    }

    #[test]
    fn an_empty_identifier_list_produces_no_statements() {
        // §6.3 — a no-op, rather than a statement with an empty `IN ()`,
        // which is a syntax error.
        assert_eq!(identifier_chunks(&[]).count(), 0);
    }
}
