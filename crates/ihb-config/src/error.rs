//! What can be wrong with a configuration, and how it reads.
//!
//! Implements `spec/02-configuration.md` §1.3 and §1.4.

use std::fmt;

/// One thing wrong with one variable.
///
/// Each variant names the variable, because the first thing anyone does with
/// a configuration error is search the deployment for that name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A required variable is unset or blank.
    ///
    /// The wording matches the Python's `RuntimeError` so an existing
    /// runbook or log-search still finds it.
    #[error("missing required configuration: {name}")]
    Missing {
        /// The variable's name.
        name: String,
    },

    /// A variable is set to something that is not the type it must be.
    #[error("invalid configuration: {name} must be {expected}, got {got:?}")]
    Invalid {
        /// The variable's name.
        name: String,
        /// What was expected, as a noun phrase: `a positive integer`.
        expected: String,
        /// What was there. Configuration values are operational, not
        /// clinical, so quoting one is safe; see
        /// `spec/index.md` §0.8.
        got: String,
    },

    /// Two or more variables are individually fine but cannot be combined.
    ///
    /// `EGRESS_QUEUE_NAME` together with `EGRESS_TOPIC_NAME`, or
    /// `SQL_USERNAME` without `MSSQL_SA_PASSWORD`.
    #[error("{message}")]
    Conflict {
        /// The whole sentence, because a conflict is about a relationship
        /// and there is no single name to lead with.
        message: String,
    },
}

impl ConfigError {
    /// A required variable is unset or blank.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::ConfigError;
    ///
    /// let error = ConfigError::missing("EGRESS_QUEUE_NAME");
    /// assert_eq!(
    ///     error.to_string(),
    ///     "missing required configuration: EGRESS_QUEUE_NAME"
    /// );
    /// ```
    #[must_use]
    pub fn missing(name: impl Into<String>) -> Self {
        Self::Missing { name: name.into() }
    }

    /// A variable holds something of the wrong shape.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::ConfigError;
    ///
    /// let error = ConfigError::invalid("PORT", "a whole number", "two");
    /// assert_eq!(
    ///     error.to_string(),
    ///     r#"invalid configuration: PORT must be a whole number, got "two""#
    /// );
    /// ```
    #[must_use]
    pub fn invalid(
        name: impl Into<String>,
        expected: impl Into<String>,
        got: impl Into<String>,
    ) -> Self {
        Self::Invalid {
            name: name.into(),
            expected: expected.into(),
            got: got.into(),
        }
    }

    /// Variables that cannot be combined as configured.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::ConfigError;
    ///
    /// let error = ConfigError::conflict(
    ///     "Cannot specify both EGRESS_QUEUE_NAME and EGRESS_TOPIC_NAME.",
    /// );
    /// assert!(error.to_string().starts_with("Cannot specify both"));
    /// ```
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict {
            message: message.into(),
        }
    }
}

/// Every problem found while reading a configuration.
///
/// Constructed by [`crate::Reader::finish`]. It is never empty: a reader
/// with no problems returns `Ok` instead.
///
/// It renders one problem per line, so a container's last words are the
/// whole list rather than the first item of it
/// (`spec/02-configuration.md` §1.4).
///
/// # Examples
///
/// ```
/// use ihb_config::Reader;
///
/// let mut reader = Reader::from_pairs([("PORT", "two")]);
/// let _: String = reader.required_string("WORKFLOW_ID");
/// let _: u16 = reader.integer_or("PORT", 2575);
///
/// let errors = reader.finish().unwrap_err();
/// assert_eq!(errors.len(), 2);
/// assert_eq!(
///     errors.to_string(),
///     "missing required configuration: WORKFLOW_ID\n\
///      invalid configuration: PORT must be a whole number, got \"two\""
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigErrors(Vec<ConfigError>);

impl ConfigErrors {
    /// Wrap a non-empty list of problems.
    ///
    /// # Panics
    ///
    /// Panics if `errors` is empty. An empty error is not a thing this type
    /// can represent, and a caller that has none should be returning `Ok`.
    #[must_use]
    pub fn new(errors: Vec<ConfigError>) -> Self {
        assert!(
            !errors.is_empty(),
            "ConfigErrors must hold at least one problem"
        );
        Self(errors)
    }

    /// How many problems there are.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::{ConfigError, ConfigErrors};
    ///
    /// let errors = ConfigErrors::new(vec![ConfigError::missing("PORT")]);
    /// assert_eq!(errors.len(), 1);
    /// assert!(!errors.is_empty());
    /// ```
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Always false; see [`ConfigErrors::new`].
    ///
    /// Present because clippy asks for it beside [`ConfigErrors::len`], and
    /// because a caller writing `if errors.is_empty()` deserves an answer
    /// rather than a compile error.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The problems, in the order they were found.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::{ConfigError, ConfigErrors};
    ///
    /// let errors = ConfigErrors::new(vec![
    ///     ConfigError::missing("A"),
    ///     ConfigError::missing("B"),
    /// ]);
    /// let names: Vec<_> = errors
    ///     .iter()
    ///     .map(|error| match error {
    ///         ConfigError::Missing { name } => name.clone(),
    ///         _ => unreachable!(),
    ///     })
    ///     .collect();
    /// assert_eq!(names, ["A", "B"]);
    /// ```
    pub fn iter(&self) -> std::slice::Iter<'_, ConfigError> {
        self.0.iter()
    }
}

impl fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for error in &self.0 {
            if !first {
                writeln!(f)?;
            }
            write!(f, "{error}")?;
            first = false;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigErrors {}

impl<'a> IntoIterator for &'a ConfigErrors {
    type Item = &'a ConfigError;
    type IntoIter = std::slice::Iter<'a, ConfigError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_variable_names_itself() {
        // spec/02-configuration.md §1.3
        assert_eq!(
            ConfigError::missing("EGRESS_QUEUE_NAME").to_string(),
            "missing required configuration: EGRESS_QUEUE_NAME"
        );
    }

    #[test]
    fn problems_render_one_per_line() {
        // spec/02-configuration.md §1.4
        let errors = ConfigErrors::new(vec![
            ConfigError::missing("A"),
            ConfigError::invalid("B", "a whole number", "x"),
        ]);
        let text = errors.to_string();
        assert_eq!(text.lines().count(), 2);
        assert!(text.starts_with("missing required configuration: A\n"));
    }

    #[test]
    #[should_panic(expected = "at least one problem")]
    fn an_empty_error_list_is_not_representable() {
        let _ = ConfigErrors::new(Vec::new());
    }
}
