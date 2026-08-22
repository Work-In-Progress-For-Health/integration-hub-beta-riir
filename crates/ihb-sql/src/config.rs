//! How to reach the database.
//!
//! Implements `spec/12-message-store.md` §5.4 and §5.5, and
//! `spec/02-configuration.md` §7.1.

use ihb_config::{ConfigErrors, Reader, Secret};

/// How the service authenticates.
///
/// `spec/12-message-store.md` §5.4 makes this a closed choice rather than
/// a pile of optional fields: a username without a password, or a password
/// without a username, is a **startup failure** rather than a silent
/// fallback to the other mode. A deployment that meant to use a password
/// and typoed the variable name would otherwise try managed identity, fail
/// obscurely against a server that has never heard of it, and look like a
/// network problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// `SQL_USERNAME` and `MSSQL_SA_PASSWORD`, both set.
    Password {
        /// `SQL_USERNAME`.
        username: String,
        /// `MSSQL_SA_PASSWORD`. Note the name: **not** `SQL_PASSWORD`.
        password: Secret,
    },
    /// Neither set: an AAD token from the ambient credential.
    ManagedIdentity {
        /// `MANAGED_IDENTITY_CLIENT_ID`, for a user-assigned identity.
        /// Absent uses the system-assigned one.
        client_id: Option<String>,
    },
}

impl Auth {
    /// Which mode this is, for a log line.
    ///
    /// Never the credential itself — `spec/12-message-store.md` §5.7.
    #[must_use]
    pub const fn describe(&self) -> &'static str {
        match self {
            Self::Password { .. } => "password",
            Self::ManagedIdentity { .. } => "managed identity",
        }
    }
}

/// Everything needed to open a connection.
///
/// # Examples
///
/// ```
/// use ihb_sql::config::{Auth, SqlConfig};
///
/// let config = SqlConfig::from_pairs([
///     ("SQL_SERVER", "sqlserver"),
///     ("SQL_DATABASE", "monitoring"),
///     ("SQL_USERNAME", "sa"),
///     ("MSSQL_SA_PASSWORD", "a-password"),
/// ])
/// .expect("both credentials are present");
///
/// assert_eq!(config.auth.describe(), "password");
/// assert!(config.encrypt, "encryption is on by default");
/// assert!(!config.trust_server_certificate, "and trust is not");
/// ```
///
/// One credential without the other is refused, naming the missing one:
///
/// ```
/// use ihb_sql::config::SqlConfig;
///
/// let errors = SqlConfig::from_pairs([
///     ("SQL_SERVER", "sqlserver"),
///     ("SQL_DATABASE", "monitoring"),
///     ("SQL_USERNAME", "sa"),
/// ])
/// .unwrap_err();
///
/// assert!(errors.to_string().contains("MSSQL_SA_PASSWORD"), "{errors}");
/// ```
#[derive(Debug, Clone)]
pub struct SqlConfig {
    /// `SQL_SERVER`.
    pub server: String,
    /// `SQL_DATABASE`.
    pub database: String,
    /// How to authenticate.
    pub auth: Auth,
    /// `SQL_ENCRYPT`.
    pub encrypt: bool,
    /// `SQL_TRUST_SERVER_CERTIFICATE`.
    pub trust_server_certificate: bool,
}

impl SqlConfig {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_env() -> Result<Self, ConfigErrors> {
        Self::read(&mut Reader::from_env())
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
        let mut reader = Reader::from_pairs(pairs);
        let config = Self::read(&mut reader)?;
        reader.finish()?;
        Ok(config)
    }

    /// Read from a reader a caller is also using for its own variables.
    ///
    /// The services here read SQL settings alongside their own, and
    /// `spec/02-configuration.md` §1.4 wants **every** problem reported at
    /// once — so this shares the caller's reader rather than opening a
    /// second one whose errors would be reported separately.
    ///
    /// # Errors
    ///
    /// Only the credential mismatch of §5.4, which cannot be deferred to
    /// [`Reader::finish`] because it decides the shape of [`Auth`].
    pub fn read(reader: &mut Reader) -> Result<Self, ConfigErrors> {
        let server = reader.required_string("SQL_SERVER");
        let database = reader.required_string("SQL_DATABASE");

        let username = reader.optional_string("SQL_USERNAME");
        let password = reader.optional_secret("MSSQL_SA_PASSWORD");

        let auth = match (username, password) {
            (Some(username), Some(password)) => Auth::Password { username, password },
            (None, None) => Auth::ManagedIdentity {
                client_id: reader.optional_string("MANAGED_IDENTITY_CLIENT_ID"),
            },
            // §5.4's third row. Naming the *missing* one is the useful half:
            // whoever set the other knows which mode they meant.
            (Some(_), None) => {
                reader.conflict(
                    "SQL_USERNAME is set without MSSQL_SA_PASSWORD; \
                     password authentication needs both",
                );
                Auth::ManagedIdentity { client_id: None }
            }
            (None, Some(_)) => {
                reader.conflict(
                    "MSSQL_SA_PASSWORD is set without SQL_USERNAME; \
                     password authentication needs both",
                );
                Auth::ManagedIdentity { client_id: None }
            }
        };

        Ok(Self {
            server,
            database,
            auth,
            // §5.5 — `Yes`/`No`, case-insensitive. Encryption defaults on
            // and certificate trust defaults off, which are the safe ends
            // of both.
            encrypt: yes_or_no(reader, "SQL_ENCRYPT", true),
            trust_server_certificate: yes_or_no(reader, "SQL_TRUST_SERVER_CERTIFICATE", false),
        })
    }
}

/// A `Yes`/`No` variable (§5.5).
///
/// Not [`Reader::bool_or`], which follows `spec/02-configuration.md` §1.5 —
/// "any non-blank value that is not `false`/`0`/`no`/`off` is true". These
/// two variables are documented as `Yes`/`No`, and a typo like `Ues` must
/// not silently enable certificate trust.
fn yes_or_no(reader: &mut Reader, name: &str, default: bool) -> bool {
    let Some(value) = reader.raw(name).map(str::to_string) else {
        return default;
    };

    match value.to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" => true,
        "no" | "false" | "0" => false,
        _ => {
            reader.invalid(name, "Yes or No", value);
            default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Vec<(&'static str, &'static str)> {
        vec![("SQL_SERVER", "sqlserver"), ("SQL_DATABASE", "monitoring")]
    }

    fn with(extra: &[(&str, &str)]) -> Result<SqlConfig, ConfigErrors> {
        let mut pairs: Vec<(String, String)> = base()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        for (key, value) in extra {
            pairs.push(((*key).to_string(), (*value).to_string()));
        }
        SqlConfig::from_pairs(pairs)
    }

    #[test]
    fn every_row_of_the_authentication_table() {
        // spec/12-message-store.md §5.4 and §6.4.

        // Both set: password.
        let password = with(&[("SQL_USERNAME", "sa"), ("MSSQL_SA_PASSWORD", "p")]).unwrap();
        assert_eq!(
            password.auth,
            Auth::Password {
                username: "sa".to_string(),
                password: Secret::new("p"),
            }
        );

        // Neither: managed identity.
        let managed = with(&[]).unwrap();
        assert_eq!(managed.auth, Auth::ManagedIdentity { client_id: None });

        // Exactly one: a startup failure, naming the missing one.
        let no_password = with(&[("SQL_USERNAME", "sa")]).unwrap_err();
        assert!(
            no_password.to_string().contains("MSSQL_SA_PASSWORD"),
            "{no_password}"
        );

        let no_username = with(&[("MSSQL_SA_PASSWORD", "p")]).unwrap_err();
        assert!(
            no_username.to_string().contains("SQL_USERNAME"),
            "{no_username}"
        );
    }

    #[test]
    fn a_user_assigned_identity_is_carried() {
        let config = with(&[("MANAGED_IDENTITY_CLIENT_ID", "abc-123")]).unwrap();

        assert_eq!(
            config.auth,
            Auth::ManagedIdentity {
                client_id: Some("abc-123".to_string()),
            }
        );
    }

    #[test]
    fn the_password_variable_is_not_called_sql_password() {
        // `spec/02-configuration.md` §7.1 flags this: the variable is
        // `MSSQL_SA_PASSWORD`, inherited from the SQL Server image. Reading
        // `SQL_PASSWORD` instead would silently select managed identity.
        let wrong_name = with(&[("SQL_USERNAME", "sa"), ("SQL_PASSWORD", "p")]).unwrap_err();

        assert!(
            wrong_name.to_string().contains("MSSQL_SA_PASSWORD"),
            "the real variable is named in the error: {wrong_name}"
        );
    }

    #[test]
    fn encryption_defaults_on_and_trust_defaults_off() {
        // §5.5 — the safe end of both.
        let config = with(&[]).unwrap();
        assert!(config.encrypt);
        assert!(!config.trust_server_certificate);
    }

    #[test]
    fn yes_and_no_are_read_case_insensitively() {
        for value in ["Yes", "yes", "YES", "true", "1"] {
            assert!(
                with(&[("SQL_TRUST_SERVER_CERTIFICATE", value)])
                    .unwrap()
                    .trust_server_certificate,
                "{value}"
            );
        }
        for value in ["No", "no", "NO", "false", "0"] {
            assert!(!with(&[("SQL_ENCRYPT", value)]).unwrap().encrypt, "{value}");
        }
    }

    #[test]
    fn a_typo_is_a_startup_failure_rather_than_a_silent_default() {
        // The reason this is not `bool_or`: under §1.5's rule, `Ues` is a
        // non-blank value that is not `false`, so it would read as **true**
        // and silently trust any certificate the server presented.
        let errors = with(&[("SQL_TRUST_SERVER_CERTIFICATE", "Ues")]).unwrap_err();

        assert!(
            errors.to_string().contains("SQL_TRUST_SERVER_CERTIFICATE"),
            "{errors}"
        );
    }

    #[test]
    fn a_password_never_renders() {
        // §5.7 — not in any log line or error, at any level.
        let config = with(&[("SQL_USERNAME", "sa"), ("MSSQL_SA_PASSWORD", "hunter2")]).unwrap();

        let rendered = format!("{config:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("sa"), "the username is not a secret");
    }
}
