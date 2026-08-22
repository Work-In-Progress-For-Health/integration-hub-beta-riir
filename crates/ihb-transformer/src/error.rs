//! What can go wrong mapping a message.
//!
//! Implements `spec/09-transformers.md` §1.6.

/// A message that could not be mapped.
///
/// `spec/index.md` §0.8: an error type carries a **structural location** —
/// a segment, a field path — and a reason, never a value. That rule is
/// sharper here than almost anywhere else, because a transform error is
/// logged with the field it failed on, and the fields a transformer touches
/// are names, addresses and dates of birth.
///
/// The one deliberate exception is [`TransformError::Datetime`], whose
/// message is normative: `spec/09-transformers.md` §5.6 gives the exact
/// text, quoting the offending value, and the source systems' support teams
/// match on it. A timestamp is not patient-identifying.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransformError {
    /// A field could not be read or written.
    #[error("could not map {path}: {reason}")]
    Field {
        /// The HL7 path, e.g. `PID-3[2].1`.
        path: String,
        /// What went wrong, structurally.
        reason: String,
    },

    /// A timestamp did not parse.
    ///
    /// The text is normative; see the type documentation.
    #[error("{0}")]
    Datetime(String),

    /// The message could not be re-parsed.
    ///
    /// PIMS re-parses flat before mapping
    /// (`spec/09-transformers.md` §5.0), which is the only place this
    /// arises.
    #[error("could not re-parse the message: {0}")]
    Parse(String),
}

impl TransformError {
    /// A field-level failure at `path`.
    #[must_use]
    pub fn field(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Field {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

impl From<hl7_2::Error> for TransformError {
    /// An `hl7-2` error, with its own message as the reason.
    ///
    /// `hl7-2`'s errors name paths and structures, not values, so this does
    /// not smuggle a field value into a log line.
    fn from(error: hl7_2::Error) -> Self {
        Self::Field {
            path: "a field".to_string(),
            reason: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_names_a_place_and_not_a_value() {
        // spec/index.md §0.8 clause 3, and clause 4's test.
        let error = TransformError::field("PID-3[2].1", "no such repetition");

        let rendered = error.to_string();
        assert!(rendered.contains("PID-3[2].1"));
        assert!(!rendered.contains("8888888"), "{rendered}");
    }

    #[test]
    fn the_datetime_message_is_carried_verbatim() {
        // spec/09-transformers.md §5.6 — normative text, matched on by the
        // source systems' support teams.
        let text = "Invalid datetime format after timezone removal: 'x'. \
                    Expected format: YYYYMMDD (8 digits) or YYYYMMDDHHMMSS (14 digits)";

        assert_eq!(TransformError::Datetime(text.to_string()).to_string(), text);
    }
}
