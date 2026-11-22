//! CSV parsing shared by seed loading and uploads.
//!
//! Implements `spec/20-lookup-service.md` §6.1 and §6.2.
//!
//! Parsing produces a header plus (line number, cells) records;
//! [`import_rows`] then validates the header against the table and maps the
//! records to canonical rows. All problems are reported with line numbers,
//! never data values (§6.4).

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::error::LookupError;
use crate::mapping::{
    DEFAULT_MAX_ERRORS, MappingResult, RecordError, RecordMapping, map_records, name_list,
};
use crate::model::{Case, KeyPartRule, TableDefinition};

/// The line a header problem is reported on.
pub const HEADER_LINE: usize = 1;

/// A parsed CSV file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CsvDocument {
    /// Trimmed header cells.
    pub header: Vec<String>,
    /// Records as (line the record ends on, cells).
    pub records: Vec<(usize, Vec<String>)>,
}

/// Decode UTF-8, tolerating a leading BOM (§6.1).
///
/// # Errors
///
/// `invalid_upload` with the byte offset of the first invalid sequence.
pub fn decode_csv_bytes(data: &[u8]) -> Result<String, LookupError> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    std::str::from_utf8(data)
        .map(str::to_owned)
        .map_err(|error| {
            LookupError::invalid_upload(format!(
                "File is not valid UTF-8 (byte offset {})",
                error.valid_up_to()
            ))
        })
}

/// Split text into CSV records the way Python's `csv.reader` does with its
/// defaults, returning each with the physical line it ends on. A blank line
/// is a record with no cells.
fn parse_records(text: &str) -> Vec<(usize, Vec<String>)> {
    let mut records = Vec::new();
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    // A quote opens a quoted field only at the start of a field; elsewhere it
    // is literal (non-strict `csv`).
    let mut field_started = false;
    let mut touched = false;
    let mut lines = 0_usize;

    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                '\r' | '\n' => {
                    // A `\r\n` inside quotes is one line break, kept as is.
                    field.push(ch);
                    if ch == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                        field.push('\n');
                    }
                    lines = next_line(lines);
                }
                _ => field.push(ch),
            }
            continue;
        }
        match ch {
            ',' => {
                fields.push(std::mem::take(&mut field));
                field_started = false;
                touched = true;
            }
            '"' if !field_started => {
                in_quotes = true;
                field_started = true;
                touched = true;
            }
            '\r' | '\n' => {
                if ch == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines = next_line(lines);
                if touched {
                    fields.push(std::mem::take(&mut field));
                }
                records.push((lines, std::mem::take(&mut fields)));
                field_started = false;
                touched = false;
            }
            _ => {
                field.push(ch);
                field_started = true;
                touched = true;
            }
        }
    }
    if touched {
        fields.push(field);
        records.push((next_line(lines), fields));
    }
    records
}

/// The line after `line`, holding at `usize::MAX` (`spec/20-lookup-service.md`
/// §7): line numbers only label messages, and a file with that many lines
/// cannot be held in memory, so the hold is unreachable, not a wrong answer.
const fn next_line(line: usize) -> usize {
    match line.checked_add(1) {
        Some(next) => next,
        None => usize::MAX, // §7
    }
}

/// Parse CSV text. Blank lines are skipped; ragged rows and header problems
/// are errors (§6.1, §6.2).
#[must_use]
pub fn read_csv(text: &str, max_errors: usize) -> (CsvDocument, Vec<RecordError>) {
    let mut records = parse_records(text).into_iter();
    let header: Vec<String> = records
        .next()
        .map_or_default(|(_, cells)| cells.iter().map(|c| c.trim().to_owned()).collect());

    if header.iter().all(String::is_empty) {
        return (
            CsvDocument::default(),
            vec![RecordError::new(
                HEADER_LINE,
                "file is empty; a header row is required",
            )],
        );
    }
    let mut errors = Vec::new();
    if header.iter().any(String::is_empty) {
        errors.push(RecordError::new(
            HEADER_LINE,
            "header contains an empty column name",
        ));
    }
    if header.iter().collect::<BTreeSet<_>>().len() != header.len() {
        errors.push(RecordError::new(
            HEADER_LINE,
            "header contains duplicate column names",
        ));
    }

    let mut kept = Vec::new();
    for (line, cells) in records {
        if cells.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        if cells.len() != header.len() {
            if errors.len() < max_errors {
                errors.push(RecordError::new(
                    line,
                    format!("expected {} columns, got {}", header.len(), cells.len()),
                ));
            }
            continue;
        }
        kept.push((line, cells));
    }
    (
        CsvDocument {
            header,
            records: kept,
        },
        errors,
    )
}

/// [`read_csv`] with the default error limit.
#[must_use]
pub fn read_csv_default(text: &str) -> (CsvDocument, Vec<RecordError>) {
    read_csv(text, DEFAULT_MAX_ERRORS)
}

/// Validate the header against the mapping and map every record to a
/// canonical row (§6.3).
#[must_use]
pub fn import_rows(
    document: &CsvDocument,
    definition: &TableDefinition,
    mapping: Option<&RecordMapping>,
) -> MappingResult {
    let identity = RecordMapping::identity(definition);
    let mapping = mapping.unwrap_or(&identity);
    let mut problems = mapping.check_targets(definition);
    let source_fields = mapping.source_fields();
    let header: BTreeSet<&str> = document.header.iter().map(String::as_str).collect();
    let missing: Vec<&str> = source_fields
        .iter()
        .copied()
        .filter(|f| !header.contains(*f))
        .collect();
    if !missing.is_empty() {
        problems.push(format!(
            "columns {} are not in the header",
            name_list(&missing)
        ));
    }
    let unmapped: Vec<&str> = header
        .iter()
        .copied()
        .filter(|c| !source_fields.contains(*c))
        .collect();
    if !unmapped.is_empty() {
        // Explicit rather than silently dropping data someone expected to be
        // served.
        problems.push(format!(
            "header columns {} are neither key nor value columns",
            name_list(&unmapped)
        ));
    }
    if !problems.is_empty() {
        return MappingResult {
            errors: problems
                .into_iter()
                .map(|problem| RecordError::new(HEADER_LINE, problem))
                .collect(),
            ..MappingResult::default()
        };
    }
    map_records(
        definition,
        mapping,
        &document.header,
        &document.records,
        DEFAULT_MAX_ERRORS,
    )
}

/// Python's truthiness, for the `preload` flag a manifest or form supplies.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// A list of strings from a JSON value, or `None` when absent or empty.
fn string_list(value: Option<&Value>, field: &str) -> Result<Option<Vec<String>>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<String>>>()
            .map(|list| if list.is_empty() { None } else { Some(list) })
            .ok_or_else(|| format!("{field}: must be a list of strings")),
        Some(_) => Err(format!("{field}: must be a list of strings")),
    }
}

/// Create a definition from a CSV header plus optional overrides.
///
/// Without overrides the first column is the key and the remaining columns
/// are values (§7.1). Unknown override fields are ignored here; the seed
/// manifest rejects them before it gets this far.
///
/// # Errors
///
/// Every validation problem (§3.1), never with a value.
pub fn build_definition(
    name: &str,
    header: &[String],
    settings: &Map<String, Value>,
) -> Result<TableDefinition, Vec<String>> {
    let mut problems = Vec::new();
    let mut take_list = |field: &str| match string_list(settings.get(field), field) {
        Ok(list) => list,
        Err(problem) => {
            problems.push(problem);
            None
        }
    };
    let key_columns =
        take_list("key_columns").unwrap_or_else(|| header.iter().take(1).cloned().collect());
    let value_columns = take_list("value_columns").unwrap_or_else(|| {
        header
            .iter()
            .filter(|column| !key_columns.contains(column))
            .cloned()
            .collect()
    });

    let mut key_normalisation = std::collections::BTreeMap::new();
    match settings.get("key_normalisation") {
        None | Some(Value::Null) => {}
        Some(Value::Object(rules)) => {
            for (column, rule) in rules {
                match KeyPartRule::from_json(rule, &format!("key_normalisation.{column}")) {
                    Ok(rule) => {
                        key_normalisation.insert(column.clone(), rule);
                    }
                    Err(mut more) => problems.append(&mut more),
                }
            }
        }
        Some(_) => problems.push("key_normalisation: must be an object".to_owned()),
    }

    let description = match settings.get("description") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            problems.push("description: must be a string".to_owned());
            String::new()
        }
    };
    let default_value_column = match settings.get("default_value_column") {
        None | Some(Value::Null) => value_columns.first().cloned().unwrap_or_default(),
        Some(Value::String(text)) if text.is_empty() => {
            value_columns.first().cloned().unwrap_or_default()
        }
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            problems.push("default_value_column: must be a string".to_owned());
            String::new()
        }
    };
    let ttl_seconds = match settings.get("ttl_seconds") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_u64().map(u32::try_from) {
            Some(Ok(ttl)) => Some(ttl),
            Some(Err(_)) => Some(u32::MAX),
            None => {
                problems.push("ttl_seconds: must be a whole number".to_owned());
                None
            }
        },
    };
    let preload = settings.get("preload").is_some_and(truthy);

    let definition = TableDefinition {
        name: name.to_owned(),
        description,
        key_columns,
        value_columns,
        default_value_column,
        key_normalisation,
        ttl_seconds,
        preload,
    };
    if let Err(mut more) = definition.validate() {
        problems.append(&mut more);
    }
    if problems.is_empty() {
        Ok(definition)
    } else {
        Err(problems)
    }
}

/// A rule that upper-cases (or lower-cases) every key column, as the upload
/// form's "key case" selector builds it.
#[must_use]
pub fn case_rules(columns: &[String], case: Case) -> Value {
    let mut rules = Map::new();
    if case != Case::Preserve {
        for column in columns {
            rules.insert(column.clone(), serde_json::json!({ "case": case.as_str() }));
        }
    }
    Value::Object(rules)
}
