//! Reading a whole configuration, collecting every problem on the way.
//!
//! Implements `spec/02-configuration.md` §1.3, §1.4, §1.6 and §10.

use std::collections::BTreeMap;
use std::str::FromStr;

use crate::error::{ConfigError, ConfigErrors};
use crate::secret::Secret;
use crate::value::{parse_bool, parse_csv, present};

/// Reads a service's configuration, accumulating problems rather than
/// stopping at the first.
///
/// A reader is built once, asked for every value the service needs, and then
/// [`finish`](Reader::finish)ed. Each getter returns a usable value even
/// when it failed — a required string that was missing comes back empty —
/// so a caller can read the whole configuration in one straight run of
/// statements without `?` after every line. `finish` is what decides whether
/// any of it was valid.
///
/// That shape is the point: a service with four missing variables reports
/// all four, once (`spec/02-configuration.md` §1.4).
///
/// # Examples
///
/// ```
/// use ihb_config::Reader;
///
/// /// What an MLLP ingress server needs, minus everything not in this example.
/// #[derive(Debug)]
/// struct Config {
///     workflow_id: String,
///     port: u16,
///     sending_apps: Vec<String>,
/// }
///
/// let mut reader = Reader::from_pairs([
///     ("WORKFLOW_ID", "chemocare-to-mpi"),
///     ("PORT", "2578"),
///     ("SENDING_APP", "245,212,192,224"),
/// ]);
///
/// let config = Config {
///     workflow_id: reader.required_string("WORKFLOW_ID"),
///     port: reader.integer_or("PORT", 2575),
///     sending_apps: reader.list("SENDING_APP"),
/// };
/// reader.finish()?;
///
/// assert_eq!(config.port, 2578);
/// assert_eq!(config.sending_apps.len(), 4);
/// # Ok::<(), ihb_config::ConfigErrors>(())
/// ```
#[derive(Debug)]
pub struct Reader {
    /// The variables this reader sees.
    source: BTreeMap<String, String>,
    /// Problems found so far, in the order they were found.
    errors: Vec<ConfigError>,
}

impl Reader {
    /// A reader over the process environment.
    ///
    /// The only function in this crate that touches the environment, so
    /// every other path is testable without mutating global state
    /// (`spec/02-configuration.md` §10.5).
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            source: std::env::vars().collect(),
            errors: Vec::new(),
        }
    }

    /// A reader over an explicit set of variables, for tests and for
    /// configuration that arrives from somewhere other than the environment.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("HEALTH_CHECK_PORT", "9000")]);
    /// assert_eq!(reader.integer_or::<u16>("HEALTH_CHECK_PORT", 9000), 9000);
    /// ```
    pub fn from_pairs<K, V, I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            source: pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
            errors: Vec::new(),
        }
    }

    /// The raw value of a variable, trimmed, or `None` when absent or blank.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let reader = Reader::from_pairs([("A", " x "), ("B", "  ")]);
    /// assert_eq!(reader.raw("A"), Some("x"));
    /// assert_eq!(reader.raw("B"), None);
    /// assert_eq!(reader.raw("C"), None);
    /// ```
    #[must_use]
    pub fn raw(&self, name: &str) -> Option<&str> {
        present(self.source.get(name).map(String::as_str))
    }

    /// A variable that must be set.
    ///
    /// Records [`ConfigError::Missing`] and returns an empty string when it
    /// is not, so reading can continue and every other problem is found too.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("WORKFLOW_ID", "phw-to-mpi")]);
    /// assert_eq!(reader.required_string("WORKFLOW_ID"), "phw-to-mpi");
    /// assert_eq!(reader.required_string("MICROSERVICE_ID"), "");
    /// assert_eq!(reader.finish().unwrap_err().len(), 1);
    /// ```
    pub fn required_string(&mut self, name: &str) -> String {
        if let Some(value) = self.raw(name) {
            value.to_string()
        } else {
            self.errors.push(ConfigError::missing(name));
            String::new()
        }
    }

    /// A variable that need not be set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let reader = Reader::from_pairs([("HL7_VERSION", "2.5")]);
    /// assert_eq!(reader.optional_string("HL7_VERSION").as_deref(), Some("2.5"));
    /// assert_eq!(reader.optional_string("SENDING_APP"), None);
    /// ```
    #[must_use]
    pub fn optional_string(&self, name: &str) -> Option<String> {
        self.raw(name).map(str::to_string)
    }

    /// An optional variable with a fallback.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let reader = Reader::from_pairs([("HOST", "0.0.0.0")]);
    /// assert_eq!(reader.string_or("HOST", "127.0.0.1"), "0.0.0.0");
    /// assert_eq!(reader.string_or("HEALTH_CHECK_HOST", "127.0.0.1"), "127.0.0.1");
    /// ```
    #[must_use]
    pub fn string_or(&self, name: &str, default: &str) -> String {
        self.raw(name).unwrap_or(default).to_string()
    }

    /// A secret that must be set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("MSSQL_SA_PASSWORD", "hunter2")]);
    /// let password = reader.required_secret("MSSQL_SA_PASSWORD");
    /// assert_eq!(format!("{password:?}"), "<redacted>");
    /// ```
    pub fn required_secret(&mut self, name: &str) -> Secret {
        Secret::new(self.required_string(name))
    }

    /// A secret that need not be set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let reader = Reader::from_pairs([("SOAP_API_KEY", "  ")]);
    /// assert!(reader.optional_secret("SOAP_API_KEY").is_none());
    /// ```
    #[must_use]
    pub fn optional_secret(&self, name: &str) -> Option<Secret> {
        self.raw(name).map(Secret::new)
    }

    /// An integer that must be set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("RECEIVER_MLLP_PORT", "2576")]);
    /// assert_eq!(reader.required_integer::<u16>("RECEIVER_MLLP_PORT"), 2576);
    /// ```
    pub fn required_integer<T>(&mut self, name: &str) -> T
    where
        T: FromStr + Default,
    {
        match self.raw(name) {
            None => {
                self.errors.push(ConfigError::missing(name));
                T::default()
            }
            Some(value) => {
                let value = value.to_string();
                self.parse_integer(name, &value).unwrap_or_default()
            }
        }
    }

    /// An integer that need not be set.
    ///
    /// A blank value is absent, not a parse failure — the difference from
    /// the Python, which would have called `int("")`
    /// (`spec/02-configuration.md` §1.1).
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("A", "30"), ("B", "  ")]);
    /// assert_eq!(reader.optional_integer::<u64>("A"), Some(30));
    /// assert_eq!(reader.optional_integer::<u64>("B"), None);
    /// assert!(reader.finish().is_ok());
    /// ```
    pub fn optional_integer<T>(&mut self, name: &str) -> Option<T>
    where
        T: FromStr,
    {
        let value = self.raw(name)?.to_string();
        self.parse_integer(name, &value)
    }

    /// An integer with a fallback.
    ///
    /// An unparseable value is a recorded error, **not** a silent fallback
    /// to the default: a service quietly running on a default nobody chose
    /// is harder to diagnose than one that will not start
    /// (`spec/02-configuration.md` §1.6).
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("PORT", "2578"), ("OTHER", "two")]);
    /// assert_eq!(reader.integer_or::<u16>("PORT", 2575), 2578);
    /// assert_eq!(reader.integer_or::<u16>("MISSING", 2575), 2575);
    /// assert_eq!(reader.integer_or::<u16>("OTHER", 2575), 2575);
    /// assert_eq!(reader.finish().unwrap_err().len(), 1);
    /// ```
    pub fn integer_or<T>(&mut self, name: &str, default: T) -> T
    where
        T: FromStr,
    {
        match self.raw(name) {
            None => default,
            Some(value) => {
                let value = value.to_string();
                self.parse_integer(name, &value).unwrap_or(default)
            }
        }
    }

    /// An optional integer that, when present, must be greater than zero.
    ///
    /// `MAX_MESSAGES_PER_MINUTE` is the case: absent means unthrottled, and
    /// a configured zero is a mistake rather than a request for an infinite
    /// delay (`spec/02-configuration.md` §6.1).
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("A", "30"), ("B", "0")]);
    /// assert_eq!(reader.optional_positive_integer::<u32>("A"), Some(30));
    /// assert_eq!(reader.optional_positive_integer::<u32>("B"), None);
    /// assert_eq!(reader.optional_positive_integer::<u32>("C"), None);
    /// // Only B is a problem; C was simply absent.
    /// assert_eq!(reader.finish().unwrap_err().len(), 1);
    /// ```
    pub fn optional_positive_integer<T>(&mut self, name: &str) -> Option<T>
    where
        T: FromStr + PartialOrd + Default,
    {
        let raw = self.raw(name)?.to_string();
        let parsed: T = self.parse_integer(name, &raw)?;
        if parsed > T::default() {
            Some(parsed)
        } else {
            self.errors.push(ConfigError::invalid(
                name,
                "a positive integer when provided",
                raw,
            ));
            None
        }
    }

    /// A boolean with a fallback, per [`crate::parse_bool`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let reader = Reader::from_pairs([("HL7_LOG_REDACTION_ENABLED", "false")]);
    /// assert!(!reader.bool_or("HL7_LOG_REDACTION_ENABLED", true));
    /// assert!(reader.bool_or("MESSAGE_STORE_ENABLED", true));
    /// ```
    #[must_use]
    pub fn bool_or(&self, name: &str, default: bool) -> bool {
        parse_bool(self.raw(name)).unwrap_or(default)
    }

    /// A comma-separated list, possibly empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let reader = Reader::from_pairs([("SENDING_APP", "245, 212")]);
    /// assert_eq!(reader.list("SENDING_APP"), ["245", "212"]);
    /// assert!(reader.list("ABSENT").is_empty());
    /// ```
    #[must_use]
    pub fn list(&self, name: &str) -> Vec<String> {
        parse_csv(self.raw(name))
    }

    /// A comma-separated list that must end up with at least one element,
    /// falling back to a default list when the variable is absent.
    ///
    /// A variable set to only separators is a recorded error rather than an
    /// empty list, because it means someone meant to configure something
    /// (`spec/02-configuration.md` §1.7).
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("BAD", " , ")]);
    /// assert_eq!(
    ///     reader.list_or("ALLOWED_HL7_STRUCTURES", "ADT_A05,ADT_A39"),
    ///     ["ADT_A05", "ADT_A39"]
    /// );
    /// assert!(reader.list_or("BAD", "x").is_empty());
    /// assert_eq!(reader.finish().unwrap_err().len(), 1);
    /// ```
    pub fn list_or(&mut self, name: &str, default: &str) -> Vec<String> {
        match self.raw(name) {
            None => parse_csv(Some(default)),
            Some(value) => {
                let parsed = parse_csv(Some(value));
                if parsed.is_empty() {
                    self.errors.push(ConfigError::invalid(
                        name,
                        "a comma-separated list with at least one value",
                        value,
                    ));
                }
                parsed
            }
        }
    }

    /// A required UUID.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([
    ///     ("REPLAY_BATCH_ID", "00000000-0000-0000-0000-000000000001"),
    /// ]);
    /// let id = reader.required_uuid("REPLAY_BATCH_ID");
    /// assert_eq!(id.to_string(), "00000000-0000-0000-0000-000000000001");
    /// ```
    pub fn required_uuid(&mut self, name: &str) -> uuid::Uuid {
        match self.raw(name) {
            None => {
                self.errors.push(ConfigError::missing(name));
                uuid::Uuid::nil()
            }
            Some(value) => {
                if let Ok(parsed) = uuid::Uuid::parse_str(value) {
                    parsed
                } else {
                    self.errors
                        .push(ConfigError::invalid(name, "a UUID", value));
                    uuid::Uuid::nil()
                }
            }
        }
    }

    /// A value that must be one of a fixed set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// const RELEASES: &[&str] = &["2.4", "2.5", "2.5.1", "2.6"];
    ///
    /// let mut reader = Reader::from_pairs([("HL7_VALIDATION_STANDARD", "2.7")]);
    /// assert_eq!(reader.optional_one_of("HL7_VALIDATION_STANDARD", RELEASES), None);
    ///
    /// let errors = reader.finish().unwrap_err();
    /// assert!(errors.to_string().contains("one of 2.4, 2.5, 2.5.1, 2.6"));
    /// ```
    pub fn optional_one_of(&mut self, name: &str, allowed: &[&str]) -> Option<String> {
        let value = self.raw(name)?.to_string();
        if allowed.contains(&value.as_str()) {
            Some(value)
        } else {
            self.errors.push(ConfigError::invalid(
                name,
                format!("one of {}", allowed.join(", ")),
                value,
            ));
            None
        }
    }

    /// Record a problem this crate cannot detect on its own — a conflict
    /// between variables, or a rule specific to one service.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("EGRESS_QUEUE_NAME", "q"), ("EGRESS_TOPIC_NAME", "t")]);
    /// if reader.raw("EGRESS_QUEUE_NAME").is_some() && reader.raw("EGRESS_TOPIC_NAME").is_some() {
    ///     reader.conflict("Cannot specify both EGRESS_QUEUE_NAME and EGRESS_TOPIC_NAME.");
    /// }
    /// assert!(reader.finish().is_err());
    /// ```
    pub fn conflict(&mut self, message: impl Into<String>) {
        self.errors.push(ConfigError::conflict(message));
    }

    /// Whether anything has gone wrong so far.
    ///
    /// Lets a caller skip a cross-field check that would only produce noise
    /// on top of an already-broken configuration.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([] as [(&str, &str); 0]);
    /// assert!(!reader.has_errors());
    /// let _ = reader.required_string("WORKFLOW_ID");
    /// assert!(reader.has_errors());
    /// ```
    #[must_use]
    pub const fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Finish reading.
    ///
    /// # Errors
    ///
    /// Returns every problem found, in the order it was found, when there
    /// was at least one.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Reader;
    ///
    /// let mut reader = Reader::from_pairs([("WORKFLOW_ID", "phw-to-mpi")]);
    /// let _ = reader.required_string("WORKFLOW_ID");
    /// assert!(reader.finish().is_ok());
    /// ```
    pub fn finish(self) -> Result<(), ConfigErrors> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigErrors::new(self.errors))
        }
    }

    /// Parse an integer, recording a problem and returning `None` when it
    /// does not parse.
    fn parse_integer<T: FromStr>(&mut self, name: &str, value: &str) -> Option<T> {
        if let Ok(parsed) = value.parse::<T>() {
            Some(parsed)
        } else {
            self.errors
                .push(ConfigError::invalid(name, "a whole number", value));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_problem_is_reported_at_once() {
        // spec/02-configuration.md §1.4
        let mut reader = Reader::from_pairs([("PORT", "two")]);
        let _ = reader.required_string("WORKFLOW_ID");
        let _ = reader.required_string("MICROSERVICE_ID");
        let _: u16 = reader.integer_or("PORT", 2575);

        let errors = reader.finish().unwrap_err();
        assert_eq!(errors.len(), 3);
        assert_eq!(errors.to_string().lines().count(), 3);
    }

    #[test]
    fn a_blank_integer_is_absent_rather_than_a_parse_failure() {
        // spec/02-configuration.md §1.1 — where the Python called int("").
        let mut reader = Reader::from_pairs([("HEALTH_CHECK_PORT", "   ")]);
        assert_eq!(reader.optional_integer::<u16>("HEALTH_CHECK_PORT"), None);
        assert_eq!(reader.integer_or::<u16>("HEALTH_CHECK_PORT", 9000), 9000);
        assert!(reader.finish().is_ok());
    }

    #[test]
    fn an_unparseable_integer_is_not_silently_defaulted() {
        // spec/02-configuration.md §1.6
        let mut reader = Reader::from_pairs([("MAX_BATCH_SIZE", "lots")]);
        assert_eq!(reader.integer_or::<usize>("MAX_BATCH_SIZE", 100), 100);
        let errors = reader.finish().unwrap_err();
        assert!(errors.to_string().contains("MAX_BATCH_SIZE"));
        assert!(errors.to_string().contains("a whole number"));
    }

    #[test]
    fn an_out_of_range_integer_is_a_problem() {
        // spec/02-configuration.md §1.6 — 70000 does not fit a port.
        let mut reader = Reader::from_pairs([("PORT", "70000")]);
        assert_eq!(reader.integer_or::<u16>("PORT", 2575), 2575);
        assert!(reader.finish().is_err());
    }

    #[test]
    fn a_configured_zero_throttle_is_a_problem_but_an_absent_one_is_not() {
        // spec/02-configuration.md §6.1
        let mut reader = Reader::from_pairs([("MAX_MESSAGES_PER_MINUTE", "0")]);
        assert_eq!(
            reader.optional_positive_integer::<u32>("MAX_MESSAGES_PER_MINUTE"),
            None
        );
        assert_eq!(reader.optional_positive_integer::<u32>("ABSENT"), None);
        assert_eq!(reader.finish().unwrap_err().len(), 1);
    }

    #[test]
    fn a_required_string_still_returns_something_so_reading_continues() {
        // spec/02-configuration.md §1.4 — the reason getters do not use `?`.
        let mut reader = Reader::from_pairs([] as [(&str, &str); 0]);
        assert_eq!(reader.required_string("A"), "");
        assert_eq!(reader.required_string("B"), "");
        assert_eq!(reader.finish().unwrap_err().len(), 2);
    }

    #[test]
    fn a_uuid_that_is_not_one_names_itself() {
        // spec/02-configuration.md §7.2
        let mut reader = Reader::from_pairs([("REPLAY_BATCH_ID", "not-a-uuid")]);
        assert!(reader.required_uuid("REPLAY_BATCH_ID").is_nil());
        assert!(
            reader
                .finish()
                .unwrap_err()
                .to_string()
                .contains("REPLAY_BATCH_ID must be a UUID")
        );
    }

    #[test]
    fn an_unsupported_release_lists_the_supported_ones() {
        // spec/02-configuration.md §3.3
        let mut reader = Reader::from_pairs([("HL7_VALIDATION_STANDARD", "2.9")]);
        assert_eq!(
            reader.optional_one_of("HL7_VALIDATION_STANDARD", &["2.4", "2.5", "2.5.1", "2.6"]),
            None
        );
        assert!(
            reader
                .finish()
                .unwrap_err()
                .to_string()
                .contains("one of 2.4, 2.5, 2.5.1, 2.6")
        );
    }

    #[test]
    fn a_blank_release_is_absent_rather_than_unsupported() {
        // spec/02-configuration.md §1.1 — phw-hl7-server.env ships
        // HL7_VALIDATION_STANDARD="" and must not fail to start.
        let mut reader = Reader::from_pairs([("HL7_VALIDATION_STANDARD", "")]);
        assert_eq!(
            reader.optional_one_of("HL7_VALIDATION_STANDARD", &["2.4", "2.5"]),
            None
        );
        assert!(reader.finish().is_ok());
    }

    #[test]
    fn a_default_list_is_used_when_the_variable_is_absent() {
        // spec/02-configuration.md §4
        let mut reader = Reader::from_pairs([] as [(&str, &str); 0]);
        assert_eq!(
            reader.list_or("ALLOWED_HL7_STRUCTURES", "ADT_A05,ADT_A39"),
            ["ADT_A05", "ADT_A39"]
        );
        assert!(reader.finish().is_ok());
    }

    #[test]
    fn a_reader_over_the_environment_sees_it() {
        // spec/02-configuration.md §10.5 — the one function that reads the
        // process environment does read it. PATH is set everywhere this
        // runs, and nothing is mutated to check.
        let reader = Reader::from_env();
        assert!(reader.raw("PATH").is_some());
    }
}
