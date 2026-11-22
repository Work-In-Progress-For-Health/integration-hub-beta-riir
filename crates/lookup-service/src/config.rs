//! Everything the lookup service reads from its environment.
//!
//! Implements `spec/20-lookup-service.md` §2 and
//! `spec/02-configuration.md` §9.3.
//!
//! Authentication is required (§2.7): either it is configured
//! (`LOOKUP_AUTH_TENANT_ID` and `LOOKUP_AUTH_AUDIENCE`) or it is switched off
//! on purpose (`LOOKUP_AUTH_DISABLED`, §2.8). Otherwise the service does not
//! start, because serving unauthenticated by omission is how a missing
//! variable becomes an open service.
//!
//! Three more combinations are refused because proceeding would be worse
//! than stopping: `COSMOS_DISABLE_SSL_VERIFY` against a non-local endpoint
//! (§2.4), `COSMOS_ENDPOINT` at all, because the Cosmos store is not ported
//! (§2.5), and the auth switch in the wrong environment (§2.8).
//!
//! Reference implementation: `lookup_service/lookup_service/config.py`
//! (upstream `2a220e3`, #426).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use ihb_config::{ConfigErrors, Reader, Secret};

/// Environments that expose the Python's API documentation (§2.1). Recorded
/// for the settings contract; this port serves none (§9.2).
pub const SWAGGER_ENABLED_ENVIRONMENTS: [&str; 3] = ["LOCAL", "DEV", "SIT"];

/// The environment of a developer machine (§2.3).
pub const LOCAL_ENVIRONMENT: &str = "LOCAL";

/// Where `LOOKUP_AUTH_DISABLED` may be set (§2.8): `LOCAL`, plus `DEV` and
/// `TST` for the unauthenticated trial (upstream plan Phase 1.25).
pub const AUTH_DISABLE_ALLOWED_ENVIRONMENTS: [&str; 3] = ["LOCAL", "DEV", "TST"];

/// What `LOOKUP_AUTH_DISABLED_ACK` must equal outside `LOCAL` (§2.8).
///
/// Long on purpose: one stray `LOOKUP_AUTH_DISABLED=true` in a variables
/// file must not be enough to open a deployed service.
pub const AUTH_DISABLED_ACK_VALUE: &str = "I_UNDERSTAND_LOOKUP_IS_UNAUTHENTICATED";

/// Hosts a Cosmos emulator endpoint may name (§2.4).
pub const LOCAL_COSMOS_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "cosmos-emulator"];

/// `ENVIRONMENT` default.
pub const DEFAULT_ENVIRONMENT: &str = "DEV";

/// `PORT` default.
pub const DEFAULT_PORT: u16 = 8080;

/// Where the seed data comes from (§7, §10.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedLocation {
    /// `LOOKUP_SEED_DIR` is unset, the deployed default: no tables are
    /// seeded and, with no writable store, none are served. Deployed
    /// environments are loaded through the GUI or API (upstream `2a220e3`
    /// removed the seed from the image).
    None,
    /// `LOOKUP_SEED_DIR`: only local Compose sets it.
    Dir(PathBuf),
}

/// The service's settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// Seed data.
    pub seed: SeedLocation,
    /// `ENVIRONMENT`, upper-cased.
    pub environment: String,
    /// Where the listener binds.
    pub listen: SocketAddr,
    /// `COSMOS_ENDPOINT`.
    pub cosmos_endpoint: Option<String>,
    /// `COSMOS_KEY`.
    pub cosmos_key: Option<Secret>,
    /// `COSMOS_DATABASE`.
    pub cosmos_database: String,
    /// `COSMOS_DISABLE_SSL_VERIFY`.
    pub cosmos_disable_ssl_verify: bool,
    /// `LOOKUP_ROWS_CONTAINER`.
    pub rows_container: String,
    /// `LOOKUP_CONFIG_CONTAINER`.
    pub config_container: String,
    /// `LOOKUP_DEFAULT_TTL_SECONDS`.
    pub default_ttl_seconds: u32,
    /// `LOOKUP_CACHE_MAX_ENTRIES`.
    pub cache_max_entries: usize,
    /// `LOOKUP_MAX_UPLOAD_MB`.
    pub max_upload_mb: usize,
    /// `LOOKUP_UPLOAD_CONCURRENCY`.
    pub upload_concurrency: usize,
    /// `LOOKUP_AUTH_TENANT_ID`: the Entra tenant whose tokens are accepted.
    pub auth_tenant_id: Option<String>,
    /// `LOOKUP_AUTH_AUDIENCE`: the `aud` a token must carry.
    pub auth_audience: Option<String>,
    /// `LOOKUP_AUTH_DISABLED` (§2.8). `false` by default, including in
    /// [`Config::default`]: a configuration built by hand is closed too.
    pub auth_disabled: bool,
    /// `LOOKUP_AUTH_DISABLED_ACK` (§2.8).
    pub auth_disabled_ack: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            seed: SeedLocation::None,
            environment: DEFAULT_ENVIRONMENT.to_owned(),
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT),
            cosmos_endpoint: None,
            cosmos_key: None,
            cosmos_database: "integration-hub".to_owned(),
            cosmos_disable_ssl_verify: false,
            rows_container: "lookup-rows".to_owned(),
            config_container: "lookup-config".to_owned(),
            default_ttl_seconds: 300,
            cache_max_entries: 10_000,
            max_upload_mb: 50,
            upload_concurrency: 32,
            auth_tenant_id: None,
            auth_audience: None,
            auth_disabled: false,
            auth_disabled_ack: None,
        }
    }
}

impl Config {
    /// Whether the Python would expose its API documentation (§2.1).
    #[must_use]
    pub fn swagger_enabled(&self) -> bool {
        SWAGGER_ENABLED_ENVIRONMENTS.contains(&self.environment.as_str())
    }

    /// Whether this is a developer machine (§2.3): the `LOCAL` environment
    /// uses plain cookies and the `local-dev` principal.
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.environment == LOCAL_ENVIRONMENT
    }

    /// Whether callers are authenticated (§2.7).
    #[must_use]
    pub const fn auth_enabled(&self) -> bool {
        !self.auth_disabled
    }

    /// Authentication is off in a **deployed** environment, i.e. the
    /// `DEV`/`TST` trial rather than a laptop (§2.8). It earns a warning at
    /// start-up and a banner on every page.
    #[must_use]
    pub fn unauthenticated_deployment(&self) -> bool {
        self.auth_disabled && !self.is_local()
    }

    /// The upload size limit in bytes.
    #[must_use]
    ///
    /// `None` when the megabyte setting does not fit a `usize` in bytes; startup
    /// refuses such a setting (§2.6), so callers treat `None` as "refuse the
    /// upload" and never as "no limit".
    pub const fn max_upload_bytes(&self) -> Option<usize> {
        self.max_upload_mb.checked_mul(BYTES_PER_MB)
    }

    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
    pub fn from_env() -> Result<Self, ConfigErrors> {
        let mut reader = Reader::from_env();
        let config = Self::read(&mut reader);
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
        let config = Self::read(&mut reader);
        reader.finish()?;
        Ok(config)
    }

    /// Read from a reader the caller owns; problems accumulate in it.
    pub fn read(reader: &mut Reader) -> Self {
        let seed = reader
            .optional_string("LOOKUP_SEED_DIR")
            .map_or(SeedLocation::None, |dir| SeedLocation::Dir(dir.into()));
        let environment = reader
            .string_or("ENVIRONMENT", DEFAULT_ENVIRONMENT)
            .to_uppercase();
        let host = reader.string_or("HOST", "0.0.0.0");
        let port = reader.integer_or("PORT", DEFAULT_PORT);
        let ip = host.parse::<IpAddr>().unwrap_or_else(|_| {
            reader.invalid("HOST", "an IP address", host.clone());
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        });
        let listen = SocketAddr::new(ip, port);
        let cosmos_endpoint = reader.optional_string("COSMOS_ENDPOINT");
        let cosmos_key = reader.optional_secret("COSMOS_KEY");
        let cosmos_disable_ssl_verify = reader.lenient_bool_or("COSMOS_DISABLE_SSL_VERIFY", false);
        let cosmos_database = reader.string_or("COSMOS_DATABASE", "integration-hub");
        let rows_container = reader.string_or("LOOKUP_ROWS_CONTAINER", "lookup-rows");
        let config_container = reader.string_or("LOOKUP_CONFIG_CONTAINER", "lookup-config");
        let default_ttl_seconds = at_least_one(reader, "LOOKUP_DEFAULT_TTL_SECONDS", 300_u32);
        let cache_max_entries = at_least_one(reader, "LOOKUP_CACHE_MAX_ENTRIES", 10_000_usize);
        let max_upload_mb = at_least_one(reader, "LOOKUP_MAX_UPLOAD_MB", 50_usize);
        if max_upload_mb.checked_mul(BYTES_PER_MB).is_none() {
            // A size whose byte count overflows is invalid, not "unlimited" (§2.6).
            reader.invalid(
                "LOOKUP_MAX_UPLOAD_MB",
                "a size whose byte count fits in memory",
                max_upload_mb.to_string(),
            );
        }
        let upload_concurrency = at_least_one(reader, "LOOKUP_UPLOAD_CONCURRENCY", 32_usize);
        let auth_tenant_id = reader.optional_string("LOOKUP_AUTH_TENANT_ID");
        let auth_audience = reader.optional_string("LOOKUP_AUTH_AUDIENCE");
        // Strict, unlike most flags here: `maybe` must be an error, not
        // `false`, or a typo silently closes (or, worse, opens) the service.
        // Upstream `_read_bool_env` accepts exactly true/false/1/0.
        let auth_disabled = auth_flag(reader, "LOOKUP_AUTH_DISABLED");
        let auth_disabled_ack = reader.optional_string("LOOKUP_AUTH_DISABLED_ACK");

        if cosmos_disable_ssl_verify && let Some(endpoint) = &cosmos_endpoint {
            let endpoint = endpoint.to_lowercase();
            // The emulator's certificate is self-signed; never disable
            // verification for a real account (§2.4).
            if !LOCAL_COSMOS_HOSTS
                .iter()
                .any(|host| endpoint.contains(host))
            {
                reader.conflict(
                    "COSMOS_DISABLE_SSL_VERIFY is only allowed for a local Cosmos emulator endpoint",
                );
            }
        }

        let config = Self {
            seed,
            environment,
            listen,
            cosmos_endpoint,
            cosmos_key,
            cosmos_database,
            cosmos_disable_ssl_verify,
            rows_container,
            config_container,
            default_ttl_seconds,
            cache_max_entries,
            max_upload_mb,
            upload_concurrency,
            auth_tenant_id,
            auth_audience,
            auth_disabled,
            auth_disabled_ack,
        };
        config.validate_auth(reader);
        config
    }

    /// §2.7 and §2.8, reported with the other problems.
    fn validate_auth(&self, reader: &mut Reader) {
        if self.auth_disabled {
            if !AUTH_DISABLE_ALLOWED_ENVIRONMENTS.contains(&self.environment.as_str()) {
                reader.conflict(format!(
                    "LOOKUP_AUTH_DISABLED=true is only allowed when ENVIRONMENT is one of {}",
                    AUTH_DISABLE_ALLOWED_ENVIRONMENTS.join(", ")
                ));
            } else if !self.is_local()
                && self.auth_disabled_ack.as_deref() != Some(AUTH_DISABLED_ACK_VALUE)
            {
                reader.conflict(format!(
                    "LOOKUP_AUTH_DISABLED=true in {} also needs \
                     LOOKUP_AUTH_DISABLED_ACK={AUTH_DISABLED_ACK_VALUE}",
                    self.environment
                ));
            }
        }
        // Fail closed: never serve unauthenticated because configuration is
        // missing.
        if self.auth_enabled() && !(self.auth_tenant_id.is_some() && self.auth_audience.is_some()) {
            let hint = if self.is_local() {
                " (set LOOKUP_AUTH_DISABLED=true for local development)"
            } else {
                ""
            };
            reader.conflict(format!(
                "Authentication is not configured: set LOOKUP_AUTH_TENANT_ID and \
                 LOOKUP_AUTH_AUDIENCE{hint}"
            ));
        }
    }
}

/// A flag that is exactly `true`, `false`, `1` or `0` (case-insensitive),
/// absent meaning `false`; anything else is an error (§2.8).
fn auth_flag(reader: &mut Reader, name: &str) -> bool {
    match reader
        .optional_string(name)
        .map(|v| v.to_lowercase())
        .as_deref()
    {
        None | Some("false" | "0") => false,
        Some("true" | "1") => true,
        Some(other) => {
            reader.invalid(name, "true or false", other.to_owned());
            false
        }
    }
}

/// Bytes in a megabyte (`RCA-EXC-1`: a constant expression).
const BYTES_PER_MB: usize = 1024 * 1024;

/// An integer that must be a whole number of at least 1 (§2.6).
fn at_least_one<T>(reader: &mut Reader, name: &str, default: T) -> T
where
    T: std::str::FromStr + PartialOrd + From<u8> + Copy + std::fmt::Display,
{
    let value = reader.integer_or(name, default);
    if value < T::from(1_u8) {
        reader.invalid(name, "a whole number >= 1", value.to_string());
        return default;
    }
    value
}
