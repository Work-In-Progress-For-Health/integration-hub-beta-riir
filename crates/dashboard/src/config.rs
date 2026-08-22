//! Everything the dashboard reads from its environment.
//!
//! Implements `spec/02-configuration.md` §9.2 and `spec/15-dashboard.md`
//! §1.2.
//!
//! # Almost every field is optional, deliberately
//!
//! §9.2.1 and §15 §1.2: **the dashboard MUST start and serve every page
//! when no Azure configuration is present.** So there is exactly one
//! required setting here — none — and the presence of a value is what
//! selects a behaviour rather than what avoids a crash.
//!
//! The two places that behaviour *is* refused are both cases where
//! proceeding would be worse than stopping:
//!
//! - `COSMOS_DISABLE_SSL_VERIFY` with a non-local endpoint (§5.6.1). That
//!   combination disables certificate verification against a real service.
//! - A threshold that is not a number. A dashboard that read
//!   `QUEUE_CRITICAL_THRESHOLD=fifty` as its default of 50 would look
//!   right and be wrong.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ihb_config::{ConfigErrors, Reader, Secret};

/// `QUEUE_WARNING_THRESHOLD` (§9.2).
pub const DEFAULT_QUEUE_WARNING: u64 = 10;

/// `QUEUE_CRITICAL_THRESHOLD` (§9.2).
pub const DEFAULT_QUEUE_CRITICAL: u64 = 50;

/// `DLQ_WARNING_THRESHOLD` (§9.2).
pub const DEFAULT_DLQ_WARNING: u64 = 1;

/// `COSMOS_DATABASE` (§9.2).
///
/// **`integration-hub`**, which is `dashboard/config.py`'s default and not
/// the `integration-hub-dashboard` an earlier draft of the specification
/// carried. The default decides which Cosmos database alarm configuration
/// is read from, so the wrong one would make every configured rule vanish
/// at cutover — silently, because an empty database is not an error. See
/// `spec/02-configuration.md` §9.2.2.
pub const DEFAULT_COSMOS_DATABASE: &str = "integration-hub";

/// `COSMOS_CONTAINER` (§9.2).
pub const DEFAULT_COSMOS_CONTAINER: &str = "alarms";

/// How long `/api/status` is cached by default, in seconds (§2.2.1).
pub const DEFAULT_API_CACHE_TTL: u64 = 30;

/// How the dashboard authenticates to Azure (§4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AzureAuth {
    /// A service principal: all three of tenant, client and secret.
    ServicePrincipal {
        /// `AZURE_TENANT_ID`.
        tenant_id: String,
        /// `AZURE_CLIENT_ID`.
        client_id: String,
        /// `AZURE_CLIENT_SECRET`.
        client_secret: Secret,
    },
    /// The ambient credential — a managed identity in Azure, whatever the
    /// developer is signed in as locally.
    ///
    /// `AZURE_CLIENT_ID` MAY still be set, selecting a user-assigned
    /// identity.
    Ambient {
        /// `AZURE_CLIENT_ID`, when one was given.
        client_id: Option<String>,
    },
}

impl AzureAuth {
    /// Which mode this is, for a log line. Never the secret itself.
    #[must_use]
    pub const fn describe(&self) -> &'static str {
        match self {
            Self::ServicePrincipal { .. } => "service principal",
            Self::Ambient { .. } => "ambient credential",
        }
    }
}

/// How alarm configuration and state are persisted (§5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CosmosMode {
    /// `COSMOS_ENDPOINT` is blank: reads return empty, writes are no-ops.
    ///
    /// The mode §1.2 is about. Alarm pages render; nothing is stored.
    Disabled,
    /// `COSMOS_KEY` is set: key authentication, and the database and
    /// container are created if absent.
    Key {
        /// `COSMOS_ENDPOINT`.
        endpoint: String,
        /// `COSMOS_KEY`.
        key: Secret,
    },
    /// `COSMOS_ENDPOINT` without a key: AAD RBAC. The database and
    /// container MUST already exist — the dashboard's identity is not
    /// expected to hold control-plane rights.
    Rbac {
        /// `COSMOS_ENDPOINT`.
        endpoint: String,
    },
}

impl CosmosMode {
    /// The endpoint, when there is one.
    #[must_use]
    pub fn endpoint(&self) -> Option<&str> {
        match self {
            Self::Disabled => None,
            Self::Key { endpoint, .. } | Self::Rbac { endpoint } => Some(endpoint),
        }
    }

    /// Whether anything is persisted at all.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Which mode this is, for a log line.
    #[must_use]
    pub const fn describe(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Key { .. } => "key",
            Self::Rbac { .. } => "AAD RBAC",
        }
    }
}

/// The flow-health thresholds of §3.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    /// `QUEUE_WARNING_THRESHOLD`.
    pub queue_warning: u64,
    /// `QUEUE_CRITICAL_THRESHOLD`.
    pub queue_critical: u64,
    /// `DLQ_WARNING_THRESHOLD`.
    pub dlq_warning: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            queue_warning: DEFAULT_QUEUE_WARNING,
            queue_critical: DEFAULT_QUEUE_CRITICAL,
            dlq_warning: DEFAULT_DLQ_WARNING,
        }
    }
}

/// Where the Azure data comes from (§4).
///
/// Every field is optional. An absent one disables its panel, which renders
/// as unavailable rather than as zero (§2.2.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AzureTargets {
    /// `AZURE_SUBSCRIPTION_ID`.
    pub subscription_id: Option<String>,
    /// `AZURE_RESOURCE_GROUP`.
    pub resource_group: Option<String>,
    /// `AZURE_SERVICE_BUS_NAMESPACE`.
    pub service_bus_namespace: Option<String>,
    /// `AZURE_LOG_ANALYTICS_WORKSPACE_ID`.
    pub log_analytics_workspace_id: Option<String>,
    /// `AZURE_APP_INSIGHTS_RESOURCE_ID`, which narrows every query that
    /// honours it to one resource (§4.3.3).
    pub app_insights_resource_id: Option<String>,
}

impl AzureTargets {
    /// Whether Log Analytics can be queried at all.
    ///
    /// The gate §4.2 and §4.5 turn into empty results rather than errors.
    #[must_use]
    pub fn has_log_analytics(&self) -> bool {
        self.log_analytics_workspace_id.is_some()
    }

    /// Whether the Service Bus management API can be reached.
    #[must_use]
    pub fn has_service_bus(&self) -> bool {
        self.subscription_id.is_some()
            && self.resource_group.is_some()
            && self.service_bus_namespace.is_some()
    }
}

/// Everything the dashboard was configured with.
///
/// # Examples
///
/// The empty environment, which §1.2 makes the important one:
///
/// ```
/// use dashboard::config::{Config, CosmosMode};
///
/// let config = Config::from_pairs::<[(&str, &str); 0], _, _>([])
///     .expect("no Azure configuration is a valid configuration");
///
/// assert_eq!(config.cosmos, CosmosMode::Disabled);
/// assert!(!config.azure.has_log_analytics());
/// assert_eq!(config.thresholds.queue_critical, 50);
/// ```
///
/// A service principal is all three parts or none:
///
/// ```
/// use dashboard::config::{AzureAuth, Config};
///
/// let config = Config::from_pairs([
///     ("AZURE_TENANT_ID", "t"),
///     ("AZURE_CLIENT_ID", "c"),
///     ("AZURE_CLIENT_SECRET", "s"),
/// ])
/// .unwrap();
/// assert_eq!(config.auth.describe(), "service principal");
///
/// // Two of the three falls back to the ambient credential, carrying the
/// // client id -- which is what a user-assigned managed identity looks
/// // like, so it is not an error.
/// let partial = Config::from_pairs([("AZURE_CLIENT_ID", "c")]).unwrap();
/// assert_eq!(partial.auth.describe(), "ambient credential");
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the HTTP listener binds.
    pub listen: SocketAddr,
    /// How to authenticate to Azure (§4.1).
    pub auth: AzureAuth,
    /// What to query (§4).
    pub azure: AzureTargets,
    /// How alarms are persisted (§5.6).
    pub cosmos: CosmosMode,
    /// `COSMOS_DATABASE`.
    pub cosmos_database: String,
    /// `COSMOS_CONTAINER`.
    pub cosmos_container: String,
    /// `COSMOS_DISABLE_SSL_VERIFY` (§5.6.1).
    pub cosmos_disable_ssl_verify: bool,
    /// §3.2's thresholds.
    pub thresholds: Thresholds,
    /// `API_CACHE_TTL` — how long `/api/status` is cached (§2.2.1).
    pub api_cache_ttl: std::time::Duration,
    /// `DEFAULT_LANGUAGE` (§6).
    pub default_language: String,
    /// `FLASK_SECRET_KEY`, kept under its Python name so a deployment's
    /// existing secret keeps working (§9.2).
    ///
    /// `None` means **generate one at startup**, not "use a default".
    /// See `spec/02-configuration.md` §9.2.3: the Python's default is the
    /// literal `dev-secret-key-change-in-production`, which is in a public
    /// repository, so every deployment that forgot the variable is signing
    /// sessions with a key anyone can read.
    pub session_key: Option<Secret>,
    /// Whether message content is redacted before display (§1.3).
    ///
    /// On by default. The Python has no such option because it does no
    /// redaction at all.
    pub redact: bool,
}

impl Config {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every problem found, together — but see this module's documentation:
    /// missing Azure configuration is not a problem.
    pub fn from_env() -> Result<Self, ConfigErrors> {
        let mut reader = Reader::from_env();
        let config = Self::read(&mut reader)?;
        reader.finish()?;
        Ok(config)
    }

    /// Read from explicit pairs, for tests and examples.
    ///
    /// # Errors
    ///
    /// As [`Config::from_env`].
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

    /// Read from a reader the caller owns.
    ///
    /// # Errors
    ///
    /// The two refusals this module's documentation names.
    pub fn read(reader: &mut Reader) -> Result<Self, ConfigErrors> {
        let host = reader.string_or("HOST", "127.0.0.1");
        let port = reader.integer_or("PORT", 8000_u16);

        let tenant_id = reader.optional_string("AZURE_TENANT_ID");
        let client_id = reader.optional_string("AZURE_CLIENT_ID");
        let client_secret = reader.optional_secret("AZURE_CLIENT_SECRET");

        // §4.1 — "a service principal when configured, otherwise the
        // ambient credential". All three, or fall back: two of the three is
        // not an error, because `AZURE_CLIENT_ID` alone is exactly what a
        // user-assigned managed identity looks like.
        let auth = match (tenant_id, client_id, client_secret) {
            (Some(tenant_id), Some(client_id), Some(client_secret)) => {
                AzureAuth::ServicePrincipal {
                    tenant_id,
                    client_id,
                    client_secret,
                }
            }
            (_, client_id, _) => AzureAuth::Ambient { client_id },
        };

        let endpoint = reader.optional_string("COSMOS_ENDPOINT");
        let key = reader.optional_secret("COSMOS_KEY");

        // §5.6's three rows, in order.
        let cosmos = match (endpoint, key) {
            (None, _) => CosmosMode::Disabled,
            (Some(endpoint), Some(key)) => CosmosMode::Key { endpoint, key },
            (Some(endpoint), None) => CosmosMode::Rbac { endpoint },
        };

        let cosmos_disable_ssl_verify = reader.bool_or("COSMOS_DISABLE_SSL_VERIFY", false);

        // §5.6.1 — "MAY be true **only** for the local emulator's
        // self-signed certificate". Anywhere else it silently accepts any
        // certificate for a service holding alarm configuration.
        if cosmos_disable_ssl_verify {
            match cosmos.endpoint() {
                Some(endpoint) if !is_local(endpoint) => {
                    reader.conflict(format!(
                        "COSMOS_DISABLE_SSL_VERIFY is set with a non-local COSMOS_ENDPOINT \
                         ({endpoint}); certificate verification may only be disabled for the \
                         local emulator (spec/15-dashboard.md §5.6.1)"
                    ));
                }
                // No endpoint at all: nothing is reached, so nothing is
                // exposed. Not worth refusing a start over.
                _ => {}
            }
        }

        let config = Self {
            listen: SocketAddr::new(
                host.parse::<IpAddr>()
                    .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port,
            ),
            auth,
            azure: AzureTargets {
                subscription_id: reader.optional_string("AZURE_SUBSCRIPTION_ID"),
                resource_group: reader.optional_string("AZURE_RESOURCE_GROUP"),
                service_bus_namespace: reader.optional_string("AZURE_SERVICE_BUS_NAMESPACE"),
                log_analytics_workspace_id: reader
                    .optional_string("AZURE_LOG_ANALYTICS_WORKSPACE_ID"),
                app_insights_resource_id: reader.optional_string("AZURE_APP_INSIGHTS_RESOURCE_ID"),
            },
            cosmos,
            cosmos_database: reader.string_or("COSMOS_DATABASE", DEFAULT_COSMOS_DATABASE),
            cosmos_container: reader.string_or("COSMOS_CONTAINER", DEFAULT_COSMOS_CONTAINER),
            cosmos_disable_ssl_verify,
            thresholds: Thresholds {
                queue_warning: reader.integer_or("QUEUE_WARNING_THRESHOLD", DEFAULT_QUEUE_WARNING),
                queue_critical: reader
                    .integer_or("QUEUE_CRITICAL_THRESHOLD", DEFAULT_QUEUE_CRITICAL),
                dlq_warning: reader.integer_or("DLQ_WARNING_THRESHOLD", DEFAULT_DLQ_WARNING),
            },
            api_cache_ttl: std::time::Duration::from_secs(
                reader.integer_or("API_CACHE_TTL", DEFAULT_API_CACHE_TTL),
            ),
            default_language: reader.string_or("DEFAULT_LANGUAGE", "en"),
            session_key: reader.optional_secret("FLASK_SECRET_KEY"),
            // §1.3 — on by default, and the deviation from the Python is
            // that the Python has no setting because it never redacts.
            redact: reader.bool_or("DASHBOARD_REDACT_MESSAGES", true),
        };

        Ok(config)
    }
}

/// Whether an endpoint is the local Cosmos emulator (§5.6.1).
///
/// Host-based rather than a substring search: `https://cosmos.localhost.
/// example.com` contains "localhost" and is not local.
fn is_local(endpoint: &str) -> bool {
    let without_scheme = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest);
    let host = without_scheme
        .split(['/', ':'])
        .next()
        .unwrap_or(without_scheme);

    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
        // The Compose service name, which is what the emulator is called
        // from inside the local stack.
        || host == "cosmos"
        || host == "cosmos-emulator"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(pairs: &[(&str, &str)]) -> Result<Config, ConfigErrors> {
        Config::from_pairs(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
        )
    }

    #[test]
    fn the_empty_environment_is_a_valid_configuration() {
        // spec/15-dashboard.md §1.2 and spec/02-configuration.md §9.2.1 —
        // the first test written and the last one allowed to fail. A
        // dashboard that will not start without Azure cannot be developed
        // on a laptop or tested in CI.
        let config = with(&[]).expect("no configuration is a configuration");

        assert_eq!(config.cosmos, CosmosMode::Disabled);
        assert_eq!(config.auth, AzureAuth::Ambient { client_id: None });
        assert!(!config.azure.has_log_analytics());
        assert!(!config.azure.has_service_bus());
        assert_eq!(config.thresholds, Thresholds::default());
        assert!(config.redact, "redaction is on unless turned off");
    }

    #[test]
    fn a_service_principal_needs_all_three_parts() {
        // §4.1.
        let full = with(&[
            ("AZURE_TENANT_ID", "t"),
            ("AZURE_CLIENT_ID", "c"),
            ("AZURE_CLIENT_SECRET", "s"),
        ])
        .unwrap();
        assert!(matches!(full.auth, AzureAuth::ServicePrincipal { .. }));
    }

    #[test]
    fn a_client_id_alone_is_a_user_assigned_identity_not_an_error() {
        // Two of three is NOT a startup failure here, unlike
        // `spec/12-message-store.md` §5.4's SQL credentials. The difference
        // is real: `AZURE_CLIENT_ID` on its own is exactly how a
        // user-assigned managed identity is selected, so refusing it would
        // refuse a correct deployment.
        let config = with(&[("AZURE_CLIENT_ID", "abc-123")]).unwrap();

        assert_eq!(
            config.auth,
            AzureAuth::Ambient {
                client_id: Some("abc-123".to_string()),
            }
        );
    }

    #[test]
    fn every_row_of_the_cosmos_table() {
        // §5.6.
        assert_eq!(with(&[]).unwrap().cosmos, CosmosMode::Disabled);

        let key = with(&[
            ("COSMOS_ENDPOINT", "https://x.documents.azure.com"),
            ("COSMOS_KEY", "abc"),
        ])
        .unwrap();
        assert!(matches!(key.cosmos, CosmosMode::Key { .. }));
        assert_eq!(key.cosmos.describe(), "key");

        let rbac = with(&[("COSMOS_ENDPOINT", "https://x.documents.azure.com")]).unwrap();
        assert!(matches!(rbac.cosmos, CosmosMode::Rbac { .. }));
        assert_eq!(rbac.cosmos.describe(), "AAD RBAC");
    }

    #[test]
    fn the_cosmos_database_default_is_the_pythons() {
        // spec/02-configuration.md §9.2.2. An earlier draft of the table
        // said `integration-hub-dashboard`; `dashboard/config.py` says
        // `integration-hub`. Reading the wrong database would find no
        // alarm rules and report no error, because an empty database is
        // not a failure — every configured rule would simply be gone.
        let config = with(&[]).unwrap();

        assert_eq!(config.cosmos_database, "integration-hub");
        assert_eq!(config.cosmos_container, "alarms");
    }

    #[test]
    fn an_absent_session_key_is_absent_rather_than_a_default() {
        // §9.2.3 — the Python's default is a literal in a public
        // repository. `None` here means "generate one", which the binary
        // does; it must never mean "use a known string".
        let config = with(&[]).unwrap();

        assert!(config.session_key.is_none());
    }

    #[test]
    fn the_cache_ttl_is_configurable_and_defaults_to_thirty_seconds() {
        // §2.2.1 and §9.2's `API_CACHE_TTL`.
        assert_eq!(with(&[]).unwrap().api_cache_ttl.as_secs(), 30);
        assert_eq!(
            with(&[("API_CACHE_TTL", "5")])
                .unwrap()
                .api_cache_ttl
                .as_secs(),
            5
        );
    }

    #[test]
    fn a_key_without_an_endpoint_is_still_disabled() {
        // §5.6's first row is keyed on the endpoint alone. A stray
        // `COSMOS_KEY` in an environment file does not switch persistence
        // on against nothing.
        let config = with(&[("COSMOS_KEY", "abc")]).unwrap();

        assert_eq!(config.cosmos, CosmosMode::Disabled);
    }

    #[test]
    fn disabling_certificate_verification_against_a_real_endpoint_is_refused() {
        // §5.6.1 — THE refusal in this file. It is the one setting that can
        // silently make a real connection insecure.
        let errors = with(&[
            ("COSMOS_ENDPOINT", "https://prod.documents.azure.com"),
            ("COSMOS_DISABLE_SSL_VERIFY", "true"),
        ])
        .unwrap_err();

        assert!(
            errors.to_string().contains("COSMOS_DISABLE_SSL_VERIFY"),
            "{errors}"
        );
        assert!(errors.to_string().contains("§5.6.1"), "{errors}");
    }

    #[test]
    fn disabling_it_against_the_local_emulator_is_allowed() {
        // §5.6.1's actual purpose.
        for endpoint in [
            "https://localhost:8081",
            "https://127.0.0.1:8081/",
            "https://cosmos:8081",
            "https://cosmos-emulator:8081",
        ] {
            let config = with(&[
                ("COSMOS_ENDPOINT", endpoint),
                ("COSMOS_DISABLE_SSL_VERIFY", "true"),
            ])
            .unwrap_or_else(|error| panic!("{endpoint}: {error}"));

            assert!(config.cosmos_disable_ssl_verify);
        }
    }

    #[test]
    fn a_hostname_that_merely_contains_localhost_is_not_local() {
        // The reason `is_local` looks at the host rather than doing a
        // substring search. `cosmos.localhost.example.com` resolves on the
        // public internet.
        for endpoint in [
            "https://cosmos.localhost.example.com",
            "https://localhost.attacker.example",
            "https://not-localhost",
        ] {
            let errors = with(&[
                ("COSMOS_ENDPOINT", endpoint),
                ("COSMOS_DISABLE_SSL_VERIFY", "true"),
            ])
            .unwrap_err();

            assert!(
                errors.to_string().contains("COSMOS_DISABLE_SSL_VERIFY"),
                "{endpoint} was treated as local: {errors}"
            );
        }
    }

    #[test]
    fn disabling_it_with_no_endpoint_at_all_is_allowed() {
        // Nothing is reached, so nothing is exposed. Refusing a start here
        // would break §1.2 for a variable that does nothing.
        let config = with(&[("COSMOS_DISABLE_SSL_VERIFY", "true")]).unwrap();

        assert_eq!(config.cosmos, CosmosMode::Disabled);
    }

    #[test]
    fn the_thresholds_are_read_and_defaulted() {
        // §3.2 and §9.2.
        let config = with(&[
            ("QUEUE_WARNING_THRESHOLD", "5"),
            ("QUEUE_CRITICAL_THRESHOLD", "25"),
            ("DLQ_WARNING_THRESHOLD", "3"),
        ])
        .unwrap();

        assert_eq!(
            config.thresholds,
            Thresholds {
                queue_warning: 5,
                queue_critical: 25,
                dlq_warning: 3,
            }
        );
    }

    #[test]
    fn a_threshold_that_is_not_a_number_is_a_startup_failure() {
        // Not a silent default. A dashboard reading `fifty` as 50 would
        // look right and be wrong, and the number decides whether an
        // operator is paged.
        let errors = with(&[("QUEUE_CRITICAL_THRESHOLD", "fifty")]).unwrap_err();

        assert!(
            errors.to_string().contains("QUEUE_CRITICAL_THRESHOLD"),
            "{errors}"
        );
    }

    #[test]
    fn no_secret_ever_renders() {
        // `spec/04-observability.md` §2 and §9.2. Two secrets here, and
        // neither may reach a log line.
        let config = with(&[
            ("AZURE_TENANT_ID", "t"),
            ("AZURE_CLIENT_ID", "c"),
            ("AZURE_CLIENT_SECRET", "principal-secret-9f3c"),
            ("COSMOS_ENDPOINT", "https://x.documents.azure.com"),
            ("COSMOS_KEY", "cosmos-key-8b2a"),
            ("FLASK_SECRET_KEY", "session-key-77"),
        ])
        .unwrap();

        let rendered = format!("{config:?}");
        for secret in ["principal-secret-9f3c", "cosmos-key-8b2a", "session-key-77"] {
            assert!(!rendered.contains(secret), "{secret} leaked: {rendered}");
        }
        assert!(
            rendered.contains("documents.azure.com"),
            "an endpoint is not a secret"
        );
    }

    #[test]
    fn redaction_can_be_turned_off_deliberately() {
        // §1.3 — "unless the deployment explicitly disables redaction".
        let config = with(&[("DASHBOARD_REDACT_MESSAGES", "false")]).unwrap();

        assert!(!config.redact);
    }

    #[test]
    fn log_analytics_needs_only_its_workspace() {
        // §4 — each integration is gated on its own settings, so a
        // dashboard with a workspace and no subscription still shows
        // exceptions.
        let config = with(&[("AZURE_LOG_ANALYTICS_WORKSPACE_ID", "w")]).unwrap();

        assert!(config.azure.has_log_analytics());
        assert!(!config.azure.has_service_bus());
    }

    #[test]
    fn service_bus_needs_all_three_of_its_settings() {
        // Two of the three cannot form an ARM resource id, so the panel is
        // unavailable rather than half-configured.
        let partial = with(&[
            ("AZURE_SUBSCRIPTION_ID", "s"),
            ("AZURE_RESOURCE_GROUP", "g"),
        ])
        .unwrap();
        assert!(!partial.azure.has_service_bus());

        let full = with(&[
            ("AZURE_SUBSCRIPTION_ID", "s"),
            ("AZURE_RESOURCE_GROUP", "g"),
            ("AZURE_SERVICE_BUS_NAMESPACE", "n"),
        ])
        .unwrap();
        assert!(full.azure.has_service_bus());
    }
}
