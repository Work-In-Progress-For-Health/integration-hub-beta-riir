//! A small CSV reader (RFC 4180) for the two import screens.
//!
//! Implements the parsing half of `spec/15-dashboard.md` §10.3.6 and
//! §10.4.4.
//!
//! # Why not a crate
//!
//! The workspace has no CSV dependency, and the two imports are a header row
//! and a few hundred short lines typed by support staff. Pulling a crate in
//! would be a change to the root `Cargo.toml` and to `deny.toml`'s review for
//! about sixty lines of state machine. What matters is that quoting is right
//! — a comma inside a quoted organisation name must not shift every column
//! after it — and the tests below are about exactly that.

use std::collections::BTreeMap;

/// A parsed file: the header names (lowercased, trimmed) and the rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Header names, lowercased and trimmed, in file order.
    pub headers: Vec<String>,
    /// One map per data row, from header to cell. A short row has the
    /// missing cells as empty strings, as Python's `DictReader` does after
    /// the `or ""` the callers apply.
    pub rows: Vec<BTreeMap<String, String>>,
}

/// Why a file was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CsvError {
    /// Nothing, or only blank lines.
    Empty,
}

/// Split text into records of fields.
fn records(text: &str) -> Vec<Vec<String>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    // Whether anything has been seen on this line, so a blank line is not a
    // record with one empty field.
    let mut started = false;

    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }

        match c {
            '"' => {
                quoted = true;
                started = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                started = true;
            }
            '\r' => {}
            '\n' => {
                if started {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                started = false;
            }
            other => {
                field.push(other);
                started = true;
            }
        }
    }

    if started {
        record.push(field);
        records.push(record);
    }

    records
}

/// Parse CSV text.
///
/// A leading byte order mark is ignored (the routes decode `utf-8-sig`).
/// Blank lines are skipped.
///
/// # Errors
///
/// [`CsvError::Empty`] when there is no header row.
///
/// # Examples
///
/// ```
/// use dashboard::csv::parse;
///
/// let table = parse("Name,Port\n\"Cardiff, North\",2575\n").unwrap();
///
/// assert_eq!(table.headers, ["name", "port"]);
/// assert_eq!(table.rows[0]["name"], "Cardiff, North");
/// ```
pub fn parse(text: &str) -> Result<Table, CsvError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut all = records(text).into_iter();

    let headers: Vec<String> = all
        .next()
        .ok_or(CsvError::Empty)?
        .into_iter()
        .map(|name| name.trim().to_lowercase())
        .collect();

    let rows = all
        .map(|record| {
            headers
                .iter()
                .enumerate()
                .filter(|(_, name)| !name.is_empty())
                .map(|(index, name)| (name.clone(), record.get(index).cloned().unwrap_or_default()))
                .collect()
        })
        .collect();

    Ok(Table { headers, rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comma_inside_quotes_does_not_shift_the_columns() {
        // The reason this module exists at all.
        let table = parse("a,b,c\n1,\"x, y\",3\n").unwrap();

        assert_eq!(table.rows[0]["a"], "1");
        assert_eq!(table.rows[0]["b"], "x, y");
        assert_eq!(table.rows[0]["c"], "3");
    }

    #[test]
    fn a_doubled_quote_is_one_quote_and_a_newline_can_be_quoted() {
        let table = parse("a,b\n\"say \"\"hi\"\"\",\"line1\nline2\"\n").unwrap();

        assert_eq!(table.rows[0]["a"], "say \"hi\"");
        assert_eq!(table.rows[0]["b"], "line1\nline2");
    }

    #[test]
    fn crlf_blank_lines_and_a_missing_final_newline_are_tolerated() {
        let table = parse("a,b\r\n\r\n1,2\r\n\r\n3,4").unwrap();

        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[1]["b"], "4");
    }

    #[test]
    fn headers_are_trimmed_and_lowercased_and_a_bom_is_ignored() {
        let table = parse("\u{feff} First_Name ,LAST_NAME\nA,B\n").unwrap();

        assert_eq!(table.headers, ["first_name", "last_name"]);
        assert_eq!(table.rows[0]["first_name"], "A");
    }

    #[test]
    fn a_short_row_has_empty_cells_and_extra_cells_are_ignored() {
        let table = parse("a,b,c\n1\n1,2,3,4\n").unwrap();

        assert_eq!(table.rows[0]["b"], "");
        assert_eq!(table.rows[0]["c"], "");
        assert_eq!(table.rows[1]["c"], "3");
    }

    #[test]
    fn nothing_at_all_is_empty() {
        assert_eq!(parse(""), Err(CsvError::Empty));
        assert_eq!(parse("\n\n"), Err(CsvError::Empty));
    }
}
