//! The WRDS settings.
//!
//! Implements `spec/09-transformers.md` §8.7.
//!
//! Read separately from the transformer runtime's own configuration
//! (`spec/02-configuration.md` §5), which knows nothing about WRDS. Every
//! problem is reported at once (`spec/02-configuration.md` §1.4).

use std::path::PathBuf;
use std::time::Duration;

use ihb_config::{ConfigErrors, Reader};

/// `WRDS_LOOKUP_TABLE_NAME`'s default (§8.7): the WRDS table that holds the
/// source-to-MPI code translations.
pub const DEFAULT_LOOKUP_TABLE: &str = "FioranoCodeTranslation";

/// Everything the WRDS client needs.
///
/// # Examples
///
/// ```
/// use hl7_core_reference_transformer::config::WrdsConfig;
///
/// let config = WrdsConfig::from_pairs([("WRDS_ENDPOINT_URL", "https://wrds.example/svc")])
///     .expect("the endpoint is all that is required");
///
/// assert_eq!(config.timeout.as_secs(), 30);
/// assert_eq!(config.lookup_table, "FioranoCodeTranslation");
/// ```
#[derive(Debug, Clone)]
pub struct WrdsConfig {
    /// `WRDS_ENDPOINT_URL`. **Required.**
    pub endpoint_url: String,
    /// `WRDS_TIMEOUT_SECONDS`: whole-request timeout; positive, default 30 s
    /// (§8.7). Zero is refused rather than meaning "no timeout".
    pub timeout: Duration,
    /// `WRDS_CLIENT_CERT_PATH`: one PEM holding the client chain and its
    /// private key, for mTLS (§8.7). Blank means unset.
    pub client_cert_path: Option<PathBuf>,
    /// `WRDS_LOOKUP_TABLE_NAME`; defaults to [`DEFAULT_LOOKUP_TABLE`].
    pub lookup_table: String,
    /// `WRDS_LOCAL_FIXTURE_PATH`: when set, the client reads this file
    /// instead of calling WRDS (`spec/09-transformers.md` §9.6). Local
    /// testing only; leave unset in a deployed environment.
    pub local_fixture_path: Option<PathBuf>,
}

impl WrdsConfig {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_env() -> Result<Self, ConfigErrors> {
        Self::read(Reader::from_env())
    }

    /// Read from explicit pairs, for tests and examples.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, ConfigErrors>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self::read(Reader::from_pairs(pairs))
    }

    /// The shared body of the two constructors.
    ///
    /// `reader.finish()` runs after every value is read so all problems are
    /// reported together (`spec/02-configuration.md` §1.4), not just the
    /// first.
    fn read(mut reader: Reader) -> Result<Self, ConfigErrors> {
        let config = Self {
            endpoint_url: reader.required_string("WRDS_ENDPOINT_URL"),
            timeout: Duration::from_secs(
                reader
                    .optional_positive_integer::<u64>("WRDS_TIMEOUT_SECONDS")
                    .unwrap_or(30),
            ),
            client_cert_path: reader
                .optional_string("WRDS_CLIENT_CERT_PATH")
                .map(PathBuf::from),
            lookup_table: reader.string_or("WRDS_LOOKUP_TABLE_NAME", DEFAULT_LOOKUP_TABLE),
            local_fixture_path: reader
                .optional_string("WRDS_LOCAL_FIXTURE_PATH")
                .map(PathBuf::from),
        };
        reader.finish()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_required_and_optional_values() {
        // The Python's `test_reads_required_and_optional_values`.
        let config = WrdsConfig::from_pairs([
            ("WRDS_ENDPOINT_URL", "https://example.test/wrdssoapservice"),
            ("WRDS_TIMEOUT_SECONDS", "45"),
            ("WRDS_CLIENT_CERT_PATH", "certs/client.pem"),
            ("WRDS_LOOKUP_TABLE_NAME", "SomeOtherTable"),
            ("WRDS_LOCAL_FIXTURE_PATH", "wrds_local_fixture.xml"),
        ])
        .unwrap();

        assert_eq!(config.endpoint_url, "https://example.test/wrdssoapservice");
        assert_eq!(config.timeout, Duration::from_secs(45));
        assert_eq!(config.client_cert_path, Some("certs/client.pem".into()));
        assert_eq!(config.lookup_table, "SomeOtherTable");
        assert_eq!(
            config.local_fixture_path,
            Some("wrds_local_fixture.xml".into())
        );
    }

    #[test]
    fn the_defaults_apply_when_the_optional_values_are_missing() {
        // spec/09-transformers.md §8.7.
        let config =
            WrdsConfig::from_pairs([("WRDS_ENDPOINT_URL", "https://example.test/x")]).unwrap();

        assert_eq!(config.timeout, Duration::from_secs(30));
        assert!(config.client_cert_path.is_none());
        assert_eq!(config.lookup_table, DEFAULT_LOOKUP_TABLE);
        assert!(config.local_fixture_path.is_none());
    }

    #[test]
    fn a_missing_endpoint_is_an_error_naming_it() {
        // §8.7.
        let errors = WrdsConfig::from_pairs([] as [(&str, &str); 0]).unwrap_err();
        assert!(errors.to_string().contains("WRDS_ENDPOINT_URL"), "{errors}");
    }

    #[test]
    fn blank_is_absent() {
        // spec/02-configuration.md §1.1 — the Python's `or None` / `or default`.
        let config = WrdsConfig::from_pairs([
            ("WRDS_ENDPOINT_URL", "https://example.test/x"),
            ("WRDS_CLIENT_CERT_PATH", " "),
            ("WRDS_LOOKUP_TABLE_NAME", ""),
            ("WRDS_TIMEOUT_SECONDS", ""),
        ])
        .unwrap();

        assert!(config.client_cert_path.is_none());
        assert_eq!(config.lookup_table, DEFAULT_LOOKUP_TABLE);
        assert_eq!(config.timeout, Duration::from_secs(30));
    }

    #[test]
    fn a_zero_timeout_is_refused() {
        // §8.7 — positive.
        let errors = WrdsConfig::from_pairs([
            ("WRDS_ENDPOINT_URL", "https://example.test/x"),
            ("WRDS_TIMEOUT_SECONDS", "0"),
        ])
        .unwrap_err();
        assert!(
            errors.to_string().contains("WRDS_TIMEOUT_SECONDS"),
            "{errors}"
        );
    }
}
