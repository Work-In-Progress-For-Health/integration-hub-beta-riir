//! The service's errors, each of which maps to one HTTP status and one
//! `error` code.
//!
//! Implements `spec/20-lookup-service.md` §8.
//!
//! # No values in an error
//!
//! §1.3. A message names a table, a column or a line number and never a key
//! or a value, because the message ends up in a response body and in the
//! caller's logs.

use thiserror::Error;

/// What went wrong, without the HTTP framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `table_not_found` (404).
    TableNotFound,
    /// `key_not_found` (404).
    KeyNotFound,
    /// `invalid_key` (422).
    InvalidKey,
    /// `invalid_upload` (422).
    InvalidUpload,
    /// `invalid_settings` (422).
    InvalidSettings,
    /// `invalid_request` (422): the Rust port's replacement for `FastAPI`'s own
    /// validation body (§8.4).
    InvalidRequest,
    /// `upload_too_large` (413).
    UploadTooLarge,
    /// `unauthorized` (401): no, an invalid or an expired credential (§12).
    Unauthorized,
    /// `forbidden` (403): authenticated, but lacking the role (§12).
    Forbidden,
    /// `csrf_failed` (403): the double-submit check failed (§12.5).
    CsrfFailed,
    /// `read_only` (409).
    ReadOnly,
    /// `backend_unavailable` (503).
    BackendUnavailable,
    /// `not_ready` (503).
    NotReady,
}

impl Kind {
    /// The HTTP status code (§8).
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Self::TableNotFound | Self::KeyNotFound => 404,
            Self::InvalidKey
            | Self::InvalidUpload
            | Self::InvalidSettings
            | Self::InvalidRequest => 422,
            Self::UploadTooLarge => 413,
            Self::Unauthorized => 401,
            Self::Forbidden | Self::CsrfFailed => 403,
            Self::ReadOnly => 409,
            Self::BackendUnavailable | Self::NotReady => 503,
        }
    }

    /// The `error` field of the response body (§8).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TableNotFound => "table_not_found",
            Self::KeyNotFound => "key_not_found",
            Self::InvalidKey => "invalid_key",
            Self::InvalidUpload => "invalid_upload",
            Self::InvalidSettings => "invalid_settings",
            Self::InvalidRequest => "invalid_request",
            Self::UploadTooLarge => "upload_too_large",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::CsrfFailed => "csrf_failed",
            Self::ReadOnly => "read_only",
            Self::BackendUnavailable => "backend_unavailable",
            Self::NotReady => "not_ready",
        }
    }
}

/// An error that maps to an HTTP response with the service's one JSON shape.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}: {detail}", kind.code())]
pub struct LookupError {
    /// What kind of failure.
    pub kind: Kind,
    /// A sentence for the caller. Never a key or a value.
    pub detail: String,
    /// The table concerned, when there is one.
    pub table: Option<String>,
    /// Per-problem messages, for upload and settings validation.
    pub errors: Option<Vec<String>>,
    /// Answer with `WWW-Authenticate: Bearer` (§12.2): set on the 401s of
    /// the bearer-token API, as RFC 6750 requires and as upstream's
    /// `UnauthorizedError.headers` does.
    pub bearer_challenge: bool,
}

impl LookupError {
    /// An error with no table and no problem list.
    #[must_use]
    pub fn new(kind: Kind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            table: None,
            errors: None,
            bearer_challenge: false,
        }
    }

    /// A `401` for the bearer-token API, carrying the `Bearer` challenge.
    #[must_use]
    pub fn bearer_unauthorized(detail: impl Into<String>) -> Self {
        let mut error = Self::new(Kind::Unauthorized, detail);
        error.bearer_challenge = true;
        error
    }

    /// Name the table concerned, so the response and the log line say which
    /// table without the detail having to.
    #[must_use]
    pub fn for_table(mut self, table: impl Into<String>) -> Self {
        self.table = Some(table.into());
        self
    }

    /// Attach the list of problems (`<field>: <reason>`, never the offending
    /// value; `spec/20-lookup-service.md` §3.1).
    #[must_use]
    pub fn with_errors(mut self, errors: Vec<String>) -> Self {
        self.errors = Some(errors);
        self
    }

    /// `table_not_found` for `table`.
    #[must_use]
    pub fn table_not_found(table: &str) -> Self {
        Self::new(
            Kind::TableNotFound,
            format!("Table '{table}' does not exist"),
        )
        .for_table(table)
    }

    /// `invalid_key` (422) for a lookup key that does not fit the table's
    /// key columns (§4).
    #[must_use]
    pub fn invalid_key(detail: impl Into<String>, table: &str) -> Self {
        Self::new(Kind::InvalidKey, detail).for_table(table)
    }

    /// `invalid_upload` without a table (the name itself may be the problem).
    #[must_use]
    pub fn invalid_upload(detail: impl Into<String>) -> Self {
        Self::new(Kind::InvalidUpload, detail)
    }

    /// `invalid_settings` (422): a settings change that would make the
    /// definition invalid (§3.1). `table` is `None` when the table is unknown.
    #[must_use]
    pub fn invalid_settings(detail: impl Into<String>, table: Option<&str>) -> Self {
        let error = Self::new(Kind::InvalidSettings, detail);
        match table {
            Some(table) => error.for_table(table),
            None => error,
        }
    }

    /// `invalid_request` (422): the request is malformed before any table
    /// rule applies (§8.4).
    #[must_use]
    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self::new(Kind::InvalidRequest, detail)
    }

    /// `backend_unavailable` carrying the failure's type, never its text
    /// (§5.5).
    #[must_use]
    pub fn backend_unavailable(detail: impl Into<String>, table: Option<&str>) -> Self {
        let error = Self::new(Kind::BackendUnavailable, detail);
        match table {
            Some(table) => error.for_table(table),
            None => error,
        }
    }

    /// `read_only` (409) for a write to a store that is not writable, such
    /// as the file store (§3.3).
    #[must_use]
    pub fn read_only(detail: impl Into<String>, table: &str) -> Self {
        Self::new(Kind::ReadOnly, detail).for_table(table)
    }
}

/// Seed data or its manifest is invalid. Raised at startup so that a bad
/// image never serves (§7.2).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct SeedDataError(pub String);
