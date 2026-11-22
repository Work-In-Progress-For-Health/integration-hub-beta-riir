//! The environment.
//!
//! Implements `spec/10-senders.md` §7.6. Upstream: `db_sender/app_config.py`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ihb_config::{ConfigErrors, Reader, Secret};

use crate::trrr::Mapping;

/// What the service does with a message (`§7.6`, `§7.8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureType {
    /// `dbo.prQueueMessage(@Message, @IsPriority)`: the whole message goes
    /// to the procedure. The default.
    QueueMessage,
    /// `dbo.prQueueIdUpdate`: identifiers are read from an `A31` or `A40`
    /// and passed as six parameters (`§7.8`).
    QueueIdUpdate,
    /// `dbo.spUpdateIdentifier` (WCRS): the old identifier, its authority and
    /// the surviving identifier are read from an `A40` and passed as three
    /// strings (`§7.9`).
    UpdateIdentifier,
    /// A TRRR procedure (`dbo.TRRRIDUpdate` or `dbo.TRRRIDMerge`), chosen by
    /// `TRRR_MAPPING_TYPE` (`§7.10`).
    Trrr(Mapping),
}

impl ProcedureType {
    /// The database this type writes to unless `DB_NAME` says otherwise.
    #[must_use]
    pub const fn default_database(self) -> &'static str {
        match self {
            Self::Trrr(_) => "Clinical_Results",
            _ => "DRS",
        }
    }

    /// `(WORKFLOW_ID, MICROSERVICE_ID, PEER_SERVICE)` when `DB_PROCEDURE_TYPE`
    /// is `trrr`, as upstream's `db_sender_trrr`; the WRRS names otherwise.
    #[must_use]
    pub const fn identity_defaults(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Trrr(Mapping::NhsIdUpdate) => ("lims-to-trrr", "db_sender_trrr", "TRRR"),
            Self::Trrr(Mapping::Merge) => ("lims-to-trrr", "db_sender_trrr_merge", "TRRR"),
            _ => ("risp-to-wrrs", "wrrs_stmsg_dbsender", "WRRS"),
        }
    }

    /// The default procedure for this type.
    #[must_use]
    pub const fn default_procedure(self) -> &'static str {
        match self {
            Self::QueueMessage => "dbo.prQueueMessage",
            Self::QueueIdUpdate => "dbo.prQueueIdUpdate",
            Self::UpdateIdentifier => "dbo.spUpdateIdentifier",
            Self::Trrr(Mapping::NhsIdUpdate) => "dbo.TRRRIDUpdate",
            Self::Trrr(Mapping::Merge) => "dbo.TRRRIDMerge",
        }
    }

    /// The value of `DB_PROCEDURE_TYPE`, also used as a metric attribute.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueueMessage => "queue_message",
            Self::QueueIdUpdate => "queue_id_update",
            Self::UpdateIdentifier => "update_identifier",
            Self::Trrr(_) => "trrr",
        }
    }
}

/// How the service authenticates to SQL Server (`§7.6`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbAuth {
    /// `DB_USERNAME` and `DB_PASSWORD`, or the `sa` / `MSSQL_SA_PASSWORD`
    /// pair the local SQL image uses.
    Password {
        /// The login.
        username: String,
        /// The password; never printed (`spec/12-message-store.md` §5.7).
        password: Secret,
    },
    /// Neither set: an AAD token. Not yet implemented, so
    /// [`AppConfig::from_pairs`] refuses it (`§7.6`).
    ManagedIdentity {
        /// `MANAGED_IDENTITY_CLIENT_ID`.
        client_id: Option<String>,
    },
}

/// The database half of the configuration.
#[derive(Debug, Clone)]
pub struct DbConfig {
    /// `DB_SERVER`, default `localhost`. A host name or IP, without the port.
    pub server: String,
    /// `DB_PORT`, default 1433. Separate from [`Self::server`] because the
    /// upstream `DB_*` contract is, and TDS takes them separately.
    pub port: u16,
    /// `DB_NAME`, default `DRS`.
    pub database: String,
    /// `DB_STORED_PROCEDURE`, default `dbo.prQueueMessage`, validated by
    /// [`validate_procedure`] because it is spliced into `EXEC` text.
    pub stored_procedure: String,
    /// The login.
    pub auth: DbAuth,
    /// `DB_ENCRYPT`, default `true`. `Yes` makes TLS mandatory: a server
    /// that will not encrypt is a connection failure, not a plaintext
    /// connection (`spec/10-senders.md` §7.6, `spec/12-message-store.md` §5.8).
    pub encrypt: bool,
    /// `DB_TRUST_SERVER_CERTIFICATE`, default `false`. Strictly `Yes`/`No`.
    pub trust_server_certificate: bool,
    /// `DB_CONNECTION_TIMEOUT`, in seconds, default 30. Bounds TCP connect
    /// plus login.
    pub connection_timeout_secs: u64,
    /// `DB_COMMAND_TIMEOUT`, in seconds, default 60. Bounds one procedure
    /// call; an expiry is classed transient (`spec/10-senders.md` §7.3).
    pub command_timeout_secs: u64,
}

/// Everything the service reads from the environment.
///
/// # Examples
///
/// ```
/// use db_sender::AppConfig;
///
/// let config = AppConfig::from_pairs([
///     ("INGRESS_TOPIC_NAME", "t"),
///     ("INGRESS_SUBSCRIPTION_NAME", "s"),
///     ("DB_USERNAME", "sa"),
///     ("DB_PASSWORD", "pw"),
///     ("DB_ENCRYPT", "No"),
/// ])?;
/// assert_eq!(config.db.database, "DRS");
/// assert_eq!(config.db.stored_procedure, "dbo.prQueueMessage");
/// assert_eq!(config.max_batch_size, 10);
/// # Ok::<(), ihb_config::ConfigErrors>(())
/// ```
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// `SERVICE_BUS_CONNECTION_STRING`.
    pub connection_string: Option<Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    pub namespace: Option<String>,
    /// `INGRESS_TOPIC_NAME`. Required.
    pub ingress_topic: String,
    /// `INGRESS_SUBSCRIPTION_NAME`. Required.
    pub ingress_subscription: String,
    /// `INGRESS_SESSION_ID`. `None` means a plain (non-session) subscription
    /// receiver, which needs no lock renewal.
    pub ingress_session_id: Option<String>,
    /// The database.
    pub db: DbConfig,
    /// `DB_PROCEDURE_TYPE`, default `queue_message`.
    pub procedure_type: ProcedureType,
    /// `SOURCE_SYSTEM`, default `328`: the assigning authority an `A31` is
    /// taken to come from when neither the message's `SourceSystem` property
    /// nor `MSH-3.1` says (`§7.8`).
    pub source_system: String,
    /// `WORKFLOW_ID`.
    pub workflow_id: String,
    /// `MICROSERVICE_ID`.
    pub microservice_id: String,
    /// `HEALTH_BOARD`.
    pub health_board: String,
    /// `PEER_SERVICE`.
    pub peer_service: String,
    /// Where the health check binds.
    pub health_check: SocketAddr,
    /// `DEFAULT_IS_PRIORITY` is non-zero. Passed as the procedure's
    /// `@IsPriority`; zero and unset both mean `false`.
    pub default_is_priority: bool,
    /// `MAX_BATCH_SIZE`, in messages, default 10: how many are received per
    /// round. A transient failure abandons the rest of the batch, so a larger
    /// batch means more redelivery after one.
    pub max_batch_size: usize,
    /// `HL7_LOG_REDACTION_ENABLED`, default `true`: whether audit events
    /// redact the MSH text (`spec/04-observability.md` §2).
    pub redaction_enabled: bool,
}

/// Whether `name` may be spliced into `EXEC` (`§7.6`).
///
/// One to three `.`-separated parts of `[A-Za-z0-9_@#$]`. The name comes
/// from the environment, not from a message, but it is concatenated into SQL
/// (a procedure name cannot be a parameter), so a stray `;` is refused at
/// startup rather than trusted. Bracket-quoted names are unsupported: the
/// Python's ODBC `{CALL ..}` escape accepted them, and nothing deployed uses one.
///
/// # Errors
///
/// A message naming the problem, never echoing more than the name.
///
/// # Examples
///
/// ```
/// use db_sender::config::validate_procedure;
///
/// assert!(validate_procedure("dbo.prQueueMessage").is_ok());
/// assert!(validate_procedure("prQueueMessage").is_ok());
/// assert!(validate_procedure("dbo.p; DROP TABLE x").is_err());
/// assert!(validate_procedure("a.b.c.d").is_err());
/// assert!(validate_procedure("").is_err());
/// ```
pub fn validate_procedure(name: &str) -> Result<(), String> {
    let parts: Vec<&str> = name.split('.').collect();
    let part_ok = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '#' | '$'))
    };
    if (1..=3).contains(&parts.len()) && parts.iter().all(|p| part_ok(p)) {
        Ok(())
    } else {
        Err(format!(
            "DB_STORED_PROCEDURE '{name}' must be one to three dot-separated identifiers \
             of letters, digits and _@#$"
        ))
    }
}

impl AppConfig {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every problem found, together (`spec/02-configuration.md` §1.4).
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

    /// The shared body of the constructors.
    fn read(mut reader: Reader) -> Result<Self, ConfigErrors> {
        let ingress_topic = reader.required_string("INGRESS_TOPIC_NAME");
        let ingress_subscription = reader.required_string("INGRESS_SUBSCRIPTION_NAME");

        let procedure_type = read_procedure_type(&mut reader);
        let stored_procedure =
            reader.string_or("DB_STORED_PROCEDURE", procedure_type.default_procedure());
        if let Err(reason) = validate_procedure(&stored_procedure) {
            reader.conflict(reason);
        }

        // The Python falls back to MSSQL_SA_PASSWORD, with user `sa`, so the
        // local SQL image's own variable works unchanged.
        let sa_password = reader.optional_secret("MSSQL_SA_PASSWORD");
        let username = reader
            .optional_string("DB_USERNAME")
            .or_else(|| sa_password.as_ref().map(|_| "sa".to_string()));
        let password = reader.optional_secret("DB_PASSWORD").or(sa_password);
        let auth = match (username, password) {
            (Some(username), Some(password)) => DbAuth::Password { username, password },
            (None, None) => {
                // Upstream used `Authentication=ActiveDirectoryMsi`; this is
                // the same thing through the identity endpoint
                // (`spec/12-message-store.md` §5.4.1).
                DbAuth::ManagedIdentity {
                    client_id: reader.optional_string("MANAGED_IDENTITY_CLIENT_ID"),
                }
            }
            // Upstream silently fell back to managed identity here, which
            // looks like a network failure when a variable name is typed
            // wrongly; `ihb-sql` makes it a startup failure and so does this.
            _ => {
                reader.conflict("DB_USERNAME and DB_PASSWORD must be set together");
                DbAuth::ManagedIdentity { client_id: None }
            }
        };

        let encrypt = yes_or_no(&mut reader, "DB_ENCRYPT", true);
        let db = DbConfig {
            server: reader.string_or("DB_SERVER", "localhost"),
            port: reader.optional_positive_integer("DB_PORT").unwrap_or(1433),
            database: reader.string_or("DB_NAME", procedure_type.default_database()),
            stored_procedure,
            auth,
            encrypt,
            trust_server_certificate: yes_or_no(&mut reader, "DB_TRUST_SERVER_CERTIFICATE", false),
            connection_timeout_secs: reader
                .optional_positive_integer("DB_CONNECTION_TIMEOUT")
                .unwrap_or(30),
            command_timeout_secs: reader
                .optional_positive_integer("DB_COMMAND_TIMEOUT")
                .unwrap_or(60),
        };

        let identity = procedure_type.identity_defaults();
        let health_host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
        let health_port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");
        // `or 0` upstream: zero and unset both mean the default.
        let priority: u32 = reader.integer_or("DEFAULT_IS_PRIORITY", 0);

        let config = Self {
            connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
            namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
            ingress_topic,
            ingress_subscription,
            ingress_session_id: reader.optional_string("INGRESS_SESSION_ID"),
            db,
            procedure_type,
            source_system: reader.string_or("SOURCE_SYSTEM", "328"),
            workflow_id: reader.string_or("WORKFLOW_ID", identity.0),
            microservice_id: reader.string_or("MICROSERVICE_ID", identity.1),
            health_board: reader.string_or("HEALTH_BOARD", "SHARED"),
            peer_service: reader.string_or("PEER_SERVICE", identity.2),
            health_check: SocketAddr::new(
                health_host
                    .parse::<IpAddr>()
                    .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                health_port.unwrap_or(ihb_health::DEFAULT_PORT),
            ),
            default_is_priority: priority != 0,
            // Fails at startup on garbage rather than falling back, as
            // `spec/02-configuration.md` §5.1 does for the transformers.
            max_batch_size: reader.integer_or("MAX_BATCH_SIZE", 10),
            redaction_enabled: reader.lenient_bool_or("HL7_LOG_REDACTION_ENABLED", true),
        };

        reader.finish_with(config)
    }
}

/// `DB_PROCEDURE_TYPE`, case-insensitive; absent or blank is `queue_message`.
///
/// Upstream treats **anything** that is not a known type as
/// `queue_message`, so a typo silently runs the wrong procedure with the
/// wrong parameters and fails at the first message. Here it is a startup
/// failure (`spec/10-senders.md` §7.8).
fn read_procedure_type(reader: &mut Reader) -> ProcedureType {
    let Some(raw) = reader.raw("DB_PROCEDURE_TYPE").map(str::to_string) else {
        return ProcedureType::QueueMessage;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "queue_message" => ProcedureType::QueueMessage,
        "queue_id_update" => ProcedureType::QueueIdUpdate,
        "update_identifier" => ProcedureType::UpdateIdentifier,
        "trrr" => ProcedureType::Trrr(read_trrr_mapping(reader)),
        _ => {
            reader.invalid(
                "DB_PROCEDURE_TYPE",
                "queue_message, queue_id_update, update_identifier or trrr",
                raw,
            );
            ProcedureType::QueueMessage
        }
    }
}

/// `TRRR_MAPPING_TYPE`: `NHS_ID_UPDATE` (the default) or `MERGE`,
/// case-insensitive. Anything else is a startup failure, as upstream's
/// `ValueError` (`spec/10-senders.md` §7.10).
fn read_trrr_mapping(reader: &mut Reader) -> Mapping {
    let Some(raw) = reader.raw("TRRR_MAPPING_TYPE").map(str::to_string) else {
        return Mapping::NhsIdUpdate;
    };
    match raw.trim().to_ascii_uppercase().as_str() {
        "" | "NHS_ID_UPDATE" => Mapping::NhsIdUpdate,
        "MERGE" => Mapping::Merge,
        _ => {
            reader.invalid("TRRR_MAPPING_TYPE", "NHS_ID_UPDATE or MERGE", raw);
            Mapping::NhsIdUpdate
        }
    }
}

/// A `Yes`/`No` variable, strictly (`spec/12-message-store.md` §5.5).
///
/// Not the lenient boolean of `spec/02-configuration.md` §1.5: a typo like
/// `Ues` must not silently enable certificate trust.
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

    fn valid() -> Vec<(&'static str, &'static str)> {
        vec![
            ("INGRESS_TOPIC_NAME", "test-topic"),
            ("INGRESS_SUBSCRIPTION_NAME", "test-sub"),
            ("SERVICE_BUS_CONNECTION_STRING", "Endpoint=sb://localhost;"),
            ("DB_SERVER", "sqledge"),
            ("DB_PORT", "1433"),
            ("DB_NAME", "DRS"),
            ("DB_STORED_PROCEDURE", "dbo.prQueueMessage"),
            ("DB_USERNAME", "sa"),
            ("DB_PASSWORD", "Password123!"),
            ("DB_ENCRYPT", "No"),
        ]
    }

    fn with(extra: &[(&'static str, &'static str)]) -> Result<AppConfig, ConfigErrors> {
        let mut pairs = valid();
        pairs.extend_from_slice(extra);
        AppConfig::from_pairs(pairs)
    }

    #[test]
    fn the_procedure_type_picks_the_default_procedure() {
        // spec/10-senders.md §7.8 (upstream test_app_config).
        let message = with(&[]).unwrap();
        assert_eq!(message.procedure_type, ProcedureType::QueueMessage);
        assert_eq!(message.db.stored_procedure, "dbo.prQueueMessage");
        assert_eq!(message.source_system, "328");

        // `valid()` names a procedure explicitly, which would win; leave it out.
        let mut pairs: Vec<_> = valid()
            .into_iter()
            .filter(|(name, _)| *name != "DB_STORED_PROCEDURE")
            .collect();
        pairs.extend([
            ("DB_PROCEDURE_TYPE", " Queue_Id_Update "),
            ("SOURCE_SYSTEM", "349"),
        ]);
        let update = AppConfig::from_pairs(pairs).unwrap();
        assert_eq!(update.procedure_type, ProcedureType::QueueIdUpdate);
        assert_eq!(update.db.stored_procedure, "dbo.prQueueIdUpdate");
        assert_eq!(update.source_system, "349");

        // An explicit procedure still wins.
        let named = with(&[
            ("DB_PROCEDURE_TYPE", "queue_id_update"),
            ("DB_STORED_PROCEDURE", "dbo.other"),
        ])
        .unwrap();
        assert_eq!(named.db.stored_procedure, "dbo.other");
    }

    #[test]
    fn update_identifier_defaults_to_the_wcrs_procedure() {
        let mut pairs: Vec<_> = valid()
            .into_iter()
            .filter(|(name, _)| *name != "DB_STORED_PROCEDURE")
            .collect();
        pairs.push(("DB_PROCEDURE_TYPE", " Update_Identifier "));
        let config = AppConfig::from_pairs(pairs).unwrap();
        assert_eq!(config.procedure_type, ProcedureType::UpdateIdentifier);
        assert_eq!(config.db.stored_procedure, "dbo.spUpdateIdentifier");
    }

    #[test]
    fn trrr_reads_its_mapping_and_takes_its_own_defaults() {
        let base = |extra: &[(&'static str, &'static str)]| {
            let mut pairs: Vec<_> = valid()
                .into_iter()
                .filter(|(name, _)| !matches!(*name, "DB_STORED_PROCEDURE" | "DB_NAME"))
                .collect();
            pairs.push(("DB_PROCEDURE_TYPE", "trrr"));
            pairs.extend_from_slice(extra);
            AppConfig::from_pairs(pairs)
        };

        let update = base(&[]).unwrap();
        assert_eq!(
            update.procedure_type,
            ProcedureType::Trrr(Mapping::NhsIdUpdate)
        );
        assert_eq!(update.db.stored_procedure, "dbo.TRRRIDUpdate");
        assert_eq!(update.db.database, "Clinical_Results");
        assert_eq!(update.workflow_id, "lims-to-trrr");
        assert_eq!(update.microservice_id, "db_sender_trrr");
        assert_eq!(update.peer_service, "TRRR");

        let merge = base(&[("TRRR_MAPPING_TYPE", " merge ")]).unwrap();
        assert_eq!(merge.procedure_type, ProcedureType::Trrr(Mapping::Merge));
        assert_eq!(merge.db.stored_procedure, "dbo.TRRRIDMerge");
        assert_eq!(merge.microservice_id, "db_sender_trrr_merge");

        let named = base(&[("MICROSERVICE_ID", "mine"), ("DB_NAME", "Other")]).unwrap();
        assert_eq!(
            (named.microservice_id.as_str(), named.db.database.as_str()),
            ("mine", "Other")
        );

        let error = base(&[("TRRR_MAPPING_TYPE", "SPLIT")]).unwrap_err();
        assert!(error.to_string().contains("TRRR_MAPPING_TYPE"), "{error}");
    }

    #[test]
    fn an_unknown_procedure_type_is_a_startup_failure_not_the_default() {
        let error = with(&[("DB_PROCEDURE_TYPE", "queue_id_updat")]).unwrap_err();
        assert!(error.to_string().contains("DB_PROCEDURE_TYPE"), "{error}");
        assert_eq!(
            with(&[("DB_PROCEDURE_TYPE", "  ")]).unwrap().procedure_type,
            ProcedureType::QueueMessage
        );
    }

    #[test]
    fn a_complete_environment_reads() {
        // §7.7 (upstream test_read_env_config_success).
        let config = with(&[]).unwrap();
        assert_eq!(config.ingress_topic, "test-topic");
        assert_eq!(config.db.server, "sqledge");
        assert_eq!(config.db.port, 1433);
        assert_eq!(config.db.stored_procedure, "dbo.prQueueMessage");
        assert!(!config.default_is_priority);
        assert_eq!(config.max_batch_size, 10);
        assert_eq!(config.workflow_id, "risp-to-wrrs");
        assert_eq!(config.microservice_id, "wrrs_stmsg_dbsender");
    }

    #[test]
    fn defaults() {
        // §7.6 (test_read_env_config_defaults), with the password and
        // encryption the Rust build additionally needs.
        let config = AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_USERNAME", "u"),
            ("DB_PASSWORD", "p"),
            ("DB_ENCRYPT", "No"),
        ])
        .unwrap();
        assert_eq!(config.db.server, "localhost");
        assert_eq!(config.db.port, 1433);
        assert_eq!(config.db.database, "DRS");
        assert!(!config.db.trust_server_certificate);
        assert_eq!(config.db.connection_timeout_secs, 30);
        assert_eq!(config.db.command_timeout_secs, 60);
        assert_eq!(config.health_board, "SHARED");
        assert_eq!(config.peer_service, "WRRS");
    }

    #[test]
    fn mssql_sa_password_is_the_fallback() {
        // §7.6 (test_read_env_config_mssql_sa_password_fallback).
        let config = AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("DB_ENCRYPT", "No"),
            ("MSSQL_SA_PASSWORD", "SaPassword123!"),
        ])
        .unwrap();
        assert_eq!(
            config.db.auth,
            DbAuth::Password {
                username: "sa".into(),
                password: Secret::new("SaPassword123!")
            }
        );
    }

    #[test]
    fn missing_required_names_the_variable() {
        // test_read_env_config_missing_required.
        let errors = AppConfig::from_pairs(Vec::<(String, String)>::new()).unwrap_err();
        assert!(
            errors.to_string().contains("INGRESS_TOPIC_NAME"),
            "{errors}"
        );
        assert!(
            errors.to_string().contains("INGRESS_SUBSCRIPTION_NAME"),
            "{errors}"
        );
    }

    #[test]
    fn an_invalid_integer_names_the_variable() {
        // test_read_env_config_invalid_int.
        let errors = with(&[("DB_PORT", "invalid-port")]).unwrap_err();
        assert!(errors.to_string().contains("DB_PORT"), "{errors}");
    }

    #[test]
    fn encryption_defaults_on_and_trust_defaults_off() {
        // §7.6 and `spec/12` §5.8: TLS is the default and is honoured.
        let mut pairs = valid();
        pairs.retain(|(k, _)| *k != "DB_ENCRYPT");
        let config = AppConfig::from_pairs(pairs).unwrap();
        assert!(config.db.encrypt);
        assert!(!config.db.trust_server_certificate);
    }

    #[test]
    fn no_credentials_selects_managed_identity() {
        let config = AppConfig::from_pairs([
            ("INGRESS_TOPIC_NAME", "t"),
            ("INGRESS_SUBSCRIPTION_NAME", "s"),
            ("MANAGED_IDENTITY_CLIENT_ID", "abc-123"),
        ])
        .unwrap();
        assert!(
            matches!(config.db.auth, DbAuth::ManagedIdentity { client_id: Some(ref id) } if id == "abc-123"),
            "{:?}",
            config.db.auth
        );
    }

    #[test]
    fn one_credential_without_the_other_is_refused() {
        let errors = with(&[("DB_PASSWORD", "")]).unwrap_err();
        assert!(errors.to_string().contains("together"), "{errors}");
    }

    #[test]
    fn a_hostile_procedure_name_is_refused() {
        // §7.6: spliced into EXEC.
        let errors = with(&[("DB_STORED_PROCEDURE", "dbo.p; DROP TABLE x")]).unwrap_err();
        assert!(
            errors.to_string().contains("DB_STORED_PROCEDURE"),
            "{errors}"
        );
    }

    #[test]
    fn priority_is_any_non_zero() {
        assert!(
            with(&[("DEFAULT_IS_PRIORITY", "1")])
                .unwrap()
                .default_is_priority
        );
        assert!(
            !with(&[("DEFAULT_IS_PRIORITY", "0")])
                .unwrap()
                .default_is_priority
        );
    }

    #[test]
    fn the_password_never_renders() {
        // spec/12-message-store.md §5.7.
        let rendered = format!("{:?}", with(&[]).unwrap());
        assert!(!rendered.contains("Password123!"), "{rendered}");
    }
}
