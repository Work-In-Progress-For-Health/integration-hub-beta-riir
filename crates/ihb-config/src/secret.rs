//! A configuration value that must not be printed.
//!
//! Implements `spec/02-configuration.md` §10.4.

use std::fmt;

/// What a secret renders as, everywhere.
const MASK: &str = "<redacted>";

/// A configuration value that never appears in output.
///
/// Wraps a connection string, a password, a shared access key or an API key.
/// Both [`Debug`] and [`std::fmt::Display`] render `<redacted>`; the value comes out
/// only through [`Secret::expose`], which is deliberately awkward to type
/// and easy to grep for.
///
/// Deriving `Debug` on a configuration struct is the normal thing to do and
/// the resulting line ends up in a log eventually. This type is what makes
/// that safe.
///
/// # Examples
///
/// ```
/// use ihb_config::Secret;
///
/// #[derive(Debug)]
/// struct Config {
///     namespace: String,
///     connection_string: Secret,
/// }
///
/// let config = Config {
///     namespace: "local-inthub".into(),
///     connection_string: Secret::new("Endpoint=sb://x;SharedAccessKey=hunter2"),
/// };
///
/// let printed = format!("{config:?}");
/// assert!(printed.contains("local-inthub"));
/// assert!(!printed.contains("hunter2"));
/// assert_eq!(config.connection_string.expose(), "Endpoint=sb://x;SharedAccessKey=hunter2");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a value.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Secret;
    ///
    /// assert_eq!(Secret::new("hunter2").to_string(), "<redacted>");
    /// ```
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value itself.
    ///
    /// Named to be conspicuous at a call site and in a code review: every
    /// use of it is a place a secret leaves this type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Secret;
    ///
    /// let secret = Secret::new("hunter2");
    /// assert_eq!(secret.expose(), "hunter2");
    /// ```
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the wrapped value is empty.
    ///
    /// Lets a caller branch on "is this configured" without exposing it.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_config::Secret;
    ///
    /// assert!(Secret::new("").is_empty());
    /// assert!(!Secret::new("hunter2").is_empty());
    /// ```
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value distinctive enough that finding it anywhere is unambiguous.
    const MARKER: &str = "SharedAccessKey=NEEDLE-9f2c";

    #[test]
    fn debug_does_not_print_the_value() {
        // spec/02-configuration.md §10.4
        let printed = format!("{:?}", Secret::new(MARKER));
        assert_eq!(printed, MASK);
        assert!(!printed.contains("NEEDLE"));
    }

    #[test]
    fn display_does_not_print_the_value() {
        // spec/02-configuration.md §10.4
        assert!(!Secret::new(MARKER).to_string().contains("NEEDLE"));
    }

    #[test]
    fn a_secret_inside_a_derived_debug_stays_hidden() {
        // spec/02-configuration.md §10.4 — the case that actually happens.
        // The fields are read by the derived `Debug` and nowhere else,
        // which is exactly what this test is about.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Config {
            /// A field whose presence proves the rest of the struct printed.
            queue: &'static str,
            /// The field under test.
            key: Secret,
        }

        let printed = format!(
            "{:?}",
            Config {
                queue: "local-inthub-messagestore",
                key: Secret::new(MARKER),
            }
        );
        assert!(printed.contains("local-inthub-messagestore"));
        assert!(!printed.contains("NEEDLE"));
    }

    #[test]
    fn the_value_is_reachable_when_asked_for() {
        assert_eq!(Secret::new(MARKER).expose(), MARKER);
    }
}
