//! What can be wrong with a message, said without saying what is in it.
//!
//! Implements `spec/06-validation.md` §8.
//!
//! This module is where `spec/index.md` §0.8 is enforced by the type system
//! rather than by care. Every variant below carries a **structural
//! location** — a path, a segment name, a structure id — and a **reason**.
//! None of them can hold a field value, because none of them has anywhere
//! to put one.
//!
//! The Python needed a whole function for this
//! (`validate.py :: _format_schema_validation_error`), because `xmlschema`'s
//! own error string embeds the entire instance document, patient data and
//! all, and that string was reaching application logs. Making the error type
//! incapable of carrying a value is a stronger guarantee than remembering to
//! strip one.

/// Why a message could not be validated or converted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// The ER7 could not be parsed at all.
    ///
    /// The wording matches the Python's `PARSE_ERROR_MSG` prefix, which its
    /// own tests assert on (`spec/06-validation.md` §6.4).
    #[error("Unable to parse ER7 message: {reason}")]
    Parse {
        /// Why the parse failed. Structural: a missing header, unreadable
        /// delimiters. Never a field value.
        reason: String,
    },

    /// Neither `MSH-9.3` nor `MSH-9.2` was present, so there is nothing to
    /// say what shape the message claims to be
    /// (`spec/06-validation.md` §2.3).
    #[error("Unable to determine structure or trigger from ER7 message")]
    Structure,

    /// The flow has no schema for this message structure.
    ///
    /// Lists what it does have, because the next question an operator asks
    /// is "then what does it accept?" (`spec/06-validation.md` §3.1.2).
    #[error(
        "No XSD mapping for flow '{flow}' and trigger/structure '{structure}'. Available: {available}"
    )]
    NoSchema {
        /// The schema group that was asked.
        flow: String,
        /// The structure that was not found.
        structure: String,
        /// The structures the group does have, comma-separated and sorted,
        /// or `<none>`.
        available: String,
    },

    /// The message contradicts the schema its flow validates against.
    #[error("{reason} (path: {path})")]
    Schema {
        /// Where, as an `er7` path or a segment name.
        path: String,
        /// What is wrong, structurally.
        reason: String,
    },

    /// An HL7 release this workspace does not validate against
    /// (`spec/06-validation.md` §6.1).
    #[error("Unsupported HL7 version '{version}'. Supported versions: {supported}")]
    UnsupportedVersion {
        /// What was asked for.
        version: String,
        /// What is available, comma-separated and sorted.
        supported: String,
    },

    /// The message declares one release and was asked to be validated as
    /// another (`spec/06-validation.md` §6.2).
    #[error("Message version {actual} does not match requested version {requested}")]
    VersionMismatch {
        /// What `MSH-12` says.
        actual: String,
        /// What the caller asked for.
        requested: String,
    },

    /// Validation against a standard release failed
    /// (`spec/06-validation.md` §6.3).
    #[error("Standard HL7 v{version} validation failed: {reason}")]
    Standard {
        /// The release validated against.
        version: String,
        /// What is wrong, structurally.
        reason: String,
    },

    /// A v2.xml document could not be read or written.
    #[error("{reason}")]
    Xml {
        /// What went wrong, structurally.
        reason: String,
    },
}

impl ValidationError {
    /// A parse failure.
    #[must_use]
    pub fn parse(reason: impl std::fmt::Display) -> Self {
        Self::Parse {
            reason: reason.to_string(),
        }
    }

    /// A schema diagnostic.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_validation::ValidationError;
    ///
    /// let error = ValidationError::schema("PID", "structure ADT_A05 requires a PID segment");
    /// assert_eq!(
    ///     error.to_string(),
    ///     "structure ADT_A05 requires a PID segment (path: PID)"
    /// );
    /// ```
    #[must_use]
    pub fn schema(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Schema {
            path: path.into(),
            reason: reason.into(),
        }
    }

    /// A v2.xml failure.
    #[must_use]
    pub fn xml(reason: impl std::fmt::Display) -> Self {
        Self::Xml {
            reason: reason.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_schema_error_renders_as_the_python_did() {
        // spec/06-validation.md §3.3.4
        let error = ValidationError::schema("/ns0:ADT_A05", "Tag EVN expected");
        assert_eq!(error.to_string(), "Tag EVN expected (path: /ns0:ADT_A05)");
    }

    #[test]
    fn a_missing_schema_lists_what_is_available() {
        // spec/06-validation.md §3.1.2 — the operator's next question.
        let error = ValidationError::NoSchema {
            flow: "phw".into(),
            structure: "ADT_A03".into(),
            available: "ADT_A05, ADT_A39".into(),
        };
        assert_eq!(
            error.to_string(),
            "No XSD mapping for flow 'phw' and trigger/structure 'ADT_A03'. \
             Available: ADT_A05, ADT_A39"
        );
    }

    #[test]
    fn a_parse_failure_keeps_the_pythons_prefix() {
        // spec/06-validation.md §6.4 — the Python's own tests match on it.
        let error = ValidationError::parse("no MSH header");
        assert!(error.to_string().starts_with("Unable to parse ER7 message"));
    }

    #[test]
    fn an_unsupported_version_lists_the_supported_ones() {
        // spec/06-validation.md §6.1
        let error = ValidationError::UnsupportedVersion {
            version: "2.9".into(),
            supported: "2.4, 2.5, 2.5.1, 2.6".into(),
        };
        assert_eq!(
            error.to_string(),
            "Unsupported HL7 version '2.9'. Supported versions: 2.4, 2.5, 2.5.1, 2.6"
        );
    }

    #[test]
    fn a_version_mismatch_names_both_versions() {
        // spec/06-validation.md §6.2
        let error = ValidationError::VersionMismatch {
            actual: "2.4".into(),
            requested: "2.5".into(),
        };
        assert_eq!(
            error.to_string(),
            "Message version 2.4 does not match requested version 2.5"
        );
    }

    #[test]
    fn a_structure_failure_says_so_without_quoting_the_message() {
        // spec/06-validation.md §2.3
        assert_eq!(
            ValidationError::Structure.to_string(),
            "Unable to determine structure or trigger from ER7 message"
        );
    }
}
