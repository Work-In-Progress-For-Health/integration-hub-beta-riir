//! The Log Analytics query calls.
//!
//! Implements `spec/15-dashboard.md` §4.3, §4.5 and §4.6's Log Analytics
//! row.
//!
//! # The response is a table, not a list of objects
//!
//! Log Analytics answers with `tables[].columns[]` and `tables[].rows[]`,
//! where a row is a positional array. Reading it means pairing each row
//! with the column names — and getting the pairing wrong puts a timestamp
//! in a workflow id without any error, which is the failure mode this
//! module's tests are mostly about.

use std::collections::BTreeMap;

use crate::azure::http::{HttpError, HttpsClient};

/// The Log Analytics host (§4.6).
pub const LOGS_HOST: &str = "https://api.loganalytics.io";

/// One row, by column name.
pub type Row = BTreeMap<String, serde_json::Value>;

/// The URL for a workspace query (§4.6).
///
/// # Examples
///
/// ```
/// use dashboard::azure::logs::query_url;
///
/// assert_eq!(
///     query_url("workspace-1"),
///     "https://api.loganalytics.io/v1/workspaces/workspace-1/query"
/// );
/// ```
#[must_use]
pub fn query_url(workspace_id: &str) -> String {
    format!("{LOGS_HOST}/v1/workspaces/{workspace_id}/query")
}

/// Run a query and return its rows (§4.3).
///
/// # Errors
///
/// [`HttpError`]. §4.5 makes a failure **no data** rather than an error for
/// the caller, so alarm rules report `unknown` rather than `healthy`.
pub async fn query(
    client: &HttpsClient,
    token: &str,
    workspace_id: &str,
    kql: &str,
) -> Result<Vec<Row>, HttpError> {
    let body = serde_json::json!({ "query": kql }).to_string();
    let document = client
        .post_json(&query_url(workspace_id), Some(token), body)
        .await?;

    Ok(parse(&document))
}

/// Turn a Log Analytics response into rows keyed by column name.
///
/// Split out so every shape below can be tested against a captured
/// response rather than a live workspace.
#[must_use]
pub fn parse(document: &serde_json::Value) -> Vec<Row> {
    let Some(tables) = document.get("tables").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };

    let mut out = Vec::new();

    for table in tables {
        let Some(columns) = table.get("columns").and_then(serde_json::Value::as_array) else {
            continue;
        };
        let names: Vec<String> = columns
            .iter()
            .map(|column| {
                column
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();

        let Some(rows) = table.get("rows").and_then(serde_json::Value::as_array) else {
            continue;
        };

        for row in rows {
            let Some(values) = row.as_array() else {
                continue;
            };

            // `zip` stops at the shorter of the two. A row with more values
            // than the table has columns is a response we do not understand,
            // and taking the first N by position would silently mislabel
            // every one of them.
            if values.len() != names.len() {
                continue;
            }

            out.push(
                names
                    .iter()
                    .cloned()
                    .zip(values.iter().cloned())
                    .collect::<Row>(),
            );
        }
    }

    out
}

/// A row's value as text.
///
/// Log Analytics returns a `datetime` column as a string and a `long` as a
/// number, and a caller wanting "whatever was there" should not have to
/// care which.
#[must_use]
pub fn text(row: &Row, column: &str) -> Option<String> {
    match row.get(column)? {
        serde_json::Value::Null => None,
        serde_json::Value::String(value) => Some(value.clone()),
        other => Some(other.to_string()),
    }
}

/// A row's value as a number.
///
/// Accepts a number or a numeric string, because `summarize count()`
/// returns a number and some columns come back as text.
#[must_use]
pub fn number(row: &Row, column: &str) -> Option<f64> {
    match row.get(column)? {
        serde_json::Value::Number(value) => value.as_f64(),
        serde_json::Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

/// A row's value as a whole number that is exact in an `i64`, else `None`.
#[must_use]
pub fn integer(row: &Row, column: &str) -> Option<i64> {
    exact_i64(number(row, column)?)
}

/// A row's value as a non-negative whole number, else `None`.
#[must_use]
pub fn count(row: &Row, column: &str) -> Option<u64> {
    u64::try_from(integer(row, column)?).ok()
}

/// `value` as an `i64` if it is finite, integral and in range (RCA-CONV-3).
fn exact_i64(value: f64) -> Option<i64> {
    // 2^63 is exactly representable; `i64::MAX as f64` rounds up to it.
    const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
    if !(value.is_finite() && value.fract() == 0.0 && (-TWO_POW_63..TWO_POW_63).contains(&value)) {
        return None;
    }
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "RCA-CONV-3: finite, integral and in range, checked above"
    )]
    Some(value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response shaped like Log Analytics', from §4.3.1's Alarm 1 query.
    fn response() -> serde_json::Value {
        serde_json::json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    {"name": "workflow_id", "type": "string"},
                    {"name": "last_message", "type": "datetime"}
                ],
                "rows": [
                    ["phw-to-mpi", "2026-08-22T09:15:00Z"],
                    ["pims-to-mpi", "2026-08-22T08:00:00Z"]
                ]
            }]
        })
    }

    #[test]
    fn the_url_is_the_workspace_query_path() {
        // spec/15-dashboard.md §4.6.
        assert_eq!(
            query_url("abc-123"),
            "https://api.loganalytics.io/v1/workspaces/abc-123/query"
        );
    }

    #[test]
    fn rows_are_paired_with_their_column_names() {
        // THE thing this module is for. A row is a positional array, so
        // pairing it wrongly puts a timestamp in a workflow id with no
        // error anywhere.
        let rows = parse(&response());

        assert_eq!(rows.len(), 2);
        assert_eq!(text(&rows[0], "workflow_id").as_deref(), Some("phw-to-mpi"));
        assert_eq!(
            text(&rows[0], "last_message").as_deref(),
            Some("2026-08-22T09:15:00Z")
        );
        assert_eq!(
            text(&rows[1], "workflow_id").as_deref(),
            Some("pims-to-mpi")
        );
    }

    #[test]
    fn a_row_whose_width_does_not_match_is_dropped() {
        // Taking the first N values by position would mislabel every one
        // of them, and nothing downstream could tell.
        let document = serde_json::json!({
            "tables": [{
                "columns": [{"name": "a"}, {"name": "b"}],
                "rows": [
                    ["1", "2"],
                    ["1"],
                    ["1", "2", "3"]
                ]
            }]
        });

        let rows = parse(&document);

        assert_eq!(rows.len(), 1, "only the row that matches the columns");
        assert_eq!(text(&rows[0], "a").as_deref(), Some("1"));
    }

    #[test]
    fn several_tables_are_all_read() {
        // A query with more than one result set. The Python iterates every
        // table too, so dropping the rest would silently lose rows.
        let document = serde_json::json!({
            "tables": [
                {"columns": [{"name": "a"}], "rows": [["1"]]},
                {"columns": [{"name": "a"}], "rows": [["2"]]}
            ]
        });

        assert_eq!(parse(&document).len(), 2);
    }

    #[test]
    fn an_unexpected_response_is_no_rows_rather_than_a_panic() {
        // §4.5 — a partial or failed query is **no data**. Every one of
        // these is a real thing Log Analytics or a proxy can return.
        for document in [
            serde_json::json!({}),
            serde_json::json!({"error": {"code": "BadArgumentError"}}),
            serde_json::json!({"tables": "not an array"}),
            serde_json::json!({"tables": [{"rows": [["x"]]}]}),
            serde_json::json!({"tables": [{"columns": [{"name": "a"}]}]}),
            serde_json::json!({"tables": [{"columns": [{"name": "a"}], "rows": "no"}]}),
            serde_json::Value::Null,
        ] {
            assert!(parse(&document).is_empty(), "{document}");
        }
    }

    #[test]
    fn a_column_with_no_name_does_not_collapse_the_others() {
        // A malformed column list must not silently key two columns under
        // the same empty name and lose one.
        let document = serde_json::json!({
            "tables": [{
                "columns": [{"name": "a"}, {"type": "string"}],
                "rows": [["1", "2"]]
            }]
        });

        let rows = parse(&document);

        assert_eq!(rows.len(), 1);
        assert_eq!(text(&rows[0], "a").as_deref(), Some("1"));
        assert_eq!(text(&rows[0], "").as_deref(), Some("2"));
    }

    #[test]
    fn a_value_is_read_whatever_type_it_arrived_as() {
        // `datetime` arrives as a string, `long` as a number, and a caller
        // wanting "whatever was there" should not have to care.
        let document = serde_json::json!({
            "tables": [{
                "columns": [{"name": "n"}, {"name": "s"}, {"name": "nul"}],
                "rows": [[42, "42", null]]
            }]
        });
        let rows = parse(&document);
        let row = &rows[0];

        assert_eq!(number(row, "n"), Some(42.0));
        assert_eq!(number(row, "s"), Some(42.0), "a numeric string counts");
        assert_eq!(text(row, "n").as_deref(), Some("42"));
        assert_eq!(text(row, "s").as_deref(), Some("42"));

        // Null is absent, not zero and not an empty string — the same
        // distinction §2.2.2 makes everywhere else.
        assert_eq!(text(row, "nul"), None);
        assert_eq!(number(row, "nul"), None);
        assert_eq!(text(row, "missing"), None);
        assert_eq!(number(row, "missing"), None);
    }

    #[test]
    fn a_count_of_zero_is_zero_and_not_absent() {
        // The other side of the same distinction: `summarize count()`
        // returning 0 is a real answer.
        let document = serde_json::json!({
            "tables": [{"columns": [{"name": "Total"}], "rows": [[0]]}]
        });
        let rows = parse(&document);

        assert_eq!(number(&rows[0], "Total"), Some(0.0));
    }

    #[test]
    fn whole_numbers_are_exact_or_nothing() {
        let row = |value: serde_json::Value| {
            let mut row = Row::new();
            row.insert("n".into(), value);
            row
        };
        assert_eq!(integer(&row(serde_json::json!(3)), "n"), Some(3));
        assert_eq!(integer(&row(serde_json::json!("7")), "n"), Some(7));
        assert_eq!(integer(&row(serde_json::json!(2.5)), "n"), None);
        assert_eq!(integer(&row(serde_json::json!(1e300)), "n"), None);
        assert_eq!(
            count(&row(serde_json::json!(-1)), "n"),
            None,
            "no negative count"
        );
        assert_eq!(count(&row(serde_json::json!(12)), "n"), Some(12));
        assert_eq!(count(&Row::new(), "n"), None, "absent is not zero");
    }
}
