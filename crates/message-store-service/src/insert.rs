//! Writing a batch.
//!
//! Implements `spec/12-message-store.md` §2.1, §2.2 and §5.6.

use ihb_sql::{SqlError, chunks};
use time::OffsetDateTime;

use crate::record::Record;

/// The insert statement, exactly as §2.1 gives it, for one row.
///
/// The column order is normative and the parameter order follows it. A
/// mismatch would put a raw payload in the `SessionId` column, which no
/// test of a single row would catch — hence the constant rather than a
/// string built per call.
pub const COLUMNS: &str = "ReceivedAt, StoredAt, CorrelationId, SourceSystem, \
                           ProcessingComponent, TargetSystem, RawPayload, XmlPayload, SessionId";

/// The `VALUES` placeholders for one row, in `tiberius`'s numbered form.
fn placeholders(row: usize) -> String {
    let base = row * ihb_sql::PARAMETERS_PER_ROW;
    let list: Vec<String> = (1..=ihb_sql::PARAMETERS_PER_ROW)
        .map(|offset| format!("@P{}", base + offset))
        .collect();
    format!("({})", list.join(", "))
}

/// The statement for `count` rows.
///
/// One `INSERT` with several `VALUES` tuples rather than several
/// statements: a hundred round trips per batch would dominate the cost, and
/// they all share the transaction anyway.
///
/// # Examples
///
/// ```
/// use message_store_service::insert::statement;
///
/// let sql = statement(2);
///
/// assert!(sql.starts_with("INSERT INTO monitoring.Message ("));
/// assert!(sql.contains("(@P1, @P2, @P3, @P4, @P5, @P6, @P7, @P8, @P9)"));
/// assert!(sql.contains("(@P10, @P11, @P12, @P13, @P14, @P15, @P16, @P17, @P18)"));
/// ```
#[must_use]
pub fn statement(count: usize) -> String {
    let values: Vec<String> = (0..count).map(placeholders).collect();
    format!(
        "INSERT INTO monitoring.Message ({COLUMNS}) VALUES {}",
        values.join(", ")
    )
}

/// Insert every record, splitting into statements that fit TDS's parameter
/// limit (§5.6).
///
/// `stored_at` is computed **once per batch** (§2.2), so every row of a
/// batch shares an insert timestamp. That is not an optimisation: it is
/// what lets an operator group a batch in the table afterwards.
///
/// # Why `query` rather than `Query::bind`
///
/// `tiberius` implements `IntoSql` — which `Query::bind` needs — for
/// `chrono` types but only `ToSql` for `time` ones. Using `bind` would mean
/// adding `chrono` for the sake of two columns, and the workspace already
/// has one date library. `Client::query` takes `&[&dyn ToSql]`, which
/// `time::PrimitiveDateTime` does implement.
///
/// # Errors
///
/// [`SqlError::Statement`] on the first statement that fails. The caller's
/// transaction rolls back, so a partial batch never reaches the table.
pub async fn insert_all<S>(
    client: &mut tiberius::Client<S>,
    records: &[Record],
    stored_at: OffsetDateTime,
) -> Result<(), SqlError>
where
    S: futures_util::AsyncRead + futures_util::AsyncWrite + Unpin + Send,
{
    // `datetime2` is a local date and time with no offset. Both values are
    // already UTC (`spec/index.md` §0.7), so dropping the offset loses
    // nothing — and storing a local time with an offset the column cannot
    // hold would be worse.
    let stored = as_datetime2(stored_at);

    for chunk in chunks(records) {
        // The converted timestamps have to outlive the parameter slice, so
        // they are materialised before it is built.
        let received: Vec<time::PrimitiveDateTime> = chunk
            .iter()
            .map(|record| as_datetime2(record.received_at))
            .collect();

        let mut parameters: Vec<&dyn tiberius::ToSql> =
            Vec::with_capacity(chunk.len() * ihb_sql::PARAMETERS_PER_ROW);
        for (record, received_at) in chunk.iter().zip(received.iter()) {
            // The order here IS §2.1's column order. Changing one without
            // the other silently writes every value one column left.
            parameters.push(received_at);
            parameters.push(&stored);
            parameters.push(&record.correlation_id);
            parameters.push(&record.source_system);
            parameters.push(&record.processing_component);
            parameters.push(&record.target_system);
            parameters.push(&record.raw_payload);
            parameters.push(&record.xml_payload);
            parameters.push(&record.session_id);
        }

        client
            .query(statement(chunk.len()), &parameters)
            .await
            .map_err(|error| SqlError::Statement(error.to_string()))?
            .into_results()
            .await
            .map_err(|error| SqlError::Statement(error.to_string()))?;
    }

    Ok(())
}

/// A UTC moment as the `datetime2` the column holds.
///
/// Every timestamp reaching here is UTC by `spec/index.md` §0.7, so this
/// converts rather than assumes: a value that somehow arrived with another
/// offset is normalised to UTC first, and only then has its offset dropped.
const fn as_datetime2(moment: OffsetDateTime) -> time::PrimitiveDateTime {
    let utc = moment.to_offset(time::UtcOffset::UTC);
    time::PrimitiveDateTime::new(utc.date(), utc.time())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_columns_are_the_ones_section_two_one_names() {
        // spec/12-message-store.md §2.1 — the order is the contract, and
        // the replay job of spec/13 reads these columns by name.
        assert_eq!(
            COLUMNS,
            "ReceivedAt, StoredAt, CorrelationId, SourceSystem, \
             ProcessingComponent, TargetSystem, RawPayload, XmlPayload, SessionId"
        );
        assert_eq!(COLUMNS.split(',').count(), ihb_sql::PARAMETERS_PER_ROW);
    }

    #[test]
    fn one_row_produces_nine_placeholders() {
        let sql = statement(1);

        assert!(
            sql.contains("(@P1, @P2, @P3, @P4, @P5, @P6, @P7, @P8, @P9)"),
            "{sql}"
        );
        assert_eq!(sql.matches("@P").count(), 9);
    }

    #[test]
    fn placeholders_continue_across_rows() {
        // The second row's parameters are @P10..@P18, not @P1..@P9 again.
        // Restarting the numbering would bind every row over the first.
        let sql = statement(3);

        assert_eq!(sql.matches("@P").count(), 27);
        assert!(
            sql.contains("@P19, @P20, @P21, @P22, @P23, @P24, @P25, @P26, @P27"),
            "{sql}"
        );
    }

    #[test]
    fn a_timestamp_is_normalised_to_utc_before_its_offset_is_dropped() {
        // `spec/index.md` §0.7 — everything here is already UTC, but
        // dropping an offset without converting would shift a moment by
        // hours and put the wrong `ReceivedAt` in the audit record.
        let one_pm_in_paris = OffsetDateTime::from_unix_timestamp(1_770_000_000)
            .unwrap()
            .to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap());

        let stored = as_datetime2(one_pm_in_paris);
        let utc = one_pm_in_paris.to_offset(time::UtcOffset::UTC);

        assert_eq!(stored.hour(), utc.hour());
        assert_ne!(stored.hour(), one_pm_in_paris.hour(), "the offset mattered");
    }

    #[test]
    fn a_full_statement_stays_inside_the_parameter_limit() {
        // §5.6 — 233 rows is the most one statement may carry.
        let sql = statement(ihb_sql::ROWS_PER_STATEMENT);

        assert_eq!(
            sql.matches("@P").count(),
            ihb_sql::ROWS_PER_STATEMENT * ihb_sql::PARAMETERS_PER_ROW
        );
        assert!(sql.matches("@P").count() <= ihb_sql::MAX_PARAMETERS);
    }
}
