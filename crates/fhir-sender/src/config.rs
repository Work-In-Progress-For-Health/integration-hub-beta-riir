//! The environment.
//!
//! Implements `spec/10-senders.md` §8.7. Upstream: `fhir_sender/app_config.py`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use ihb_config::{ConfigErrors, Reader, Secret};

use crate::auth::Auth;
use crate::client::Settings;

/// Which credentials are configured (`spec/10-senders.md` §8.3).
#[derive(Debug, Clone)]
pub enum AuthConfig {
    /// `FHIR_AUTH_MODE=api_key`, with `FHIR_API_KEY`.
    ApiKey(Secret),
    /// `FHIR_AUTH_MODE=oauth2`.
    OAuth2 {
        /// `FHIR_OAUTH_TOKEN_URL`, `https` only.
        token_url: String,
        /// `FHIR_OAUTH_CLIENT_ID`.
        client_id: String,
        /// `FHIR_OAUTH_CLIENT_SECRET`.
        client_secret: Secret,
        /// `FHIR_OAUTH_SCOPE`.
        scope: String,
    },
}

/// Everything the service reads from the environment.
///
/// # Examples
///
/// ```
/// use fhir_sender::AppConfig;
///
/// let config = AppConfig::from_pairs([
///     ("FHIR_ENDPOINT_URL", "https://promptly.example/fhir/r4/$process-message"),
///     ("FHIR_API_KEY", "k"),
///     ("INGRESS_QUEUE_NAME", "q"),
///     ("INGRESS_SESSION_ID", "s"),
///     ("WORKFLOW_ID", "w"),
///     ("MICROSERVICE_ID", "m"),
///     ("HEALTH_BOARD", "h"),
///     ("PEER_SERVICE", "p"),
/// ])?;
/// assert_eq!(config.max_batch_size, 1);
/// assert_eq!(config.settings.max_attempts, 3);
/// # Ok::<(), ihb_config::ConfigErrors>(())
/// ```
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// `SERVICE_BUS_CONNECTION_STRING`.
    pub connection_string: Option<Secret>,
    /// `SERVICE_BUS_NAMESPACE`.
    pub namespace: Option<String>,
    /// `INGRESS_QUEUE_NAME`. Required.
    pub ingress_queue: String,
    /// `INGRESS_SESSION_ID`. Required: the shared receiver renews locks only
    /// for sessions, and a status poll can outlast the queue's lock duration.
    pub ingress_session_id: String,
    /// `MAX_BATCH_SIZE`, default 1. Each message may poll for up to the poll
    /// timeout, and a renewed lock is capped at five minutes.
    pub max_batch_size: usize,
    /// `FHIR_ENDPOINT_URL`. Required.
    pub endpoint_url: String,
    /// The credentials.
    pub auth: AuthConfig,
    /// The client's tunables.
    pub settings: Settings,
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

    /// The credentials as a live [`Auth`].
    ///
    /// # Errors
    ///
    /// A message when the key is blank or the token URL is not `https`;
    /// [`AppConfig::from_pairs`] has already refused both.
    pub fn build_auth(&self) -> Result<Auth, String> {
        match &self.auth {
            AuthConfig::ApiKey(key) => Auth::api_key(key.clone()),
            AuthConfig::OAuth2 {
                token_url,
                client_id,
                client_secret,
                scope,
            } => Auth::oauth2(
                token_url,
                client_id,
                client_secret.clone(),
                scope,
                Duration::try_from_secs_f64(self.settings.timeout_seconds)
                    .map_err(|_| "the timeout is not a valid duration".to_string())?,
            ),
        }
    }

    /// The shared body of the constructors.
    fn read(mut reader: Reader) -> Result<Self, ConfigErrors> {
        let mode = reader
            .string_or("FHIR_AUTH_MODE", "api_key")
            .to_ascii_lowercase();
        let auth = match mode.as_str() {
            "api_key" => AuthConfig::ApiKey(reader.required_secret("FHIR_API_KEY")),
            "oauth2" => AuthConfig::OAuth2 {
                token_url: reader.required_string("FHIR_OAUTH_TOKEN_URL"),
                client_id: reader.required_string("FHIR_OAUTH_CLIENT_ID"),
                client_secret: reader.required_secret("FHIR_OAUTH_CLIENT_SECRET"),
                scope: reader.required_string("FHIR_OAUTH_SCOPE"),
            },
            other => {
                reader.invalid("FHIR_AUTH_MODE", "api_key or oauth2", other.to_string());
                AuthConfig::ApiKey(Secret::new(""))
            }
        };
        if let AuthConfig::OAuth2 { token_url, .. } = &auth
            && !token_url.to_ascii_lowercase().starts_with("https://")
        {
            reader.conflict("FHIR_OAUTH_TOKEN_URL must use HTTPS");
        }

        let endpoint_url = reader.required_string("FHIR_ENDPOINT_URL");
        // Only the literal `true`, as upstream.
        let allow_insecure_http = reader
            .raw("FHIR_ALLOW_INSECURE_HTTP")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
        let lower = endpoint_url.to_ascii_lowercase();
        let scheme_ok =
            lower.starts_with("https://") || (allow_insecure_http && lower.starts_with("http://"));
        if !endpoint_url.is_empty() && !scheme_ok {
            reader.conflict(
                "FHIR_ENDPOINT_URL must use HTTPS (plain HTTP only with FHIR_ALLOW_INSECURE_HTTP=true)",
            );
        }

        let defaults = Settings::default();
        let settings = Settings {
            timeout_seconds: positive(
                &mut reader,
                "FHIR_TIMEOUT_SECONDS",
                defaults.timeout_seconds,
            ),
            max_attempts: reader
                .optional_positive_integer("FHIR_MAX_ATTEMPTS")
                .unwrap_or(defaults.max_attempts),
            backoff_seconds: positive(
                &mut reader,
                "FHIR_BACKOFF_SECONDS",
                defaults.backoff_seconds,
            ),
            max_retry_after_seconds: positive(
                &mut reader,
                "FHIR_MAX_RETRY_AFTER_SECONDS",
                defaults.max_retry_after_seconds,
            ),
            poll_interval_seconds: positive(
                &mut reader,
                "FHIR_POLL_INTERVAL_SECONDS",
                defaults.poll_interval_seconds,
            ),
            poll_timeout_seconds: positive(
                &mut reader,
                "FHIR_POLL_TIMEOUT_SECONDS",
                defaults.poll_timeout_seconds,
            ),
            allow_insecure_http,
        };

        let health_host = reader.string_or("HEALTH_CHECK_HOST", ihb_health::DEFAULT_HOST);
        let health_port: Option<u16> = reader.optional_positive_integer("HEALTH_CHECK_PORT");

        let config = Self {
            connection_string: reader.optional_secret("SERVICE_BUS_CONNECTION_STRING"),
            namespace: reader.optional_string("SERVICE_BUS_NAMESPACE"),
            ingress_queue: reader.required_string("INGRESS_QUEUE_NAME"),
            ingress_session_id: reader.required_string("INGRESS_SESSION_ID"),
            max_batch_size: reader
                .optional_positive_integer("MAX_BATCH_SIZE")
                .unwrap_or(1),
            endpoint_url,
            auth,
            settings,
            workflow_id: reader.required_string("WORKFLOW_ID"),
            microservice_id: reader.required_string("MICROSERVICE_ID"),
            health_board: reader.required_string("HEALTH_BOARD"),
            peer_service: reader.required_string("PEER_SERVICE"),
            health_check: SocketAddr::new(
                health_host
                    .parse::<IpAddr>()
                    .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                health_port.unwrap_or(ihb_health::DEFAULT_PORT),
            ),
        };

        reader.finish_with(config)
    }
}

/// A positive finite number, defaulting when absent.
///
/// Unparseable or non-positive is a startup failure, as upstream's
/// `_read_positive_number`.
fn positive(reader: &mut Reader, name: &str, default: f64) -> f64 {
    let Some(raw) = reader.raw(name).map(str::to_string) else {
        return default;
    };
    match raw.trim().parse::<f64>() {
        Ok(number) if number.is_finite() && number > 0.0 => number,
        _ => {
            reader.invalid(name, "a positive number", raw);
            default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "FHIR_ENDPOINT_URL",
                "https://p.example/fhir/r4/$process-message",
            ),
            ("FHIR_API_KEY", "key-1"),
            ("INGRESS_QUEUE_NAME", "q"),
            ("INGRESS_SESSION_ID", "wpas-to-proms"),
            ("WORKFLOW_ID", "wpas-to-proms"),
            ("MICROSERVICE_ID", "fhir-sender"),
            ("HEALTH_BOARD", "PROMS"),
            ("PEER_SERVICE", "PROMPTLY"),
        ]
    }

    fn with(extra: &[(&'static str, &'static str)]) -> Result<AppConfig, ConfigErrors> {
        let mut pairs = valid();
        pairs.extend_from_slice(extra);
        AppConfig::from_pairs(pairs)
    }

    #[test]
    fn the_defaults_are_the_upstream_ones() {
        // spec/10-senders.md §8.7.
        let config = with(&[]).unwrap();
        assert_eq!(config.max_batch_size, 1);
        assert_eq!(config.settings, Settings::default());
        assert!((config.settings.timeout_seconds - 30.0).abs() < f64::EPSILON);
        assert_eq!(config.settings.max_attempts, 3);
        assert!(matches!(config.auth, AuthConfig::ApiKey(_)));
    }

    #[test]
    fn every_problem_is_reported_together() {
        let errors = AppConfig::from_pairs([("FHIR_AUTH_MODE", "basic")]).unwrap_err();
        let text = errors.to_string();
        for name in [
            "FHIR_AUTH_MODE",
            "FHIR_ENDPOINT_URL",
            "INGRESS_QUEUE_NAME",
            "INGRESS_SESSION_ID",
            "WORKFLOW_ID",
        ] {
            assert!(text.contains(name), "{name} in {text}");
        }
    }

    #[test]
    fn api_key_mode_needs_the_key_and_oauth_mode_needs_its_four() {
        let pairs: Vec<_> = valid()
            .into_iter()
            .filter(|(name, _)| *name != "FHIR_API_KEY")
            .collect();
        assert!(
            AppConfig::from_pairs(pairs.clone())
                .unwrap_err()
                .to_string()
                .contains("FHIR_API_KEY")
        );

        let mut oauth = pairs;
        oauth.push(("FHIR_AUTH_MODE", "OAuth2"));
        let text = AppConfig::from_pairs(oauth.clone())
            .unwrap_err()
            .to_string();
        for name in [
            "FHIR_OAUTH_TOKEN_URL",
            "FHIR_OAUTH_CLIENT_ID",
            "FHIR_OAUTH_CLIENT_SECRET",
            "FHIR_OAUTH_SCOPE",
        ] {
            assert!(text.contains(name), "{name}");
        }

        oauth.extend([
            ("FHIR_OAUTH_TOKEN_URL", "https://auth.example/t"),
            ("FHIR_OAUTH_CLIENT_ID", "i"),
            ("FHIR_OAUTH_CLIENT_SECRET", "s"),
            ("FHIR_OAUTH_SCOPE", "target-entity:1"),
        ]);
        let config = AppConfig::from_pairs(oauth).unwrap();
        assert!(config.build_auth().is_ok());
    }

    #[test]
    fn plain_http_is_refused_unless_explicitly_allowed() {
        // spec/10-senders.md §8.4.
        let local = [("FHIR_ENDPOINT_URL", "http://mock:8080/x")];
        assert!(with(&local).is_err());
        let mut allowed = local.to_vec();
        allowed.push(("FHIR_ALLOW_INSECURE_HTTP", "true"));
        // The later pair wins for a duplicate name, as in the environment.
        assert!(with(&allowed).is_ok());
        let mut yes = local.to_vec();
        yes.push(("FHIR_ALLOW_INSECURE_HTTP", "yes"));
        assert!(with(&yes).is_err(), "only the literal true");
    }

    #[test]
    fn a_plain_http_token_endpoint_is_refused() {
        let mut pairs: Vec<_> = valid()
            .into_iter()
            .filter(|(name, _)| *name != "FHIR_API_KEY")
            .collect();
        pairs.extend([
            ("FHIR_AUTH_MODE", "oauth2"),
            ("FHIR_OAUTH_TOKEN_URL", "http://auth/t"),
            ("FHIR_OAUTH_CLIENT_ID", "i"),
            ("FHIR_OAUTH_CLIENT_SECRET", "s"),
            ("FHIR_OAUTH_SCOPE", "x"),
        ]);
        assert!(
            AppConfig::from_pairs(pairs)
                .unwrap_err()
                .to_string()
                .contains("HTTPS")
        );
    }

    #[test]
    fn numbers_must_be_positive_and_parse() {
        for (name, bad) in [
            ("FHIR_TIMEOUT_SECONDS", "0"),
            ("FHIR_BACKOFF_SECONDS", "-1"),
            ("FHIR_POLL_INTERVAL_SECONDS", "soon"),
            ("FHIR_POLL_TIMEOUT_SECONDS", "inf"),
        ] {
            assert!(with(&[(name, bad)]).is_err(), "{name}={bad}");
        }
        let config = with(&[
            ("FHIR_POLL_INTERVAL_SECONDS", "1"),
            ("FHIR_MAX_ATTEMPTS", "5"),
            ("MAX_BATCH_SIZE", "2"),
        ])
        .unwrap();
        assert!((config.settings.poll_interval_seconds - 1.0).abs() < f64::EPSILON);
        assert_eq!(config.settings.max_attempts, 5);
        assert_eq!(config.max_batch_size, 2);
    }

    #[test]
    fn secrets_do_not_appear_in_debug_output() {
        let text = format!("{:?}", with(&[]).unwrap());
        assert!(!text.contains("key-1"), "{text}");
    }
}
