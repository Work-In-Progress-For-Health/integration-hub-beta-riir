#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
// Checked arithmetic (`rust-checked-arithmetic`, RCA-ENF-3): denied for the whole
// crate. Tests may use plain operators and indexing (RCA-EXC-5).
#![cfg_attr(
    not(test),
    deny(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::integer_division,
        clippy::unreachable
    )
)]
//! A client for the WRDS (Welsh Reference Data Service) `GetResultSet`
//! SOAP operation.
//!
//! Implements `spec/09-transformers.md` §9.
//!
//! WRDS answers one generic question: given a lookup table, some filter
//! attributes and a list of attributes to retrieve, which rows match? The
//! calling pattern the core reference transformer uses is **one call per
//! HL7 message**, filtered on the two systems and retrieving every code
//! pair, after which each coded field is translated *client-side* with
//! [`to_code`]. That is why translation is a free function over rows rather
//! than a second network call.
//!
//! # Not `soap-sender`
//!
//! This shares nothing with `soap-sender` but the idea of an envelope. It
//! has its own request, its own response parser, and its own error type.

mod envelope;
mod http;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use envelope::{build_request, parse_response};

/// One result row: attribute name to value.
///
/// Names keep the case WRDS returned them in (it capitalises the requested
/// `type` as `Type`), so look them up with [`to_code`] rather than by key.
pub type Row = HashMap<String, String>;

/// The default request timeout, `WRDS_TIMEOUT_SECONDS`
/// (`spec/09-transformers.md` §8.7).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a call failed (`spec/09-transformers.md` §9.5).
///
/// No variant carries a value from a request or a row. `Fault` carries
/// WRDS's own `faultstring`, which is the server's diagnosis of the call and
/// is what an operator needs.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No answer within the timeout.
    #[error("WRDS GetResultSet timed out after {0}s")]
    Timeout(u64),
    /// The connection or TLS handshake failed.
    #[error("WRDS GetResultSet connection error: {0}")]
    Connection(String),
    /// A status other than `200`.
    #[error("WRDS GetResultSet returned HTTP {0}")]
    Status(u16),
    /// A SOAP `Fault`.
    #[error("WRDS GetResultSet SOAP fault: {0}")]
    Fault(String),
    /// The body was not a usable response.
    #[error("WRDS GetResultSet response is not usable: {0}")]
    Malformed(String),
    /// The local fixture could not be read (`§9.6`).
    #[error("WRDS local fixture file not found or unreadable: {0}")]
    Fixture(String),
    /// The client certificate or trust roots could not be loaded.
    #[error("WRDS client TLS configuration is not usable: {0}")]
    Tls(String),
}

/// A WRDS client.
///
/// # Examples
///
/// Translation needs no network, only rows:
///
/// ```
/// use wrds_service::{Row, to_code};
///
/// let rows: Vec<Row> = vec![Row::from([
///     ("FromCode".to_string(), "1".to_string()),
///     ("Type".to_string(), "Sex".to_string()),
///     ("ToCode".to_string(), "M".to_string()),
/// ])];
///
/// assert_eq!(to_code(&rows, "1", "Sex"), "M");
/// assert_eq!(to_code(&rows, "2", "Sex"), "");
/// ```
pub struct Client {
    /// Where to `POST`.
    endpoint: String,
    /// The whole-request timeout.
    timeout: Duration,
    /// Present for an `https` endpoint.
    tls: Option<tokio_rustls::TlsConnector>,
    /// `§9.6`. When set, no request is made.
    fixture: Option<PathBuf>,
}

impl std::fmt::Debug for Client {
    /// By hand: a TLS connector is not `Debug`, and the endpoint is the
    /// only thing worth printing.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("endpoint", &self.endpoint)
            .field("timeout", &self.timeout)
            .field("tls", &self.tls.is_some())
            .field("fixture", &self.fixture)
            .finish()
    }
}

impl Client {
    /// A client for `endpoint`.
    ///
    /// The TLS configuration is built **here**, not on first use, so a
    /// certificate that cannot be read stops the service at startup
    /// (`spec/18-deployment.md` §4.3's reasoning) rather than failing every
    /// message.
    ///
    /// # Errors
    ///
    /// [`Error::Tls`] when the trust roots or the client certificate cannot
    /// be loaded.
    pub fn new(
        endpoint: impl Into<String>,
        timeout: Duration,
        client_cert: Option<&Path>,
        fixture: Option<PathBuf>,
    ) -> Result<Self, Error> {
        let endpoint = endpoint.into();
        let tls = if fixture.is_none() && endpoint.starts_with("https://") {
            Some(http::connector(client_cert)?)
        } else {
            None
        };
        Ok(Self {
            endpoint,
            timeout,
            tls,
            fixture,
        })
    }

    /// Call `GetResultSet` and return its rows.
    ///
    /// In fixture mode (`spec/09-transformers.md` §9.6) the fixture's rows
    /// are returned whole and the filters are ignored; callers translate
    /// client-side anyway.
    ///
    /// # Errors
    ///
    /// Any [`Error`].
    pub async fn get_result_set(
        &self,
        lookup_table: &str,
        attributes: &[(&str, &str)],
        retrieve: &[&str],
        exact_match: bool,
    ) -> Result<Vec<Row>, Error> {
        if let Some(path) = &self.fixture {
            tracing::info!(
                lookup_table,
                filters = attributes.len(),
                "using the local WRDS fixture instead of a network call"
            );
            let text = std::fs::read_to_string(path)
                .map_err(|error| Error::Fixture(error.kind().to_string()))?;
            return parse_response(&text);
        }

        let body = build_request(lookup_table, attributes, retrieve, exact_match);
        tracing::debug!(
            lookup_table,
            filters = attributes.len(),
            "WRDS GetResultSet"
        );

        let text = tokio::time::timeout(
            self.timeout,
            http::post(&self.endpoint, self.tls.as_ref(), body),
        )
        .await
        .map_err(|_| Error::Timeout(self.timeout.as_secs()))??;

        parse_response(&text)
    }

    /// [`Self::get_result_set`] for a synchronous caller.
    ///
    /// The transformer trait is synchronous and runs on the service's
    /// multi-thread runtime, so the call blocks *this worker* with
    /// [`tokio::task::block_in_place`] and lets the others (the health
    /// check, the bus connection's keep-alives) carry on. Outside any
    /// runtime — a plain unit test — a private one is used.
    ///
    /// # Panics
    ///
    /// Inside a **current-thread** runtime, where `block_in_place` cannot
    /// work. The service never runs on one.
    ///
    /// # Errors
    ///
    /// Any [`Error`].
    pub fn get_result_set_blocking(
        &self,
        lookup_table: &str,
        attributes: &[(&str, &str)],
        retrieve: &[&str],
        exact_match: bool,
    ) -> Result<Vec<Row>, Error> {
        let call = self.get_result_set(lookup_table, attributes, retrieve, exact_match);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(|| handle.block_on(call)),
            Err(_) => tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| Error::Connection(error.to_string()))?
                .block_on(call),
        }
    }
}

/// The `ToCode` for `from_code` of `reference_type`, from rows already
/// fetched (`spec/09-transformers.md` §9.4).
///
/// Attribute **names** are compared case-insensitively, because WRDS
/// returns the requested `type` as `Type`; values are compared exactly.
/// The first matching row wins. No match, or an empty `ToCode`, is `""`.
#[must_use]
pub fn to_code(rows: &[Row], from_code: &str, reference_type: &str) -> String {
    for row in rows {
        let field = |name: &str| {
            row.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        if field("fromcode") == Some(from_code) && field("type") == Some(reference_type) {
            return field("tocode").unwrap_or_default().to_string();
        }
    }
    String::new()
}
