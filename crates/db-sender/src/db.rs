//! The connection, the stored-procedure call and the failure classes.
//!
//! Implements `spec/10-senders.md` §7.2 step 5 and §7.3. Upstream:
//! `db_sender/db_client.py`.
//!
//! This does not use `ihb_sql::SqlConnection`: that one reads `SQL_*`
//! variables, hard-codes port 1433 and has no connect timeout, and the
//! upstream service has its own `DB_*` contract (`DB_PORT`,
//! `DB_CONNECTION_TIMEOUT`). The rules it embodies are kept: **lazy**, **one
//! long-lived connection**, **discarded on any error**
//! (`spec/12-message-store.md` §5.1, §5.2) and **no credential in any log
//! line** (§5.7).

use std::future::Future;
use std::time::Duration;

use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::config::{DbAuth, DbConfig};
use crate::idupdate::{IdUpdate, IdentifierUpdate};
use crate::sanitize::sanitize_error_message;
use crate::trrr::TrrrRequest;

/// SQL Server native error codes that are worth a retry (`§7.3`).
///
/// Upstream `TRANSIENT_ERROR_CODES`, less `-2`: that is the ODBC client
/// timeout, which here is our own [`tokio::time::timeout`] and is classified
/// at the call site.
const TRANSIENT_ERROR_CODES: [u32; 4] = [
    1205,  // deadlock victim
    233,   // connection initialisation error
    10054, // connection reset by peer
    10060, // connection timed out
];

/// Which way a failure goes (`§7.3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Temporary: abandon the message and retry.
    Transient,
    /// Permanent: dead-letter the message so it does not block the session.
    Fatal,
}

/// A failed stored-procedure call.
///
/// The message is sanitised on construction (`§7.4`), so nothing downstream
/// can forget to do it: a [`DatabaseError`] never holds a value the server
/// echoed.
///
/// # Examples
///
/// ```
/// use db_sender::{DatabaseError, FailureKind};
///
/// let error = DatabaseError::new(
///     FailureKind::Fatal,
///     245,
///     "Conversion failed when converting the varchar value '9999999999' to data type int.",
/// );
/// assert!(!error.message.contains("9999999999"));
/// assert_eq!(error.description(), format!("SQLState: UNKNOWN, Code: 245: {}", error.message));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DatabaseError {
    /// Retry or dead-letter.
    pub kind: FailureKind,
    /// The SQL Server native error code, `0` when there is none.
    pub code: u32,
    /// The sanitised message.
    pub message: String,
}

impl DatabaseError {
    /// A failure, sanitising `message`.
    #[must_use]
    pub fn new(kind: FailureKind, code: u32, message: &str) -> Self {
        Self {
            kind,
            code,
            message: sanitize_error_message(message),
        }
    }

    /// The dead-letter description (`§7.3`).
    ///
    /// TDS carries no SQLSTATE, so that field is `UNKNOWN`, the Python's own
    /// default for "not known".
    #[must_use]
    pub fn description(&self) -> String {
        format!("SQLState: UNKNOWN, Code: {}: {}", self.code, self.message)
    }
}

/// Classify a failure (`§7.3`).
///
/// `code` is the native error code when the server reported one. Matching
/// the Python's `_is_transient`: a known code, or a message that mentions a
/// timeout, deadlock or communication-link failure.
///
/// # Examples
///
/// ```
/// use db_sender::db::classify;
/// use db_sender::FailureKind;
///
/// assert_eq!(classify(Some(1205), "deadlocked"), FailureKind::Transient);
/// assert_eq!(classify(Some(2812), "Could not find stored procedure"), FailureKind::Fatal);
/// assert_eq!(classify(None, "Query TIMEOUT expired"), FailureKind::Transient);
/// ```
#[must_use]
pub fn classify(code: Option<u32>, message: &str) -> FailureKind {
    if code.is_some_and(|c| TRANSIENT_ERROR_CODES.contains(&c)) {
        return FailureKind::Transient;
    }
    let lower = message.to_ascii_lowercase();
    if lower.contains("timeout")
        || lower.contains("deadlock")
        || lower.contains("communication link failure")
    {
        FailureKind::Transient
    } else {
        FailureKind::Fatal
    }
}

/// Convert a `tiberius` error (`§7.3`).
///
/// Only an error **the server reported** can be fatal. Everything else (I/O,
/// protocol, TLS, a redirect) is a failure of the path to the server and
/// says nothing about the message, so it is transient. That is upstream's
/// split too: its `except Exception` arm was a [`FailureKind::Transient`].
#[must_use]
pub fn from_tiberius(error: &tiberius::error::Error) -> DatabaseError {
    match error {
        tiberius::error::Error::Server(token) => DatabaseError::new(
            classify(Some(token.code()), token.message()),
            token.code(),
            token.message(),
        ),
        other => DatabaseError::new(FailureKind::Transient, 0, &other.to_string()),
    }
}

/// Refuse a stored procedure that reported failure by return code
/// (`spec/10-senders.md` §7.3a).
///
/// WRRS's `dbo.prQueueMessage` signals a rejected message with
/// `RETURN 55555` and **no SQL error** (upstream #446's README), so a call
/// that raised nothing is not proof the message was stored. Any non-zero
/// code is a failure: a rejection is a property of the content, so it is
/// [`FailureKind::Fatal`] (a retry would be refused again). Only the code is
/// reported; the procedure has no message text and none is invented, so no
/// patient data can reach the dead-letter queue by this route.
///
/// The code is the last row of the last result set (the `SELECT @rc` the
/// statement ends with); a missing one is treated as a failure too, since
/// acknowledging on no evidence is the one thing this service must not do.
fn check_return_code(results: &[Vec<tiberius::Row>]) -> Result<(), DatabaseError> {
    let code = results
        .last()
        .and_then(|rows| rows.first())
        .and_then(|row| row.try_get::<i32, _>(0).ok().flatten());
    match code {
        Some(0) => Ok(()),
        Some(code) => Err(DatabaseError::new(
            FailureKind::Fatal,
            code.unsigned_abs(),
            &format!("the stored procedure returned code {code}; nothing was stored"),
        )),
        None => Err(DatabaseError::new(
            FailureKind::Fatal,
            0,
            "the stored procedure returned no return code",
        )),
    }
}

/// Something that can run the configured stored procedure.
///
/// A trait so the processor is tested without SQL Server
/// (`spec/10-senders.md` §7.7).
pub trait ProcedureRunner: Send {
    /// Execute the procedure with `@Message` and `@IsPriority` and commit.
    ///
    /// # Errors
    ///
    /// A [`DatabaseError`], already classified and sanitised.
    fn execute(
        &mut self,
        message: &str,
        is_priority: bool,
    ) -> impl Future<Output = Result<(), DatabaseError>> + Send;

    /// Execute `dbo.prQueueIdUpdate` with the six parameters of `update` and
    /// commit (`spec/10-senders.md` §7.8).
    ///
    /// Defaults to a fatal "not supported" so a runner that only knows the
    /// message procedure need not implement it; [`SqlRunner`] does.
    ///
    /// # Errors
    ///
    /// A [`DatabaseError`], already classified and sanitised.
    fn execute_id_update(
        &mut self,
        update: &IdUpdate,
    ) -> impl Future<Output = Result<(), DatabaseError>> + Send {
        let _ = update;
        async {
            Err(DatabaseError::new(
                FailureKind::Fatal,
                0,
                "this runner cannot execute the Queue ID Update",
            ))
        }
    }

    /// Execute `dbo.spUpdateIdentifier` with three strings and commit
    /// (`spec/10-senders.md` §7.9).
    ///
    /// Defaults to a fatal "not supported", as [`Self::execute_id_update`].
    ///
    /// # Errors
    ///
    /// A [`DatabaseError`], already classified and sanitised.
    fn execute_update_identifier(
        &mut self,
        update: &IdentifierUpdate,
    ) -> impl Future<Output = Result<(), DatabaseError>> + Send {
        let _ = update;
        async {
            Err(DatabaseError::new(
                FailureKind::Fatal,
                0,
                "this runner cannot execute the identifier update",
            ))
        }
    }

    /// Execute a TRRR procedure with three or four strings and commit
    /// (`spec/10-senders.md` §7.10).
    ///
    /// Defaults to a fatal "not supported", as [`Self::execute_id_update`].
    ///
    /// # Errors
    ///
    /// A [`DatabaseError`], already classified and sanitised.
    fn execute_trrr(
        &mut self,
        request: &TrrrRequest,
    ) -> impl Future<Output = Result<(), DatabaseError>> + Send {
        let _ = request;
        async {
            Err(DatabaseError::new(
                FailureKind::Fatal,
                0,
                "this runner cannot execute the TRRR procedures",
            ))
        }
    }
}

/// A `tiberius` client over a Tokio socket.
type Connected = Client<Compat<TcpStream>>;

/// The real runner: a lazily-opened, long-lived connection.
pub struct SqlRunner {
    /// Where and how to connect.
    config: DbConfig,
    /// `EXEC @rc = dbo.proc @P1, @P2` and `SELECT @rc`, built once from the validated name.
    statement: String,
    /// `EXEC dbo.proc @P1 .. @P6`, for the Queue ID Update.
    id_update_statement: String,
    /// `EXEC dbo.proc @P1 .. @P3`, for the WCRS identifier update.
    update_identifier_statement: String,
    /// `EXEC dbo.proc @P1 .. @P3` and `@P1 .. @P4`: the TRRR association and merge.
    trrr_statements: (String, String),
    /// The managed identity, when that is the mode; its token cache must
    /// outlive a reconnect (`spec/12-message-store.md` §5.4.2).
    identity: Option<ihb_identity::ManagedIdentity>,
    /// The connection, once opened.
    client: Option<Connected>,
}

impl std::fmt::Debug for SqlRunner {
    /// Hand-written so a future field cannot leak a credential
    /// (`spec/12-message-store.md` §5.7).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlRunner")
            .field("server", &self.config.server)
            .field("database", &self.config.database)
            .field("open", &self.client.is_some())
            // The config (with its password) and the statement are left out on purpose.
            .finish_non_exhaustive()
    }
}

impl SqlRunner {
    /// A runner that connects on first use (`spec/12-message-store.md` §5.1:
    /// a database that is briefly down must not stop the service starting).
    #[must_use]
    pub fn new(config: DbConfig) -> Self {
        // The name was validated at startup (`config::validate_procedure`),
        // which is what makes splicing it into SQL acceptable. Positional
        // parameters, as the Python's `{CALL proc (?, ?)}`.
        let statement = format!(
            "DECLARE @rc INT; EXEC @rc = {} @P1, @P2; SELECT @rc;",
            config.stored_procedure
        );
        let id_update_statement = format!(
            "DECLARE @rc INT; EXEC @rc = {} @P1, @P2, @P3, @P4, @P5, @P6; SELECT @rc;",
            config.stored_procedure
        );
        let update_identifier_statement = format!(
            "DECLARE @rc INT; EXEC @rc = {} @P1, @P2, @P3; SELECT @rc;",
            config.stored_procedure
        );
        let trrr_statements = (
            format!(
                "DECLARE @rc INT; EXEC @rc = {} @P1, @P2, @P3; SELECT @rc;",
                config.stored_procedure
            ),
            format!(
                "DECLARE @rc INT; EXEC @rc = {} @P1, @P2, @P3, @P4; SELECT @rc;",
                config.stored_procedure
            ),
        );
        let identity = match &config.auth {
            DbAuth::ManagedIdentity { client_id } => {
                Some(ihb_identity::ManagedIdentity::new(client_id.clone()))
            }
            DbAuth::Password { .. } => None,
        };
        Self {
            config,
            statement,
            id_update_statement,
            update_identifier_statement,
            trrr_statements,
            identity,
            client: None,
        }
    }

    /// Open a connection, bounded by `DB_CONNECTION_TIMEOUT`.
    async fn open(&self) -> Result<Connected, DatabaseError> {
        let mut config = Config::new();
        config.host(&self.config.server);
        config.port(self.config.port);
        config.database(&self.config.database);
        // `spec/12-message-store.md` §5.8: `Required` is TLS or no
        // connection, never a quiet downgrade to plaintext.
        config.encryption(if self.config.encrypt {
            EncryptionLevel::Required
        } else {
            EncryptionLevel::NotSupported
        });
        if self.config.trust_server_certificate {
            config.trust_cert();
        } else {
            // The image has no system trust store (spec/18 §4.4).
            config.trust_webpki_roots();
        }
        // `tiberius` 0.13 bounds a command at 30 s by default. This
        // sender's own `DB_COMMAND_TIMEOUT` is the authority (§7.3), so the
        // driver's must not preempt it with a different error.
        config.command_timeout(None);
        match &self.config.auth {
            DbAuth::Password { username, password } => {
                config.authentication(AuthMethod::sql_server(username, password.expose()));
            }
            DbAuth::ManagedIdentity { .. } => {
                // `spec/12-message-store.md` §5.4.1. A failure to get a token
                // is transient: the identity endpoint can be briefly down at
                // start-up, and the message must be abandoned for redelivery,
                // never dead-lettered for a fault that is not in it.
                let token = match &self.identity {
                    Some(identity) => identity.sql_token().await,
                    None => unreachable!("a managed-identity config always builds an identity"),
                }
                .map_err(|error| {
                    DatabaseError::new(FailureKind::Transient, 0, &error.to_string())
                })?;
                config.authentication(AuthMethod::aad_token(token));
            }
        }

        tracing::info!(
            server = %self.config.server,
            database = %self.config.database,
            "opening a database connection"
        );
        let address = format!("{}:{}", self.config.server, self.config.port);
        let timeout = Duration::from_secs(self.config.connection_timeout_secs);
        let connect = async {
            let stream = TcpStream::connect(&address)
                .await
                .map_err(|e| DatabaseError::new(FailureKind::Transient, 0, &e.to_string()))?;
            stream.set_nodelay(true).ok();
            // A login the server refused (18456) is `Error::Server`, so it
            // is classified fatal, as upstream's `pyodbc.Error` was.
            // Boxed: the TLS handshake future is ~17 KB (`large_futures`).
            Box::pin(Client::connect(config, stream.compat_write()))
                .await
                .map_err(|e| from_tiberius(&e))
        };
        tokio::time::timeout(timeout, connect)
            .await
            .unwrap_or_else(|_| {
                Err(DatabaseError::new(
                    FailureKind::Transient,
                    0,
                    "connection timeout expired",
                ))
            })
    }

    /// Run the call inside an explicit transaction.
    ///
    /// Autocommit off, commit on success (upstream `autocommit=False`): a
    /// procedure that does two writes lands both or neither.
    async fn run(
        &self,
        client: &mut Connected,
        statement: &str,
        parameters: &[&dyn tiberius::ToSql],
    ) -> Result<(), DatabaseError> {
        client
            .simple_query("BEGIN TRANSACTION")
            .await
            .map_err(|e| from_tiberius(&e))?
            .into_results()
            .await
            .map_err(|e| from_tiberius(&e))?;
        // `query`, not `execute`: `execute` returns only row counts, and the
        // procedure's return code arrives as the final one-row result set
        // (`spec/10-senders.md` §7.3a).
        let results = client
            .query(statement, parameters)
            .await
            .map_err(|e| from_tiberius(&e))?
            .into_results()
            .await
            .map_err(|e| from_tiberius(&e))?;
        check_return_code(&results)?;
        client
            .simple_query("COMMIT TRANSACTION")
            .await
            .map_err(|e| from_tiberius(&e))?
            .into_results()
            .await
            .map_err(|e| from_tiberius(&e))?;
        Ok(())
    }
}

impl SqlRunner {
    /// Open or reuse the connection, run `statement` with `parameters` in a
    /// transaction, and keep the connection only if it succeeded.
    async fn call(
        &mut self,
        statement: &str,
        parameters: &[&dyn tiberius::ToSql],
    ) -> Result<(), DatabaseError> {
        let mut client = match self.client.take() {
            Some(client) => client,
            None => self.open().await?,
        };

        let command_timeout = Duration::from_secs(self.config.command_timeout_secs);
        let outcome = tokio::time::timeout(
            command_timeout,
            self.run(&mut client, statement, parameters),
        )
        .await
        .unwrap_or_else(|_| {
            Err(DatabaseError::new(
                FailureKind::Transient,
                0,
                "command timeout expired",
            ))
        });

        match outcome {
            Ok(()) => {
                // Healthy, reused by the next message.
                self.client = Some(client);
                Ok(())
            }
            Err(error) => {
                // Not put back (`spec/12-message-store.md` §5.2): after any
                // failure the transaction state is unknown. Dropping the
                // socket rolls an open transaction back server-side, and the
                // next call reconnects.
                tracing::info!("discarding the database connection; the next call will reconnect");
                Err(error)
            }
        }
    }
}

impl ProcedureRunner for SqlRunner {
    async fn execute(&mut self, message: &str, is_priority: bool) -> Result<(), DatabaseError> {
        let statement = self.statement.clone();
        self.call(&statement, &[&message, &is_priority]).await
    }

    async fn execute_id_update(&mut self, update: &IdUpdate) -> Result<(), DatabaseError> {
        let statement = self.id_update_statement.clone();
        self.call(
            &statement,
            &[
                &update.provider_identifier.as_deref(),
                &update.provider_authority.as_deref(),
                &update.old_identifier.as_deref(),
                &update.old_authority.as_deref(),
                &update.new_identifier.as_str(),
                &update.new_authority.as_str(),
            ],
        )
        .await
    }

    async fn execute_update_identifier(
        &mut self,
        update: &IdentifierUpdate,
    ) -> Result<(), DatabaseError> {
        let statement = self.update_identifier_statement.clone();
        self.call(
            &statement,
            &[
                &update.old_identifier.as_str(),
                &update.old_authority.as_str(),
                &update.new_identifier.as_str(),
            ],
        )
        .await
    }

    async fn execute_trrr(&mut self, request: &TrrrRequest) -> Result<(), DatabaseError> {
        match request {
            TrrrRequest::Association {
                lims_identifier,
                new_identifier,
                new_authority,
            } => {
                let statement = self.trrr_statements.0.clone();
                self.call(
                    &statement,
                    &[
                        &lims_identifier.as_str(),
                        &new_identifier.as_str(),
                        &new_authority.as_str(),
                    ],
                )
                .await
            }
            TrrrRequest::Merge {
                old_identifier,
                old_authority,
                new_identifier,
                new_authority,
            } => {
                let statement = self.trrr_statements.1.clone();
                self.call(
                    &statement,
                    &[
                        &old_identifier.as_str(),
                        &old_authority.as_str(),
                        &new_identifier.as_str(),
                        &new_authority.as_str(),
                    ],
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_codes_and_phrases() {
        // §7.3 (upstream test_execute_stored_procedure_transient_error and
        // _deadlock_error).
        for code in [1205, 233, 10054, 10060] {
            assert_eq!(classify(Some(code), "x"), FailureKind::Transient, "{code}");
        }
        for message in [
            "Query timeout expired",
            "Transaction was DEADLOCKED",
            "Communication link failure",
        ] {
            assert_eq!(classify(None, message), FailureKind::Transient, "{message}");
        }
    }

    #[test]
    fn everything_else_the_server_says_is_fatal() {
        // test_execute_stored_procedure_fatal_error: 2812.
        assert_eq!(
            classify(
                Some(2812),
                "Could not find stored procedure 'dbo.prQueueMessage'"
            ),
            FailureKind::Fatal
        );
        assert_eq!(
            classify(Some(18456), "Login failed for user"),
            FailureKind::Fatal
        );
    }

    #[test]
    fn an_error_is_sanitised_on_construction() {
        // §7.4 (test_execute_stored_procedure_redacts_patient_data_on_conversion_error).
        let error = DatabaseError::new(
            FailureKind::Fatal,
            245,
            "Conversion failed when converting the varchar value '9876543210' to data type int.",
        );
        assert!(!error.message.contains("9876543210"));
        assert!(!error.to_string().contains("9876543210"));
        assert!(!error.description().contains("9876543210"));
        assert!(
            error
                .message
                .contains("converting the varchar value '***' to data type int")
        );
    }

    #[test]
    fn io_failures_are_transient() {
        let error = from_tiberius(&tiberius::error::Error::Io {
            kind: tiberius::error::IoErrorKind::ConnectionRefused,
            message: "refused".into(),
        });
        assert_eq!(error.kind, FailureKind::Transient);
    }

    #[test]
    fn the_runner_renders_no_credential() {
        let config = crate::AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "hunter2"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap();
        let rendered = format!("{:?}", SqlRunner::new(config.db));
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }

    #[test]
    fn the_id_update_statement_has_six_positional_parameters() {
        let config = crate::AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_PROCEDURE_TYPE", "queue_id_update"),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "p"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap();
        assert_eq!(
            SqlRunner::new(config.db).id_update_statement,
            "DECLARE @rc INT; EXEC @rc = dbo.prQueueIdUpdate @P1, @P2, @P3, @P4, @P5, @P6; SELECT @rc;"
        );
    }

    #[test]
    fn the_update_identifier_statement_has_three_positional_parameters() {
        let config = crate::AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_PROCEDURE_TYPE", "update_identifier"),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "p"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap();
        assert_eq!(config.db.stored_procedure, "dbo.spUpdateIdentifier");
        assert_eq!(
            SqlRunner::new(config.db).update_identifier_statement,
            "DECLARE @rc INT; EXEC @rc = dbo.spUpdateIdentifier @P1, @P2, @P3; SELECT @rc;"
        );
    }

    #[test]
    fn the_trrr_statements_have_three_and_four_positional_parameters() {
        let config = crate::AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_PROCEDURE_TYPE", "trrr"),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "p"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap();
        let (association, merge) = SqlRunner::new(config.db).trrr_statements;
        assert_eq!(
            association,
            "DECLARE @rc INT; EXEC @rc = dbo.TRRRIDUpdate @P1, @P2, @P3; SELECT @rc;"
        );
        assert!(
            merge.ends_with("@P1, @P2, @P3, @P4; SELECT @rc;"),
            "{merge}"
        );
    }

    #[test]
    fn the_statement_is_positional() {
        let config = crate::AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "p"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap();
        assert_eq!(
            SqlRunner::new(config.db).statement,
            "DECLARE @rc INT; EXEC @rc = dbo.prQueueMessage @P1, @P2; SELECT @rc;"
        );
    }
}
