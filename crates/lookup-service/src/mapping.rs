//! Record mapping: how source records become canonical rows.
//!
//! Implements `spec/20-lookup-service.md` §6.3 and §6.4.
//!
//! Phase 1 implements the CSV subset: a field's `path` is a CSV column name
//! and only the `trim`/`upper`/`lower` transforms exist. The Python's
//! Phase 5 extends the same model with `XPath` and `JMESPath` paths.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::keys::normalise_part;
use crate::model::{Row, TableDefinition, Values};

/// The most problems listed (§6.4).
pub const DEFAULT_MAX_ERRORS: usize = 50;

/// A list of names as the messages spell it: `['a', 'b']`.
///
/// The Python's messages print a list this way and `spec/20` §6.3 quotes
/// them, so a message here reads the same to someone comparing the two.
#[must_use]
pub fn name_list<S: AsRef<str>>(items: &[S]) -> String {
    let quoted: Vec<String> = items.iter().map(|i| format!("'{}'", i.as_ref())).collect();
    format!("[{}]", quoted.join(", "))
}

/// A transform applied to a source value (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    /// Trim whitespace.
    Trim,
    /// Upper-case.
    Upper,
    /// Lower-case.
    Lower,
}

impl Transform {
    /// Parse the JSON spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "trim" => Some(Self::Trim),
            "upper" => Some(Self::Upper),
            "lower" => Some(Self::Lower),
            _ => None,
        }
    }
}

/// Apply transforms in order.
#[must_use]
pub fn apply_transforms(value: &str, transforms: &[Transform]) -> String {
    let mut value = value.to_owned();
    for transform in transforms {
        value = match transform {
            Transform::Trim => value.trim().to_owned(),
            Transform::Upper => value.to_uppercase(),
            Transform::Lower => value.to_lowercase(),
        };
    }
    value
}

/// Where one target column is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSpec {
    /// The CSV column name.
    pub path: String,
    /// Transforms, in order.
    pub transforms: Vec<Transform>,
}

impl FieldSpec {
    /// A field read as-is.
    #[must_use]
    pub fn plain(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            transforms: Vec::new(),
        }
    }

    /// A field with transforms.
    #[must_use]
    pub fn with(path: impl Into<String>, transforms: &[Transform]) -> Self {
        Self {
            path: path.into(),
            transforms: transforms.to_vec(),
        }
    }
}

/// Maps each table column (target) to a field in the source record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordMapping {
    /// Key column to field.
    pub key: BTreeMap<String, FieldSpec>,
    /// Value column to field.
    pub values: BTreeMap<String, FieldSpec>,
}

impl RecordMapping {
    /// Each table column is read from the source field of the same name.
    #[must_use]
    pub fn identity(definition: &TableDefinition) -> Self {
        Self {
            key: definition
                .key_columns
                .iter()
                .map(|c| (c.clone(), FieldSpec::plain(c)))
                .collect(),
            values: definition
                .value_columns
                .iter()
                .map(|c| (c.clone(), FieldSpec::plain(c)))
                .collect(),
        }
    }

    /// Read a mapping from the upload's JSON, without echoing any value.
    ///
    /// # Errors
    ///
    /// One message per problem, as `<path>: <reason>`.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, Vec<String>> {
        let Some(object) = value.as_object() else {
            return Err(vec!["mapping: must be an object".to_owned()]);
        };
        let mut problems = Vec::new();
        let mut section = |name: &str| -> BTreeMap<String, FieldSpec> {
            let mut out = BTreeMap::new();
            match object.get(name) {
                None => problems.push(format!("{name}: field required")),
                Some(serde_json::Value::Object(columns)) => {
                    for (column, spec) in columns {
                        match field_spec(spec, &format!("{name}.{column}")) {
                            Ok(spec) => {
                                out.insert(column.clone(), spec);
                            }
                            Err(mut more) => problems.append(&mut more),
                        }
                    }
                }
                Some(_) => problems.push(format!("{name}: must be an object")),
            }
            out
        };
        let key = section("key");
        let values = section("values");
        for name in object.keys() {
            if name != "key" && name != "values" {
                problems.push(format!("{name}: unknown field"));
            }
        }
        if problems.is_empty() {
            Ok(Self { key, values })
        } else {
            Err(problems)
        }
    }

    /// Problems where the mapping's targets do not match the table's
    /// columns (§6.3).
    #[must_use]
    pub fn check_targets(&self, definition: &TableDefinition) -> Vec<String> {
        let mut problems = Vec::new();
        let keys: BTreeSet<&String> = self.key.keys().collect();
        if keys != definition.key_columns.iter().collect() {
            problems.push(format!(
                "mapping key targets {} must equal key columns {}",
                name_list(&self.key.keys().collect::<Vec<_>>()),
                name_list(&definition.key_columns)
            ));
        }
        let values: BTreeSet<&String> = self.values.keys().collect();
        if values != definition.value_columns.iter().collect() {
            problems.push(format!(
                "mapping value targets {} must equal value columns {}",
                name_list(&self.values.keys().collect::<Vec<_>>()),
                name_list(&definition.value_columns)
            ));
        }
        problems
    }

    /// Every source field the mapping reads.
    #[must_use]
    pub fn source_fields(&self) -> BTreeSet<&str> {
        self.key
            .values()
            .chain(self.values.values())
            .map(|spec| spec.path.as_str())
            .collect()
    }
}

/// One field of a mapping, from JSON.
fn field_spec(value: &serde_json::Value, path: &str) -> Result<FieldSpec, Vec<String>> {
    let Some(object) = value.as_object() else {
        return Err(vec![format!("{path}: must be an object")]);
    };
    let mut problems = Vec::new();
    let mut field_path = None;
    let mut transforms = Vec::new();
    for (name, field) in object {
        match name.as_str() {
            "path" => match field.as_str() {
                Some(text) if (1..=200).contains(&text.chars().count()) => {
                    field_path = Some(text.to_owned());
                }
                _ => problems.push(format!(
                    "{path}.path: must be a string of 1 to 200 characters"
                )),
            },
            "transforms" => match field.as_array() {
                Some(list) => {
                    for (index, item) in list.iter().enumerate() {
                        match item.as_str().and_then(Transform::parse) {
                            Some(transform) => transforms.push(transform),
                            None => problems.push(format!(
                                "{path}.transforms.{index}: must be one of trim, upper, lower"
                            )),
                        }
                    }
                }
                None => problems.push(format!("{path}.transforms: must be a list")),
            },
            _ => problems.push(format!("{path}.{name}: unknown field")),
        }
    }
    if field_path.is_none() && problems.is_empty() {
        problems.push(format!("{path}.path: field required"));
    }
    match (field_path, problems.is_empty()) {
        (Some(path), true) => Ok(FieldSpec { path, transforms }),
        _ => Err(problems),
    }
}

/// A problem with one record, located by line and never carrying a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordError {
    /// The physical line the record ends on.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl RecordError {
    /// A problem at a line.
    #[must_use]
    pub fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// The outcome of mapping a file: rows in file order, and the problems.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MappingResult {
    /// Accepted rows, in file order.
    pub rows: Vec<Row>,
    /// Problems, at most [`DEFAULT_MAX_ERRORS`].
    pub errors: Vec<RecordError>,
    /// Whether more problems were suppressed.
    pub truncated: bool,
}

impl MappingResult {
    /// The problems as messages, with a closing line when truncated (§6.4).
    #[must_use]
    pub fn error_messages(&self) -> Vec<String> {
        let mut messages: Vec<String> = self.errors.iter().map(ToString::to_string).collect();
        if self.truncated {
            messages.push(format!(
                "... further errors suppressed after the first {}",
                self.errors.len()
            ));
        }
        messages
    }
}

/// Map (line, cells) records to rows keyed by canonical key.
///
/// Key parts get the mapping's transforms and then the table's key
/// normalisation, the same normalisation applied on lookup. Errors carry
/// line numbers only, never data values (§6.4).
#[must_use]
pub fn map_records(
    definition: &TableDefinition,
    mapping: &RecordMapping,
    header: &[String],
    records: &[(usize, Vec<String>)],
    max_errors: usize,
) -> MappingResult {
    let mut result = MappingResult::default();
    let mut first_seen: HashMap<Vec<String>, usize> = HashMap::new();
    let column_index = |path: &str| header.iter().position(|name| name == path);

    for (line, cells) in records {
        let add_error = |result: &mut MappingResult, message: String| {
            if result.errors.len() < max_errors {
                result.errors.push(RecordError::new(*line, message));
            } else {
                result.truncated = true;
            }
        };

        let mut key_parts = Vec::with_capacity(definition.key_columns.len());
        for column in &definition.key_columns {
            let Some(spec) = mapping.key.get(column) else {
                break;
            };
            let Some(raw) = column_index(&spec.path).and_then(|i| cells.get(i)) else {
                add_error(&mut result, format!("missing field '{}'", spec.path));
                break;
            };
            let part = normalise_part(
                &apply_transforms(raw, &spec.transforms),
                &definition.rule_for(column),
            );
            if part.is_empty() {
                add_error(&mut result, format!("empty key part '{column}'"));
                break;
            }
            key_parts.push(part);
        }
        if key_parts.len() != definition.key_columns.len() {
            continue;
        }

        if let Some(first) = first_seen.get(&key_parts) {
            add_error(
                &mut result,
                format!("duplicate key (first seen on line {first})"),
            );
            continue;
        }

        let mut values = Vec::with_capacity(definition.value_columns.len());
        for column in &definition.value_columns {
            let Some(spec) = mapping.values.get(column) else {
                break;
            };
            let Some(raw) = column_index(&spec.path).and_then(|i| cells.get(i)) else {
                add_error(&mut result, format!("missing field '{}'", spec.path));
                break;
            };
            values.push((column.clone(), apply_transforms(raw, &spec.transforms)));
        }
        if values.len() != definition.value_columns.len() {
            continue;
        }

        first_seen.insert(key_parts.clone(), *line);
        result.rows.push(Row {
            key: key_parts,
            values: Values::new(values),
        });
    }
    result
}
