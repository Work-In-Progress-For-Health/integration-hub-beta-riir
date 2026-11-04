//! Contacts: who to call about a flow.
//!
//! Implements `spec/15-dashboard.md` §10.3 (upstream `e0c9d88`).
//!
//! # Personal data, but not patient data
//!
//! A contact is a named person with an e-mail address and a phone number.
//! They are shown on a page — that is the feature — and nowhere else:
//! §10.3.8. Every log line here carries a contact's `id` and nothing else
//! about them, and an error message names a *field*, never a value.

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::alarm::documents::CONTACT;
use crate::alarm::store::{AlarmStore, StoreError};
use crate::csv;
use crate::flows::flow;

/// The Cosmos partition contacts live in (§10.3.1).
pub const PARTITION: &str = "contacts";

/// The values `organisation_type` may take (§10.3.2).
pub const ORGANISATION_TYPES: [&str; 3] = ["Team", "Health Board", "Supplier"];

/// Contacts per page unless asked otherwise (§10.3.5).
pub const DEFAULT_PAGE_SIZE: usize = 25;

const MAX_NAME_LENGTH: usize = 100;
const MAX_ORGANISATION_LENGTH: usize = 200;
const MAX_NOTES_LENGTH: usize = 2000;

const NOT_CONFIGURED: &str = "Contact persistence is not configured.";
const UNAVAILABLE: &str =
    "Contact persistence is currently unavailable — please try again shortly.";

/// Why a contact operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactError {
    /// The input failed validation. Safe to show.
    Invalid(String),
    /// No contact has that id.
    NotFound,
    /// Nothing could be stored or read. Safe to show.
    Persistence(String),
}

impl ContactError {
    /// The message for a response body.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Invalid(message) | Self::Persistence(message) => message,
            Self::NotFound => "Contact not found.",
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl std::fmt::Display for ContactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

fn persistence(error: &StoreError) -> ContactError {
    // The store's error names a status and a URL, never a contact.
    tracing::error!(%error, "contact persistence failed");
    ContactError::Persistence(UNAVAILABLE.to_string())
}

/// A contact, as the API presents it (§10.3.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Contact {
    /// `contact_id` in storage.
    pub id: String,
    /// Given name.
    pub first_name: String,
    /// Family name.
    pub last_name: String,
    /// Lowercased; may be empty when there is a phone number.
    pub email: String,
    /// May be empty when there is an e-mail address.
    pub phone_number: String,
    /// One of [`ORGANISATION_TYPES`].
    pub organisation_type: String,
    /// Which organisation.
    pub organisation_name: String,
    /// Free text.
    pub notes: String,
    /// The flows they can speak to.
    pub flow_ids: Vec<String>,
    /// Soft-deleted (§10.3.3).
    pub deleted: bool,
    /// When it was soft-deleted, RFC 3339.
    pub deleted_at: Option<String>,
    /// When it was made, RFC 3339.
    pub created_at: Option<String>,
    /// When it last changed, RFC 3339.
    pub updated_at: Option<String>,
}

/// The validated, user-editable part of a contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fields {
    /// Given name.
    pub first_name: String,
    /// Family name.
    pub last_name: String,
    /// Lowercased.
    pub email: String,
    /// As typed, trimmed.
    pub phone_number: String,
    /// One of [`ORGANISATION_TYPES`].
    pub organisation_type: String,
    /// Which organisation.
    pub organisation_name: String,
    /// Free text.
    pub notes: String,
    /// Known flows, de-duplicated.
    pub flow_ids: Vec<String>,
}

fn name(value: Option<&Value>, label: &str, max: usize) -> Result<String, ContactError> {
    let text = match value {
        None | Some(Value::Null) => "",
        Some(Value::String(text)) => text.trim(),
        Some(_) => return Err(ContactError::invalid(format!("{label} must be a string"))),
    };
    if text.is_empty() {
        return Err(ContactError::invalid(format!("{label} must not be empty")));
    }
    if text.chars().count() > max {
        return Err(ContactError::invalid(format!(
            "{label} is too long (max {max} characters)"
        )));
    }
    Ok(text.to_string())
}

/// An optional string: absent or null is empty; anything else must be text.
fn optional(value: Option<&Value>, label: &str) -> Result<String, ContactError> {
    match value {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.trim().to_string()),
        Some(_) => Err(ContactError::invalid(format!("{label} must be a string"))),
    }
}

/// `^[^@\s.]+(\.[^@\s.]+)*@[^@\s.]+(\.[^@\s.]+)+$`, without a regex engine.
///
/// The local part is dot-separated runs of characters that are none of `@`,
/// whitespace or `.`; the domain is the same with at least one dot. That
/// rules out a leading, trailing or doubled dot on either side.
fn is_email(text: &str) -> bool {
    let plain = |c: char| c != '@' && c != '.' && !c.is_whitespace();
    let runs = |part: &str| {
        part.split('.')
            .all(|run| !run.is_empty() && run.chars().all(plain))
    };

    let mut halves = text.split('@');
    let (Some(local), Some(domain), None) = (halves.next(), halves.next(), halves.next()) else {
        return false;
    };

    runs(local) && runs(domain) && domain.contains('.')
}

fn is_phone(text: &str) -> bool {
    (5..=30).contains(&text.chars().count())
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '(' | ')' | '-') || c.is_whitespace())
}

fn flow_ids(value: Option<&Value>) -> Result<Vec<String>, ContactError> {
    let list = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => {
            return Err(ContactError::invalid(
                "Linked flows must be a list of flow ids",
            ));
        }
    };

    let mut ids: Vec<String> = Vec::new();
    for item in list {
        let Some(text) = item.as_str() else {
            return Err(ContactError::invalid(
                "Linked flows must be a list of flow ids",
            ));
        };
        let text = text.trim();
        if !text.is_empty() {
            ids.push(text.to_string());
        }
    }

    // §10.3.2 — against the closed set of §3.1. Named, so a CSV row with a
    // typo says which flow it got wrong; a flow id is not personal data.
    let unknown: Vec<&str> = ids
        .iter()
        .map(String::as_str)
        .filter(|id| flow(id).is_none())
        .collect();
    if !unknown.is_empty() {
        return Err(ContactError::invalid(format!(
            "Unknown flow id(s): {}",
            unknown.join(", ")
        )));
    }

    let mut deduped: Vec<String> = Vec::new();
    for id in ids {
        if !deduped.contains(&id) {
            deduped.push(id);
        }
    }
    Ok(deduped)
}

/// Validate every field, reporting the first problem (§10.3.2).
///
/// # Errors
///
/// [`ContactError::Invalid`], naming the field and never its value.
pub fn validate(data: &Value) -> Result<Fields, ContactError> {
    let first_name = name(data.get("first_name"), "First name", MAX_NAME_LENGTH)?;
    let last_name = name(data.get("last_name"), "Last name", MAX_NAME_LENGTH)?;

    let email = optional(data.get("email"), "Email")?.to_lowercase();
    if !email.is_empty() && !is_email(&email) {
        return Err(ContactError::invalid("Email is not a valid email address"));
    }

    let phone_number = optional(data.get("phone_number"), "Phone number")?;
    if !phone_number.is_empty() && !is_phone(&phone_number) {
        return Err(ContactError::invalid(
            "Phone number may only contain digits, spaces, +, (), and -",
        ));
    }

    if email.is_empty() && phone_number.is_empty() {
        return Err(ContactError::invalid(
            "Either an email address or a phone number is required",
        ));
    }

    let organisation_type = data
        .get("organisation_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| ORGANISATION_TYPES.contains(t))
        .ok_or_else(|| {
            ContactError::invalid(format!(
                "Organisation type must be one of: {}",
                ORGANISATION_TYPES.join(", ")
            ))
        })?
        .to_string();

    let organisation_name = name(
        data.get("organisation_name"),
        "Organisation name",
        MAX_ORGANISATION_LENGTH,
    )?;

    let notes = optional(data.get("notes"), "Notes")?;
    if notes.chars().count() > MAX_NOTES_LENGTH {
        return Err(ContactError::invalid(format!(
            "Notes are too long (max {MAX_NOTES_LENGTH} characters)"
        )));
    }

    Ok(Fields {
        first_name,
        last_name,
        email,
        phone_number,
        organisation_type,
        organisation_name,
        notes,
        flow_ids: flow_ids(data.get("flow_ids"))?,
    })
}

fn stamp_time(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

impl Contact {
    fn from_document(document: &Value) -> Option<Self> {
        let text = |name: &str| {
            document
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let optional_text = |name: &str| {
            document
                .get(name)
                .and_then(Value::as_str)
                .map(ToString::to_string)
        };
        let id = text("contact_id");
        if id.is_empty() {
            return None;
        }

        Some(Self {
            id,
            first_name: text("first_name"),
            last_name: text("last_name"),
            email: text("email"),
            phone_number: text("phone_number"),
            organisation_type: text("organisation_type"),
            organisation_name: text("organisation_name"),
            notes: text("notes"),
            flow_ids: document
                .get("flow_ids")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|v| v.as_str().map(ToString::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            deleted: document
                .get("deleted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            deleted_at: optional_text("deleted_at"),
            created_at: optional_text("created_at"),
            updated_at: optional_text("updated_at"),
        })
    }

    /// The document to store: `contact_id`, never `id` (§10.1.3).
    fn to_document(&self) -> Value {
        serde_json::json!({
            "contact_id": self.id,
            "first_name": self.first_name,
            "last_name": self.last_name,
            "email": self.email,
            "phone_number": self.phone_number,
            "organisation_type": self.organisation_type,
            "organisation_name": self.organisation_name,
            "notes": self.notes,
            "flow_ids": self.flow_ids,
            "deleted": self.deleted,
            "deleted_at": self.deleted_at,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }

    fn apply(&mut self, fields: Fields) {
        self.first_name = fields.first_name;
        self.last_name = fields.last_name;
        self.email = fields.email;
        self.phone_number = fields.phone_number;
        self.organisation_type = fields.organisation_type;
        self.organisation_name = fields.organisation_name;
        self.notes = fields.notes;
        self.flow_ids = fields.flow_ids;
    }

    fn haystack(&self) -> String {
        [
            &self.first_name,
            &self.last_name,
            &self.email,
            &self.phone_number,
            &self.organisation_name,
            &self.organisation_type,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
    }
}

/// Every stored contact, soft-deleted ones included.
///
/// # Errors
///
/// [`ContactError::Persistence`] when the partition could not be read — not
/// an empty list (§10.1.4).
pub async fn list_all<S: AlarmStore>(store: &S) -> Result<Vec<Contact>, ContactError> {
    Ok(store
        .documents(PARTITION)
        .await
        .map_err(|error| persistence(&error))?
        .iter()
        .filter_map(Contact::from_document)
        .collect())
}

/// What `GET /api/contacts` asked for (§10.3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// Case-insensitive substring.
    pub search: Option<String>,
    /// Only contacts linked to this flow.
    pub flow_id: Option<String>,
    /// 1-based.
    pub page: usize,
    /// At least 1.
    pub per_page: usize,
    /// Whether soft-deleted contacts are included.
    pub include_deleted: bool,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            search: None,
            flow_id: None,
            page: 1,
            per_page: DEFAULT_PAGE_SIZE,
            include_deleted: true,
        }
    }
}

/// Filter, sort and paginate (§10.3.5), in memory: the partition is tens to
/// low hundreds of contacts.
#[must_use]
pub fn select(mut contacts: Vec<Contact>, query: &Query) -> (Vec<Contact>, usize) {
    if !query.include_deleted {
        contacts.retain(|c| !c.deleted);
    }
    if let Some(flow_id) = &query.flow_id {
        contacts.retain(|c| c.flow_ids.contains(flow_id));
    }
    if let Some(search) = query.search.as_deref().filter(|s| !s.is_empty()) {
        let needle = search.to_lowercase();
        contacts.retain(|c| c.haystack().contains(&needle));
    }

    contacts.sort_by_key(|c| (c.last_name.to_lowercase(), c.first_name.to_lowercase()));

    let total = contacts.len();
    let per_page = query.per_page.max(1);
    let start = (query.page.max(1) - 1).saturating_mul(per_page);

    (
        contacts.into_iter().skip(start).take(per_page).collect(),
        total,
    )
}

/// Non-deleted contacts grouped by linked flow — one read for the whole
/// Flows page rather than one per card (§10.3.3).
#[must_use]
pub fn by_flow(contacts: Vec<Contact>) -> std::collections::BTreeMap<String, Vec<Contact>> {
    let (live, _) = select(
        contacts,
        &Query {
            per_page: usize::MAX,
            include_deleted: false,
            ..Query::default()
        },
    );

    let mut grouped: std::collections::BTreeMap<String, Vec<Contact>> =
        std::collections::BTreeMap::new();
    for contact in live {
        for flow_id in &contact.flow_ids {
            grouped
                .entry(flow_id.clone())
                .or_default()
                .push(contact.clone());
        }
    }
    grouped
}

fn require_persistence<S: AlarmStore>(store: &S) -> Result<(), ContactError> {
    if store.is_enabled() {
        Ok(())
    } else {
        Err(ContactError::Persistence(NOT_CONFIGURED.to_string()))
    }
}

async fn persist<S: AlarmStore>(store: &S, contact: &Contact) -> Result<(), ContactError> {
    store
        .upsert_document(PARTITION, &contact.id, CONTACT, &contact.to_document())
        .await
        .map_err(|error| persistence(&error))
}

/// Validate and store a new contact.
///
/// # Errors
///
/// [`ContactError::Invalid`] or [`ContactError::Persistence`].
pub async fn add<S: AlarmStore>(
    store: &S,
    data: &Value,
    now: OffsetDateTime,
) -> Result<Contact, ContactError> {
    let fields = validate(data)?;
    require_persistence(store)?;

    let mut contact = Contact {
        id: uuid::Uuid::new_v4().to_string(),
        first_name: String::new(),
        last_name: String::new(),
        email: String::new(),
        phone_number: String::new(),
        organisation_type: String::new(),
        organisation_name: String::new(),
        notes: String::new(),
        flow_ids: Vec::new(),
        deleted: false,
        deleted_at: None,
        created_at: Some(stamp_time(now)),
        updated_at: Some(stamp_time(now)),
    };
    contact.apply(fields);

    persist(store, &contact).await?;
    tracing::info!(contact_id = %contact.id, "contact added");

    Ok(contact)
}

/// Validate and overwrite a contact, keeping its deletion state and
/// creation time.
///
/// # Errors
///
/// [`ContactError::Invalid`] — including "Contact not found.", which is
/// upstream's behaviour (§10.3.9) — or [`ContactError::Persistence`].
pub async fn update<S: AlarmStore>(
    store: &S,
    id: &str,
    data: &Value,
    now: OffsetDateTime,
) -> Result<Contact, ContactError> {
    let mut contact = list_all(store)
        .await?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| ContactError::invalid("Contact not found."))?;

    let fields = validate(data)?;
    require_persistence(store)?;

    contact.apply(fields);
    contact.created_at.get_or_insert_with(|| stamp_time(now));
    contact.updated_at = Some(stamp_time(now));

    persist(store, &contact).await?;
    tracing::info!(contact_id = %contact.id, "contact updated");

    Ok(contact)
}

/// Soft-delete (§10.3.3). Deleting one that is not there is a no-op.
///
/// # Errors
///
/// [`ContactError::Persistence`].
pub async fn delete<S: AlarmStore>(
    store: &S,
    id: &str,
    now: OffsetDateTime,
) -> Result<(), ContactError> {
    let Some(mut contact) = list_all(store).await?.into_iter().find(|c| c.id == id) else {
        return Ok(());
    };
    require_persistence(store)?;

    contact.deleted = true;
    contact.deleted_at = Some(stamp_time(now));
    contact.updated_at = Some(stamp_time(now));

    persist(store, &contact).await?;
    tracing::info!(contact_id = %contact.id, "contact deleted");

    Ok(())
}

/// Reverse a soft delete, starting a fresh retention window if it is
/// deleted again (§10.3.3).
///
/// # Errors
///
/// [`ContactError::NotFound`] or [`ContactError::Persistence`].
pub async fn restore<S: AlarmStore>(
    store: &S,
    id: &str,
    now: OffsetDateTime,
) -> Result<Contact, ContactError> {
    let mut contact = list_all(store)
        .await?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or(ContactError::NotFound)?;
    require_persistence(store)?;

    contact.deleted = false;
    contact.deleted_at = None;
    contact.updated_at = Some(stamp_time(now));

    persist(store, &contact).await?;
    tracing::info!(contact_id = %contact.id, "contact restored");

    Ok(contact)
}

/// One contact by id.
///
/// # Errors
///
/// [`ContactError::Persistence`].
pub async fn get<S: AlarmStore>(store: &S, id: &str) -> Result<Option<Contact>, ContactError> {
    Ok(list_all(store).await?.into_iter().find(|c| c.id == id))
}

/// Permanently delete contacts soft-deleted for longer than `retention_days`
/// (§10.3.4). Returns how many.
///
/// A contact with neither `deleted_at` nor `updated_at` readable is left
/// alone: this is the one irreversible operation here, and purging on a
/// guess is the one mistake that cannot be put right.
pub async fn purge_expired<S: AlarmStore>(
    store: &S,
    retention_days: u32,
    now: OffsetDateTime,
) -> usize {
    if !store.is_enabled() {
        return 0;
    }
    let Ok(contacts) = list_all(store).await else {
        return 0;
    };
    let cutoff = now - time::Duration::days(i64::from(retention_days));

    let mut purged = 0;
    for contact in contacts.iter().filter(|c| c.deleted) {
        let when = [&contact.deleted_at, &contact.updated_at]
            .into_iter()
            .flatten()
            .find_map(|text| OffsetDateTime::parse(text, &Rfc3339).ok());
        let Some(when) = when else { continue };
        if when > cutoff {
            continue;
        }

        match store.delete_document(PARTITION, &contact.id).await {
            Ok(()) => purged += 1,
            Err(error) => {
                tracing::warn!(%error, contact_id = %contact.id, "could not purge an expired contact");
            }
        }
    }

    purged
}

/// What an import did (§10.3.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ImportResult {
    /// New contacts.
    pub created: usize,
    /// Existing contacts updated.
    pub updated: usize,
    /// One line per problem, `Row N: …`.
    pub errors: Vec<String>,
    /// A write failed, so the import stopped.
    pub persistence_failed: bool,
}

const REQUIRED_COLUMNS: [&str; 4] = [
    "first_name",
    "last_name",
    "organisation_type",
    "organisation_name",
];

/// Parse a CSV into validated rows and the errors of the rest (§10.3.6).
#[must_use]
pub fn parse_csv(content: &str) -> (Vec<Fields>, Vec<String>) {
    let Ok(table) = csv::parse(content) else {
        return (Vec::new(), vec!["CSV file is empty.".to_string()]);
    };

    let missing: Vec<&str> = REQUIRED_COLUMNS
        .into_iter()
        .filter(|column| !table.headers.iter().any(|h| h == column))
        .collect();
    if !missing.is_empty() {
        return (
            Vec::new(),
            vec![format!(
                "CSV header is missing required column(s): {}",
                missing.join(", ")
            )],
        );
    }

    let mut valid = Vec::new();
    let mut errors = Vec::new();
    for (index, row) in table.rows.iter().enumerate() {
        // Row 1 is the header.
        let row_number = index + 2;
        let cell = |column: &str| row.get(column).cloned().unwrap_or_default();
        let flow_ids: Vec<Value> = cell("flow_ids")
            .split(';')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(|f| Value::String(f.to_string()))
            .collect();

        let data = serde_json::json!({
            "first_name": cell("first_name"),
            "last_name": cell("last_name"),
            "email": cell("email"),
            "phone_number": cell("phone_number"),
            "organisation_type": cell("organisation_type"),
            "organisation_name": cell("organisation_name"),
            "notes": cell("notes"),
            "flow_ids": flow_ids,
        });

        match validate(&data) {
            Ok(fields) => valid.push(fields),
            Err(error) => errors.push(format!("Row {row_number}: {}", error.message())),
        }
    }

    (valid, errors)
}

/// The natural key an imported row is matched on: e-mail, else phone
/// (§10.3.6).
fn dedupe_key(email: &str, phone_number: &str) -> Option<String> {
    if !email.is_empty() {
        Some(format!("email:{}", email.to_lowercase()))
    } else if !phone_number.is_empty() {
        Some(format!("phone:{phone_number}"))
    } else {
        None
    }
}

/// Import a CSV: create, or update the contact it matches (§10.3.6).
pub async fn import<S: AlarmStore>(store: &S, content: &str, now: OffsetDateTime) -> ImportResult {
    let (rows, mut errors) = parse_csv(content);

    if let Err(error) = require_persistence(store) {
        errors.push(error.message().to_string());
        return ImportResult {
            errors,
            persistence_failed: true,
            ..ImportResult::default()
        };
    }

    let mut result = ImportResult::default();
    let mut by_key: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    match list_all(store).await {
        Ok(existing) => {
            for contact in existing {
                if let Some(key) = dedupe_key(&contact.email, &contact.phone_number) {
                    by_key.insert(key, contact.id);
                }
            }
        }
        Err(error) => {
            errors.push(error.message().to_string());
            return ImportResult {
                errors,
                persistence_failed: true,
                ..ImportResult::default()
            };
        }
    }

    for fields in rows {
        let key = dedupe_key(&fields.email, &fields.phone_number);
        let data = serde_json::json!({
            "first_name": fields.first_name,
            "last_name": fields.last_name,
            "email": fields.email,
            "phone_number": fields.phone_number,
            "organisation_type": fields.organisation_type,
            "organisation_name": fields.organisation_name,
            "notes": fields.notes,
            "flow_ids": fields.flow_ids,
        });

        let matched = key.as_ref().and_then(|k| by_key.get(k)).cloned();
        let outcome = match &matched {
            Some(id) => update(store, id, &data, now).await.map(|c| (c, false)),
            None => add(store, &data, now).await.map(|c| (c, true)),
        };

        match outcome {
            Ok((contact, created)) => {
                if created {
                    result.created += 1;
                } else {
                    result.updated += 1;
                }
                // So a later row with the same key updates this one instead
                // of creating a duplicate.
                if let Some(key) = key {
                    by_key.insert(key, contact.id);
                }
            }
            Err(ContactError::Persistence(message)) => {
                // The label is a name; it stays out of the response and the
                // log, which carry the row's position instead.
                errors.push(message);
                result.persistence_failed = true;
                break;
            }
            Err(error) => errors.push(error.message().to_string()),
        }
    }

    result.errors = errors;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::store::{Disabled, InMemory};
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-10-01 09:00 UTC);

    fn input() -> Value {
        serde_json::json!({
            "first_name": " Ada ",
            "last_name": "Lovelace",
            "email": "Ada.Lovelace@Example.org",
            "phone_number": "+44 29 2000 0000",
            "organisation_type": "Supplier",
            "organisation_name": "Analytical Engines Ltd",
            "notes": "",
            "flow_ids": ["phw-to-mpi", "phw-to-mpi", " ", "paris-to-mpi"],
        })
    }

    #[test]
    fn a_valid_contact_is_cleaned() {
        // spec/15-dashboard.md §10.3.2.
        let fields = validate(&input()).unwrap();

        assert_eq!(fields.first_name, "Ada", "trimmed");
        assert_eq!(fields.email, "ada.lovelace@example.org", "lowercased");
        assert_eq!(
            fields.flow_ids,
            ["phw-to-mpi", "paris-to-mpi"],
            "deduped, blanks dropped"
        );
    }

    #[test]
    fn each_field_has_its_own_refusal() {
        let refused = |change: Value, fragment: &str| {
            let mut data = input();
            for (key, value) in change.as_object().unwrap() {
                data[key] = value.clone();
            }
            let error = validate(&data).unwrap_err();
            assert!(error.message().contains(fragment), "{error}");
        };

        refused(
            serde_json::json!({"first_name": ""}),
            "First name must not be empty",
        );
        refused(
            serde_json::json!({"first_name": "x".repeat(101)}),
            "too long",
        );
        refused(
            serde_json::json!({"last_name": 5}),
            "Last name must be a string",
        );
        refused(serde_json::json!({"email": "not-an-email"}), "valid email");
        refused(
            serde_json::json!({"phone_number": "abc"}),
            "Phone number may only",
        );
        refused(
            serde_json::json!({"phone_number": "123"}),
            "Phone number may only",
        );
        refused(
            serde_json::json!({"organisation_type": "Charity"}),
            "must be one of",
        );
        refused(
            serde_json::json!({"organisation_name": " "}),
            "Organisation name must not be empty",
        );
        refused(
            serde_json::json!({"notes": "x".repeat(2001)}),
            "Notes are too long",
        );
        refused(
            serde_json::json!({"flow_ids": "phw-to-mpi"}),
            "list of flow ids",
        );
        refused(
            serde_json::json!({"flow_ids": ["no-such-flow"]}),
            "Unknown flow id(s): no-such-flow",
        );
    }

    #[test]
    fn a_contact_nobody_can_reach_is_refused() {
        // §10.3.2 -- email or phone, at least one.
        let mut data = input();
        data["email"] = Value::Null;
        data["phone_number"] = Value::String(String::new());

        assert!(
            validate(&data)
                .unwrap_err()
                .message()
                .contains("Either an email")
        );

        data["phone_number"] = Value::String("029 2000 0000".to_string());
        assert!(validate(&data).is_ok(), "a phone number alone is enough");
    }

    #[test]
    fn email_shapes() {
        for good in ["a@b.co", "a.b@c.d.e", "first.last+tag@example.org"] {
            assert!(is_email(good), "{good}");
        }
        for bad in [
            "a@b",
            "@b.co",
            "a@.co",
            "a..b@c.co",
            "a@b..co",
            "a b@c.co",
            "a@b@c.co",
            ".a@b.co",
            "a.@b.co",
            "a@b.co.",
        ] {
            assert!(!is_email(bad), "{bad}");
        }
    }

    #[test]
    fn the_error_names_the_field_and_never_the_value() {
        // §10.3.8 -- an error message is a log line waiting to happen.
        let mut data = input();
        data["email"] = Value::String("secret.person@@example.org".to_string());

        let error = validate(&data).unwrap_err();

        assert!(!error.message().contains("secret"), "{error}");
    }

    #[tokio::test]
    async fn a_contact_is_stored_typed_with_its_own_id_field() {
        // §10.1.2, §10.1.3, §10.3.1.
        let store = InMemory::new();

        let contact = add(&store, &input(), NOW).await.unwrap();

        let stored = store.raw_document(PARTITION, &contact.id).unwrap();
        assert_eq!(stored["type"], "contact");
        assert_eq!(stored["contact_id"], contact.id.as_str());

        let read = list_all(&store).await.unwrap();
        assert_eq!(
            read,
            vec![contact],
            "it survives the round trip with its id"
        );
    }

    #[tokio::test]
    async fn a_contact_is_refused_when_nothing_is_persisted() {
        // §10.3.7.
        let error = add(&Disabled, &input(), NOW).await.unwrap_err();

        assert_eq!(error, ContactError::Persistence(NOT_CONFIGURED.to_string()));
    }

    #[tokio::test]
    async fn update_keeps_creation_and_deletion_state() {
        let store = InMemory::new();
        let contact = add(&store, &input(), NOW).await.unwrap();
        delete(&store, &contact.id, NOW).await.unwrap();

        let mut change = input();
        change["first_name"] = Value::String("Augusta".to_string());
        let later = NOW + time::Duration::hours(1);
        let updated = update(&store, &contact.id, &change, later).await.unwrap();

        assert_eq!(updated.first_name, "Augusta");
        assert!(updated.deleted, "an edit does not undelete");
        assert_eq!(updated.created_at, contact.created_at);
        assert_eq!(updated.updated_at, Some(stamp_time(later)));
    }

    #[tokio::test]
    async fn updating_a_contact_that_is_not_there_is_the_pythons_400() {
        // §10.3.9.
        let error = update(&InMemory::new(), "ghost", &input(), NOW)
            .await
            .unwrap_err();

        assert_eq!(
            error,
            ContactError::Invalid("Contact not found.".to_string())
        );
    }

    #[tokio::test]
    async fn delete_is_soft_and_restore_reverses_it() {
        // §10.3.3.
        let store = InMemory::new();
        let contact = add(&store, &input(), NOW).await.unwrap();

        delete(&store, &contact.id, NOW).await.unwrap();
        let deleted = get(&store, &contact.id).await.unwrap().unwrap();
        assert!(deleted.deleted);
        assert!(deleted.deleted_at.is_some());

        let restored = restore(&store, &contact.id, NOW).await.unwrap();
        assert!(!restored.deleted);
        assert_eq!(
            restored.deleted_at, None,
            "a fresh retention window next time"
        );

        delete(&store, "ghost", NOW).await.unwrap();
        assert_eq!(
            restore(&store, "ghost", NOW).await.unwrap_err(),
            ContactError::NotFound
        );
    }

    #[tokio::test]
    async fn the_purge_removes_only_the_long_deleted_and_never_on_a_guess() {
        // §10.3.4.
        let store = InMemory::new();
        let old = add(&store, &input(), NOW - time::Duration::days(200))
            .await
            .unwrap();
        delete(&store, &old.id, NOW - time::Duration::days(100))
            .await
            .unwrap();

        let mut other = input();
        other["email"] = Value::String("b@example.org".to_string());
        let recent = add(&store, &other, NOW).await.unwrap();
        delete(&store, &recent.id, NOW - time::Duration::days(10))
            .await
            .unwrap();

        let mut third = input();
        third["email"] = Value::String("c@example.org".to_string());
        let live = add(&store, &third, NOW - time::Duration::days(300))
            .await
            .unwrap();

        // Deleted, but with no timestamp that can be read.
        let mut unreadable = serde_json::json!({"contact_id": "u", "deleted": true, "deleted_at": "garbage", "updated_at": null});
        unreadable["first_name"] = Value::String("U".to_string());
        store
            .upsert_document(PARTITION, "u", CONTACT, &unreadable)
            .await
            .unwrap();

        assert_eq!(purge_expired(&store, 90, NOW).await, 1);

        let ids: Vec<String> = list_all(&store)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert!(!ids.contains(&old.id));
        assert!(ids.contains(&recent.id), "inside the window");
        assert!(ids.contains(&live.id), "not deleted");
        assert!(
            ids.contains(&"u".to_string()),
            "unreadable age is left alone"
        );
    }

    #[tokio::test]
    async fn the_purge_does_nothing_without_persistence() {
        assert_eq!(purge_expired(&Disabled, 90, NOW).await, 0);
    }

    #[tokio::test]
    async fn listing_filters_searches_sorts_and_pages() {
        // §10.3.5.
        let store = InMemory::new();
        for (first, last, email, flow) in [
            ("Zed", "adams", "z@example.org", "phw-to-mpi"),
            ("Amy", "Brown", "a@example.org", "paris-to-mpi"),
            ("Bob", "Brown", "b@example.org", "phw-to-mpi"),
        ] {
            let mut data = input();
            data["first_name"] = Value::String(first.to_string());
            data["last_name"] = Value::String(last.to_string());
            data["email"] = Value::String(email.to_string());
            data["flow_ids"] = serde_json::json!([flow]);
            add(&store, &data, NOW).await.unwrap();
        }
        let all = list_all(&store).await.unwrap();

        let (page, total) = select(all.clone(), &Query::default());
        assert_eq!(total, 3);
        let order: Vec<&str> = page.iter().map(|c| c.first_name.as_str()).collect();
        assert_eq!(
            order,
            ["Zed", "Amy", "Bob"],
            "by last name, case-insensitively, then first"
        );

        let (flow_only, total) = select(
            all.clone(),
            &Query {
                flow_id: Some("phw-to-mpi".to_string()),
                ..Query::default()
            },
        );
        assert_eq!((flow_only.len(), total), (2, 2));

        let (found, _) = select(
            all.clone(),
            &Query {
                search: Some("BROWN".to_string()),
                ..Query::default()
            },
        );
        assert_eq!(found.len(), 2);
        let (found, _) = select(
            all.clone(),
            &Query {
                search: Some("supplier".to_string()),
                ..Query::default()
            },
        );
        assert_eq!(found.len(), 3, "organisation type is searched too");

        let (second, total) = select(
            all.clone(),
            &Query {
                per_page: 2,
                page: 2,
                ..Query::default()
            },
        );
        assert_eq!((second.len(), total), (1, 3));

        let (clamped, _) = select(
            all,
            &Query {
                per_page: 0,
                page: 0,
                ..Query::default()
            },
        );
        assert_eq!(clamped.len(), 1, "page and per_page below 1 are 1");
    }

    #[tokio::test]
    async fn deleted_contacts_are_listed_unless_excluded_and_never_grouped_by_flow() {
        let store = InMemory::new();
        let kept = add(&store, &input(), NOW).await.unwrap();
        let mut other = input();
        other["email"] = Value::String("x@example.org".to_string());
        let gone = add(&store, &other, NOW).await.unwrap();
        delete(&store, &gone.id, NOW).await.unwrap();
        let all = list_all(&store).await.unwrap();

        assert_eq!(
            select(all.clone(), &Query::default()).1,
            2,
            "include_deleted defaults to true"
        );
        assert_eq!(
            select(
                all.clone(),
                &Query {
                    include_deleted: false,
                    ..Query::default()
                }
            )
            .1,
            1
        );

        let grouped = by_flow(all);
        assert_eq!(grouped["phw-to-mpi"].len(), 1);
        assert_eq!(grouped["phw-to-mpi"][0].id, kept.id);
    }

    #[test]
    fn csv_rows_are_validated_one_by_one() {
        // §10.3.6.
        let (valid, errors) = parse_csv(
            "First_Name,last_name,Organisation_Type,organisation_name,email,flow_ids\n\
             Ada,Lovelace,Supplier,\"Engines, Ltd\",ada@example.org,phw-to-mpi;paris-to-mpi\n\
             Bad,Row,Charity,X,b@example.org,\n\
             No,Contact,Team,Y,,\n",
        );

        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0].organisation_name, "Engines, Ltd");
        assert_eq!(valid[0].flow_ids, ["phw-to-mpi", "paris-to-mpi"]);
        assert_eq!(errors.len(), 2);
        assert!(errors[0].starts_with("Row 3: "), "{errors:?}");
        assert!(errors[1].starts_with("Row 4: "), "{errors:?}");
    }

    #[test]
    fn a_csv_without_the_required_columns_is_refused_whole() {
        let (valid, errors) = parse_csv("first_name,last_name\nA,B\n");

        assert!(valid.is_empty());
        assert!(
            errors[0].contains("organisation_type, organisation_name"),
            "{errors:?}"
        );
        assert_eq!(parse_csv("").1, ["CSV file is empty."]);
    }

    #[tokio::test]
    async fn an_import_updates_by_email_then_phone_and_a_later_row_matches_an_earlier_one() {
        // §10.3.6.
        let store = InMemory::new();
        let existing = add(&store, &input(), NOW).await.unwrap();

        let result = import(
            &store,
            "first_name,last_name,organisation_type,organisation_name,email,phone_number\n\
             Augusta,Lovelace,Supplier,AE,ADA.LOVELACE@example.org,\n\
             New,Person,Team,NHS,new@example.org,\n\
             New2,Person,Team,NHS,NEW@example.org,\n\
             Phone,Only,Team,NHS,,029 2000 1111\n\
             Phone2,Only,Team,NHS,,029 2000 1111\n",
            NOW,
        )
        .await;

        assert_eq!((result.created, result.updated), (2, 3), "{result:?}");
        assert!(result.errors.is_empty(), "{result:?}");
        assert!(!result.persistence_failed);

        let all = list_all(&store).await.unwrap();
        assert_eq!(all.len(), 3);
        let ada = all.iter().find(|c| c.id == existing.id).unwrap();
        assert_eq!(ada.first_name, "Augusta");
    }

    #[tokio::test]
    async fn an_import_with_nowhere_to_store_says_so_and_503s() {
        let result = import(
            &Disabled,
            "first_name,last_name,organisation_type,organisation_name,email\nA,B,Team,C,a@b.co\n",
            NOW,
        )
        .await;

        assert!(result.persistence_failed);
        assert_eq!((result.created, result.updated), (0, 0));
        assert!(result.errors.iter().any(|e| e.contains("not configured")));
    }

    #[tokio::test]
    async fn an_import_stops_at_the_first_failed_write() {
        let store = InMemory::new();
        store.fail_writes("throttled");

        let result = import(
            &store,
            "first_name,last_name,organisation_type,organisation_name,email\nA,B,Team,C,a@b.co\nD,E,Team,F,d@e.co\n",
            NOW,
        )
        .await;

        assert!(result.persistence_failed);
        assert_eq!(result.errors.len(), 1, "it stopped: {result:?}");
        assert!(!result.errors[0].contains("throttled"));
    }
}
