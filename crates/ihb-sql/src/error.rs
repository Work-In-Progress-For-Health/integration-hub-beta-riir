//! What can go wrong talking to the database.
//!
//! Implements the error half of `spec/12-message-store.md` §5.7.

/// A database failure.
///
/// # No credential, ever
///
/// §5.7: a connection string, a username or a password MUST NOT appear in
/// any log line or error, at any level. `tiberius`'s own errors do not
/// carry them, and the variants here carry a *reason*, never the
/// configuration that produced it.
///
/// That is why [`SqlError::Connect`] holds a string rather than the
/// underlying error's `Debug`: a `Debug` rendering of a connection
/// configuration is exactly the thing this forbids.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SqlError {
    /// The connection could not be opened.
    #[error("could not connect to {server}: {reason}")]
    Connect {
        /// The server name. Not a credential.
        server: String,
        /// Why, without any credential in it.
        reason: String,
    },

    /// A statement failed.
    #[error("the statement failed: {0}")]
    Statement(String),

    /// The transaction could not be committed.
    #[error("the transaction could not be committed: {0}")]
    Commit(String),

    /// An AAD token could not be obtained.
    #[error("could not obtain a database access token: {0}")]
    Token(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_names_a_server_but_never_a_credential() {
        // spec/12-message-store.md §5.7 and §6.7.
        let error = SqlError::Connect {
            server: "sqlserver".to_string(),
            reason: "connection refused".to_string(),
        };

        let rendered = error.to_string();
        assert!(rendered.contains("sqlserver"));
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.to_lowercase().contains("password"));
    }
}
