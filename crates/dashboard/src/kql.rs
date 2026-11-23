//! Building Log Analytics queries.
//!
//! Implements `spec/15-dashboard.md` §4.3 and §4.4.
//!
//! # Why there is a sanitiser at all
//!
//! The Log Analytics client takes a query as **text**. There is no
//! parameter binding, so every identifier that reaches a query — a workflow
//! id from a form, a health board from a query string — is concatenated
//! into KQL. That is injection, and the only defence is the filter in
//! [`identifier`].
//!
//! §4.4 fixes the rule as the Python's: keep ASCII alphanumerics, hyphen
//! and underscore, drop everything else. The clause says the Rust "MUST NOT
//! widen it", and the reason is that widening is invisible — a query with
//! an injected clause returns *rows*, not an error, so a dashboard would go
//! on looking healthy while answering the wrong question.

use std::fmt::Write as _;

/// Strip a value to what may appear inside a query (§4.4).
///
/// ASCII alphanumerics, `-` and `_` survive; everything else is dropped.
/// Dropped rather than escaped, because there is no escaping to get right:
/// the result is used as a bare identifier and as the contents of a
/// double-quoted KQL string, and this character set is safe in both.
///
/// # Examples
///
/// An ordinary workflow id is unchanged:
///
/// ```
/// use dashboard::kql::identifier;
///
/// assert_eq!(identifier("phw-to-mpi"), "phw-to-mpi");
/// assert_eq!(identifier("lims_to_mpi"), "lims_to_mpi");
/// ```
///
/// Anything that could end a string or start a clause does not survive:
///
/// ```
/// use dashboard::kql::identifier;
///
/// assert_eq!(identifier(r#"x" or 1==1 //"#), "xor11");
/// assert_eq!(identifier("a | project b"), "aprojectb");
/// assert_eq!(identifier("'; drop"), "drop");
/// ```
///
/// Including non-ASCII, which KQL would accept in an identifier and which
/// has no business arriving from a form:
///
/// ```
/// use dashboard::kql::identifier;
///
/// assert_eq!(identifier("phw–to–mpi"), "phwtompi", "en dashes, not hyphens");
/// assert_eq!(identifier("🏥"), "");
/// ```
#[must_use]
pub fn identifier(value: &str) -> String {
    kql_sanitize::identifier(value)
}

/// A KQL list literal of sanitised identifiers: `"a", "b"`.
///
/// Empty values are dropped. An empty result means the caller has nothing
/// to ask about and MUST NOT run the query — `in ()` is a syntax error, and
/// a query that fails reads as `unknown` for every rule at once (§4.5).
///
/// # Examples
///
/// ```
/// use dashboard::kql::identifier_list;
///
/// assert_eq!(
///     identifier_list(["phw-to-mpi", "lims-to-mpi"]),
///     r#""phw-to-mpi", "lims-to-mpi""#
/// );
///
/// // Values that sanitise away are dropped rather than left as `""`.
/// assert_eq!(identifier_list(["🏥", "ok"]), r#""ok""#);
/// assert!(identifier_list(["🏥"]).is_empty());
/// ```
#[must_use]
pub fn identifier_list<I, S>(values: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    kql_sanitize::identifier_list(values)
}

/// The optional resource filter of §4.3.3.
///
/// Returns a clause to append, or an empty string when
/// `AZURE_APP_INSIGHTS_RESOURCE_ID` is not set.
///
/// # Examples
///
/// ```
/// use dashboard::kql::resource_filter;
///
/// assert_eq!(resource_filter(None), "");
/// assert!(resource_filter(Some("/subscriptions/abc/x"))
///     .contains("_ResourceId =~"));
/// ```
#[must_use]
pub fn resource_filter(resource_id: Option<&str>) -> String {
    // An ARM resource id is a path, so §4.4's identifier filter is too
    // strict for it. It is sanitised for the characters that could end the
    // literal or start a clause instead: quotes, pipes and newlines.
    resource_id.map_or_else(String::new, |id| {
        // An ARM resource id is a path, so §4.4's identifier filter is too
        // strict for it. `kql_sanitize::path` is the looser rule: keep what
        // a path needs, drop what could end the literal or start a clause.
        format!("\n    | where _ResourceId =~ '{}'", kql_sanitize::path(id))
    })
}

/// §4.3.1's "Alarm 1 — last received".
///
/// The most recent `MESSAGE_RECEIVED` trace per workflow, over 30 days.
#[must_use]
pub fn alarm1_last_received(workflow_ids: &[String], resource_id: Option<&str>) -> Option<String> {
    let ids = identifier_list(workflow_ids);
    if ids.is_empty() {
        return None;
    }

    Some(format!(
        r#"
    AppTraces
    | where TimeGenerated > ago(30d){filter}
    | where Message == "Integration Hub Event"
    | where Properties contains "MESSAGE_RECEIVED"
    | extend workflow_id = tostring(parse_json(Properties)["workflow_id"])
    | where workflow_id in ({ids})
    | summarize last_message = max(TimeGenerated) by workflow_id
    "#,
        filter = resource_filter(resource_id),
    ))
}

/// §4.3.1's "Alarm 2 — last sent".
///
/// Metrics, not traces — see §4.3.2, which is a real asymmetry rather than
/// an oversight to tidy.
#[must_use]
pub fn alarm2_last_sent(
    workflow_ids: &[String],
    lookback_days: u32,
    resource_id: Option<&str>,
) -> Option<String> {
    let ids = identifier_list(workflow_ids);
    if ids.is_empty() {
        return None;
    }

    Some(format!(
        r#"
    AppMetrics
    | where TimeGenerated > ago({lookback_days}d){filter}
    | where Name == "messages_sent"
    | extend workflow_id = tostring(Properties["workflow_id"])
    | where workflow_id in ({ids})
    | summarize last_message = max(TimeGenerated) by workflow_id
    "#,
        filter = resource_filter(resource_id),
    ))
}

/// §4.3.1's "Alarm 3 — failures".
///
/// A count within the rule's own window, so this is a rate rather than an
/// age (§5.5.3).
#[must_use]
pub fn alarm3_failures(
    workflow_ids: &[String],
    window_minutes: u32,
    resource_id: Option<&str>,
) -> Option<String> {
    let ids = identifier_list(workflow_ids);
    if ids.is_empty() {
        return None;
    }

    Some(format!(
        r#"
    AppTraces
    | where TimeGenerated > ago({window_minutes}m){filter}
    | where Message has "Integration Hub Event"
    | extend workflow_id = tostring(parse_json(Properties)["workflow_id"]),
             event_type  = tostring(parse_json(Properties)["event_type"])
    | where workflow_id in ({ids})
    | where event_type == "MESSAGE_FAILED"
    | summarize Total = count() by workflow_id
    "#,
        filter = resource_filter(resource_id),
    ))
}

/// §4.3.1's "Exceptions".
#[must_use]
pub fn exceptions(hours: u32, resource_id: Option<&str>) -> String {
    format!(
        r"
    AppExceptions
    | where TimeGenerated > ago({hours}h){filter}
    | project timestamp=TimeGenerated,
              type=ExceptionType,
              outerMessage=coalesce(OuterMessage, Message),
              severityLevel=SeverityLevel,
              appName=AppRoleName,
              operation_Id=OperationId
    | order by timestamp desc
    | take 200
    ",
        filter = resource_filter(resource_id),
    )
}

/// §4.3.1's "Messages today".
///
/// `workflow_id` is an optional substring filter on `Properties`, which is
/// what the Python does — not an extracted-field comparison, so it matches
/// a workflow id appearing anywhere in the properties blob.
#[must_use]
pub fn messages_today(workflow_id: Option<&str>, resource_id: Option<&str>) -> String {
    let workflow_filter = workflow_id
        .map(identifier)
        .filter(|id| !id.is_empty())
        .map_or_else(String::new, |id| {
            format!("\n    | where Properties contains \"{id}\"")
        });

    format!(
        r#"
    AppTraces
    | where TimeGenerated > startofday(now()){filter}
    | where Message == "Integration Hub Event"{workflow_filter}
    | project timestamp=TimeGenerated,
              name=Message,
              customDimensions=Properties,
              appName=AppRoleName
    | order by timestamp desc
    | take 500
    "#,
        filter = resource_filter(resource_id),
    )
}

/// §4.3.1's "Throughput series".
///
/// Note the absent resource filter: this is §9.2's finding, reproduced as-is
/// so the numbers do not silently change.
#[must_use]
pub fn throughput(
    hours: u32,
    bin: &str,
    health_board: Option<&str>,
    workflow: Option<&str>,
) -> String {
    let mut filters = String::new();
    if let Some(board) = health_board.map(identifier).filter(|b| !b.is_empty()) {
        let _ = write!(
            filters,
            "\n    | where tostring(Properties[\"health_board\"]) == \"{board}\""
        );
    }
    if let Some(flow) = workflow.map(identifier).filter(|w| !w.is_empty()) {
        let _ = write!(
            filters,
            "\n    | where tostring(Properties[\"workflow_id\"]) == \"{flow}\""
        );
    }

    format!(
        r#"
    AppMetrics
    | where TimeGenerated > ago({hours}h)
    | where Name in ("messages_received", "messages_sent"){filters}
    | summarize Value = sum(Sum) by bin(TimeGenerated, {bin}), Name
    | order by TimeGenerated asc
    "#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything a query must never contain after sanitising.
    const DANGEROUS: [&str; 8] = ["\"", "'", "|", "\n", "\r", "\\", "/", ";"];

    #[test]
    fn the_sanitiser_keeps_only_what_section_four_four_allows() {
        // spec/15-dashboard.md §4.4 and §8.10.
        assert_eq!(identifier("phw-to-mpi"), "phw-to-mpi");
        assert_eq!(identifier("lims_to_mpi_2"), "lims_to_mpi_2");
        assert_eq!(identifier(""), "");
    }

    #[test]
    fn nothing_dangerous_survives_the_sanitiser() {
        // §8.10 — THE test of this file. There is no parameter binding in
        // the Log Analytics client, so this filter is the whole defence.
        let hostile = [
            r#"x" or 1==1 //"#,
            r#"a" | project *; //"#,
            "a\nb",
            "a\r\nb",
            r"a\b",
            "'; drop table x --",
            "*",
            "{}",
            "$(whoami)",
        ];

        for value in hostile {
            let safe = identifier(value);
            for bad in DANGEROUS {
                assert!(
                    !safe.contains(bad),
                    "{value:?} sanitised to {safe:?}, which still contains {bad:?}"
                );
            }
            assert!(
                safe.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "{value:?} sanitised to {safe:?}"
            );
        }
    }

    #[test]
    fn non_ascii_is_dropped_rather_than_transliterated() {
        // KQL accepts a wider identifier alphabet than this. Narrowing to
        // ASCII means a lookalike character cannot stand in for a real one.
        assert_eq!(identifier("phw–to–mpi"), "phwtompi");
        assert_eq!(identifier("café"), "caf");
        assert_eq!(identifier("１２３"), "", "full-width digits are not digits");
    }

    #[test]
    fn a_list_quotes_each_value_and_drops_the_empty_ones() {
        assert_eq!(identifier_list(["a", "b"]), r#""a", "b""#);
        assert_eq!(identifier_list(["a", "", "b"]), r#""a", "b""#);
        assert_eq!(identifier_list(["!!!"]), "");
        assert_eq!(identifier_list::<[&str; 0], _>([]), "");
    }

    #[test]
    fn a_query_with_no_usable_ids_is_not_built_at_all() {
        // `where x in ()` is a KQL syntax error, and §4.5 turns a failed
        // query into `unknown` for every rule in it. Not building it is
        // the difference between "no rules to check" and "every rule is
        // unknown".
        assert!(alarm1_last_received(&[], None).is_none());
        assert!(alarm2_last_sent(&["🏥".to_string()], 7, None).is_none());
        assert!(alarm3_failures(&[], 15, None).is_none());
    }

    #[test]
    fn the_alarm_queries_match_the_python() {
        // §4.3.1. These texts are a contract with the telemetry names of
        // spec/04-observability.md, so they are asserted rather than
        // eyeballed.
        let ids = vec!["phw-to-mpi".to_string()];

        let one = alarm1_last_received(&ids, None).unwrap();
        assert!(one.contains("AppTraces"), "{one}");
        assert!(one.contains("ago(30d)"), "{one}");
        assert!(
            one.contains(r#"Message == "Integration Hub Event""#),
            "{one}"
        );
        assert!(
            one.contains(r#"Properties contains "MESSAGE_RECEIVED""#),
            "{one}"
        );
        assert!(one.contains("summarize last_message = max(TimeGenerated) by workflow_id"));
        assert!(one.contains(r#"in ("phw-to-mpi")"#), "{one}");

        // §4.3.2 — metrics, not traces.
        let two = alarm2_last_sent(&ids, 7, None).unwrap();
        assert!(two.contains("AppMetrics"), "{two}");
        assert!(!two.contains("AppTraces"), "{two}");
        assert!(two.contains(r#"Name == "messages_sent""#), "{two}");
        assert!(two.contains("ago(7d)"), "{two}");

        // §5.5.4 — `has`, not `==`.
        let three = alarm3_failures(&ids, 15, None).unwrap();
        assert!(
            three.contains(r#"Message has "Integration Hub Event""#),
            "{three}"
        );
        assert!(
            three.contains(r#"event_type == "MESSAGE_FAILED""#),
            "{three}"
        );
        assert!(three.contains("ago(15m)"), "{three}");
        assert!(three.contains("summarize Total = count() by workflow_id"));
    }

    #[test]
    fn the_resource_filter_is_present_only_when_configured() {
        // §4.3.3.
        assert_eq!(resource_filter(None), "");

        let ids = vec!["phw-to-mpi".to_string()];
        let unfiltered = alarm1_last_received(&ids, None).unwrap();
        assert!(!unfiltered.contains("_ResourceId"), "{unfiltered}");

        let filtered = alarm1_last_received(&ids, Some("/subscriptions/abc/insights/x")).unwrap();
        assert!(
            filtered.contains("| where _ResourceId =~ '/subscriptions/abc/insights/x'"),
            "{filtered}"
        );
    }

    #[test]
    fn a_hostile_resource_id_cannot_end_its_literal() {
        // An ARM id is a path, so §4.4's alphabet is too narrow for it and
        // it gets its own filter. That filter still has to hold.
        let filter = resource_filter(Some("/subs/x' | project *; //"));

        // Exactly two quotes and one pipe: the ones this function writes.
        // Anything more means the id closed its own literal and started a
        // clause of its own.
        assert_eq!(filter.matches('\'').count(), 2, "{filter}");
        assert_eq!(filter.matches('|').count(), 1, "{filter}");
        assert!(filter.starts_with("\n    | where _ResourceId =~ '"));
        assert!(filter.ends_with('\''), "{filter}");
    }

    #[test]
    fn the_throughput_query_carries_no_resource_filter() {
        // §9.2's finding, reproduced deliberately. This test exists so
        // that "fixing" it is a visible decision rather than a tidy-up:
        // adding the filter changes which rows the chart counts.
        let query = throughput(24, "1h", None, None);

        assert!(
            !query.contains("_ResourceId"),
            "spec/15 §9.2 says this query has no resource filter; \
             adding one changes the numbers an operator sees: {query}"
        );
    }

    #[test]
    fn the_throughput_filters_are_sanitised() {
        let query = throughput(24, "1h", Some(r#"x" or 1==1"#), Some("phw-to-mpi"));

        assert!(query.contains(r#"["health_board"]) == "xor11""#), "{query}");
        assert!(
            query.contains(r#"["workflow_id"]) == "phw-to-mpi""#),
            "{query}"
        );
        // Two quotes per filter clause, none of them injected.
        assert!(!query.contains("or 1==1"), "{query}");
    }

    #[test]
    fn the_exception_query_takes_two_hundred_ordered_rows() {
        // §4.3.1 — the `take` is a contract with the page, which does not
        // paginate.
        let query = exceptions(24, None);

        assert!(query.contains("AppExceptions"));
        assert!(query.contains("ago(24h)"));
        assert!(query.contains("coalesce(OuterMessage, Message)"));
        assert!(query.contains("order by timestamp desc"));
        assert!(query.contains("take 200"));
    }

    #[test]
    fn messages_today_starts_at_midnight_and_filters_by_substring() {
        // §4.3.1. The workflow filter is `Properties contains`, which is
        // what the Python does — a substring of the whole properties blob,
        // not an extracted field.
        let all = messages_today(None, None);
        assert!(all.contains("startofday(now())"), "{all}");
        assert!(!all.contains("Properties contains"), "{all}");
        assert!(all.contains("take 500"));

        let one = messages_today(Some("phw-to-mpi"), None);
        assert!(one.contains(r#"Properties contains "phw-to-mpi""#), "{one}");
    }

    #[test]
    fn no_built_query_contains_an_unbalanced_quote() {
        // A cheap invariant over every builder: KQL string literals are
        // double-quoted, so an odd count means something escaped.
        let ids = vec!["phw-to-mpi".to_string(), r#"bad" or 1==1"#.to_string()];
        let queries = [
            alarm1_last_received(&ids, Some("/subs/x")).unwrap(),
            alarm2_last_sent(&ids, 7, None).unwrap(),
            alarm3_failures(&ids, 15, None).unwrap(),
            exceptions(1, Some("/subs/x")),
            messages_today(Some(r#"x" //"#), None),
            throughput(1, "5m", Some(r#"y""#), None),
        ];

        for query in queries {
            assert_eq!(
                query.matches('"').count() % 2,
                0,
                "unbalanced quotes: {query}"
            );
        }
    }
}
