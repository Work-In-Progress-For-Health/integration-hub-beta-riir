//! The connection, and the transaction discipline around it.
//!
//! Implements `spec/12-message-store.md` §5.1, §5.2 and §5.3.

use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::config::{Auth, SqlConfig};
use crate::error::SqlError;

/// A `tiberius` client over a Tokio socket. The `Compat` adapter is needed
/// because `tiberius` speaks `futures::io`, not `tokio::io`.
type Connected = Client<Compat<TcpStream>>;

/// A lazily-opened, long-lived connection.
///
/// # The two rules this type exists to enforce
///
/// **Lazy** (§5.1): nothing is opened until the first statement. A service
/// whose database is briefly unreachable still starts, binds its health
/// check, and reports itself — rather than failing at boot and being
/// restarted in a loop by the orchestrator, which is how a five-minute
/// database blip becomes a thirty-minute outage.
///
/// **Discarded on error** (§5.2): a connection that has seen a database
/// error may be in an unknown transaction state, so it is dropped rather
/// than reused. The next call reconnects transparently. That also refreshes
/// an AAD token (§5.4.2) with no expiry tracking anywhere.
pub struct SqlConnection {
    /// How to reach the server.
    config: SqlConfig,
    /// The managed identity, when that is the mode. Held for the life of the
    /// connection object so its token cache survives a reconnect (§5.4.2).
    identity: Option<ihb_identity::ManagedIdentity>,
    /// The connection, once opened.
    client: Option<Connected>,
}

impl std::fmt::Debug for SqlConnection {
    /// Deliberately hand-written.
    ///
    /// §5.7: a derived `Debug` would print [`SqlConfig`], and while that
    /// redacts its own password, a future field might not. This prints only
    /// what is safe by construction.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlConnection")
            .field("server", &self.config.server)
            .field("database", &self.config.database)
            .field("auth", &self.config.auth.describe())
            .field("open", &self.client.is_some())
            // The identity (and its cached token) is left out on purpose.
            .finish_non_exhaustive()
    }
}

impl SqlConnection {
    /// A connection that will open on first use.
    #[must_use]
    pub fn new(config: SqlConfig) -> Self {
        let identity = match &config.auth {
            Auth::ManagedIdentity { client_id } => {
                Some(ihb_identity::ManagedIdentity::new(client_id.clone()))
            }
            Auth::Password { .. } => None,
        };
        Self {
            config,
            identity,
            client: None,
        }
    }

    /// Where this connects, for a log line.
    #[must_use]
    pub fn server(&self) -> &str {
        &self.config.server
    }

    /// Whether a connection is currently held.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.client.is_some()
    }

    /// Close and discard, so the next call reconnects (§5.2).
    ///
    /// Public because a caller that catches a database error at a higher
    /// level — a failed batch, say — needs to be able to say so.
    pub fn discard(&mut self) {
        if self.client.take().is_some() {
            tracing::info!(
                server = %self.config.server,
                "discarding the database connection; the next call will reconnect"
            );
        }
    }

    /// Take the connection out, opening one if needed.
    ///
    /// Ownership rather than a borrow, because that is what §5.2 actually
    /// describes: the caller holds the connection for the duration of its
    /// work, and either hands it back or drops it. A connection that has
    /// seen an error is never put back.
    ///
    /// # Errors
    ///
    /// [`SqlError::Connect`] or [`SqlError::Token`].
    pub async fn take(&mut self) -> Result<Connected, SqlError> {
        match self.client.take() {
            Some(client) => Ok(client),
            None => self.open().await,
        }
    }

    /// Hand a healthy connection back for the next call to reuse (§5.1).
    pub fn restore(&mut self, client: Connected) {
        self.client = Some(client);
    }

    /// Open a connection.
    async fn open(&self) -> Result<Connected, SqlError> {
        let mut config = Config::new();
        config.host(&self.config.server);
        config.port(1433);
        config.database(&self.config.database);

        // §5.5 and §5.8. `Required` means TLS or no connection: the server
        // refusing to encrypt is a failure, never a downgrade to plaintext,
        // because this is the connection that carries full patient records.
        // Trusting the server certificate without checking it is a separate,
        // explicit opt-in (`SQL_TRUST_SERVER_CERTIFICATE`), off by default.
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

        match &self.config.auth {
            Auth::Password { username, password } => {
                config.authentication(AuthMethod::sql_server(username, password.expose()));
            }
            Auth::ManagedIdentity { .. } => {
                // §5.4.1 — an AAD token for `https://database.windows.net/`
                // from the identity endpoint. This replaces the Python's
                // `Authentication=ActiveDirectoryMsi` ODBC parameter. §5.4.2:
                // the identity caches it and refreshes it before expiry, so a
                // reconnect per §5.2 always presents a live one.
                let token = match &self.identity {
                    Some(identity) => identity.sql_token().await,
                    None => unreachable!("a managed-identity config always builds an identity"),
                }
                .map_err(|error| SqlError::Token(error.to_string()))?;
                config.authentication(AuthMethod::aad_token(token));
            }
        }

        tracing::info!(
            server = %self.config.server,
            database = %self.config.database,
            auth = self.config.auth.describe(),
            encrypt = self.config.encrypt,
            "opening a database connection"
        );

        let address = format!("{}:1433", self.config.server);
        let stream = TcpStream::connect(&address)
            .await
            .map_err(|error| SqlError::Connect {
                server: self.config.server.clone(),
                reason: error.to_string(),
            })?;
        stream.set_nodelay(true).ok();

        // Boxed: with a TLS backend the handshake future is ~17 KB, and it
        // would otherwise be inlined into every caller's future.
        Box::pin(Client::connect(config, stream.compat_write()))
            .await
            .map_err(|error| SqlError::Connect {
                server: self.config.server.clone(),
                // `tiberius`'s errors describe the protocol failure, not the
                // configuration, so this carries no credential (§5.7).
                reason: error.to_string(),
            })
    }

    /// Run `work` inside an explicit transaction (§5.3).
    ///
    /// Autocommit off, commit on success, rollback on failure — and the
    /// rollback is the point. Without it, an abandoned batch could be
    /// redelivered and reinserted, and `monitoring.Message` would hold the
    /// same message twice with no way to tell which row is real.
    ///
    /// On any failure the connection is discarded (§5.2). A rollback that
    /// itself fails is logged at warn rather than replacing the original
    /// error, because the original is the one that explains what happened.
    ///
    /// # Errors
    ///
    /// Whatever `work` returned, or [`SqlError::Commit`].
    pub async fn transaction<F>(&mut self, work: F) -> Result<(), SqlError>
    where
        F: AsyncFnOnce(&mut Connected) -> Result<(), SqlError>,
    {
        let mut client = self.take().await?;

        if let Err(error) = statement(&mut client, "BEGIN TRANSACTION").await {
            // Not put back: it failed before any work began, so its state
            // is unknown.
            self.discard();
            return Err(SqlError::Statement(error));
        }

        let outcome = work(&mut client).await;

        match outcome {
            Ok(()) => match statement(&mut client, "COMMIT TRANSACTION").await {
                Ok(()) => {
                    // Healthy, and reused by the next batch (§5.1).
                    self.restore(client);
                    Ok(())
                }
                Err(error) => {
                    // Dropped rather than restored (§5.2).
                    self.discard();
                    Err(SqlError::Commit(error))
                }
            },
            Err(original) => {
                if let Err(rollback) = statement(&mut client, "ROLLBACK TRANSACTION").await {
                    // Logged, not returned: the original error is what
                    // explains the failure, and replacing it would hide the
                    // cause behind its consequence.
                    tracing::warn!(
                        %rollback,
                        "the rollback failed as well; reporting the original error"
                    );
                }
                self.discard();
                Err(original)
            }
        }
    }
}

/// Run one statement and consume its results.
///
/// `simple_query` hands back a stream that borrows the client, and the
/// borrow lasts as long as the stream does. Consuming it here releases the
/// client, which is what lets the caller hand a healthy connection back.
async fn statement(client: &mut Connected, sql: &str) -> Result<(), String> {
    client
        .simple_query(sql)
        .await
        .map_err(|error| error.to_string())?
        .into_results()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ihb_config::Secret;

    fn config() -> SqlConfig {
        SqlConfig {
            server: "sqlserver".to_string(),
            database: "monitoring".to_string(),
            auth: Auth::Password {
                username: "sa".to_string(),
                password: Secret::new("hunter2"),
            },
            encrypt: true,
            trust_server_certificate: false,
        }
    }

    #[test]
    fn a_new_connection_is_not_open() {
        // §5.1 — nothing is opened until the first statement, so a service
        // whose database is briefly unreachable still starts and reports
        // itself healthy rather than being restarted in a loop.
        let connection = SqlConnection::new(config());

        assert!(!connection.is_open());
        assert_eq!(connection.server(), "sqlserver");
    }

    #[test]
    fn discarding_a_closed_connection_is_a_no_op() {
        let mut connection = SqlConnection::new(config());

        connection.discard();
        assert!(!connection.is_open());
    }

    #[test]
    fn the_debug_rendering_carries_no_credential() {
        // §5.7 and §6.7 — hand-written for exactly this reason.
        let connection = SqlConnection::new(config());

        let rendered = format!("{connection:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("sqlserver"));
        assert!(rendered.contains("password"), "the mode, not the value");
    }

    #[tokio::test]
    async fn managed_identity_without_an_endpoint_names_the_missing_variable() {
        // §5.4.1. Off Azure there is no identity endpoint, and the failure
        // must say so at once rather than look like a firewall problem. Not
        // asserted when the variable is set, as it is on a Container App.
        if std::env::var("IDENTITY_ENDPOINT").is_ok() {
            return;
        }
        let mut connection = SqlConnection::new(SqlConfig {
            auth: Auth::ManagedIdentity {
                client_id: Some("abc-123".to_string()),
            },
            ..config()
        });

        let error = connection.take().await.unwrap_err();

        assert!(matches!(error, SqlError::Token(_)), "{error:?}");
        assert!(error.to_string().contains("IDENTITY_ENDPOINT"), "{error}");
    }
}
