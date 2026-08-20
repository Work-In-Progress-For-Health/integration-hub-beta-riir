//! How a service reaches Service Bus, and — the part that matters — whether
//! it does so over TLS.
//!
//! Implements `spec/03-message-bus.md` §1 and §8.1.1.
//!
//! # The rule this module exists to enforce
//!
//! **A real namespace is never reached over plaintext.**
//!
//! `azservicebus` does not parse `UseDevelopmentEmulator` at all: given the
//! emulator's own connection string it dials TLS on 5671 and fails with a
//! bare `Connection refused`. The way in is the crate's `unsecured` feature
//! — and that feature, used by mistake, would put clinical messages on the
//! wire unencrypted.
//!
//! So the choice is made **here, once**, from the connection string itself,
//! and [`Endpoint::is_plaintext`] is the only thing that can select it.
//! `spec/03-message-bus.md` §8.1.1 requires a test that a production-shaped
//! connection string never does; this module's tests have several.

use ihb_config::Secret;

use crate::error::BusError;

/// The suffix a namespace short name is completed with.
const NAMESPACE_SUFFIX: &str = ".servicebus.windows.net";

/// The connection-string flag the local emulator sets.
const EMULATOR_FLAG: &str = "usedevelopmentemulator";

/// How a service was told to reach Service Bus.
///
/// Built from configuration by [`Endpoint::from_config`], which applies the
/// precedence of `spec/03-message-bus.md` §1.1: a connection string wins
/// when both are set.
#[derive(Clone)]
pub enum Endpoint {
    /// A full connection string.
    ConnectionString(Secret),
    /// A namespace short name, reached with the ambient Azure credential.
    Namespace(String),
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A connection string carries a shared access key. It renders as
        // `<redacted>` through `Secret`, and this keeps that true for the
        // enum around it. See spec/02-configuration.md §10.4.
        match self {
            Self::ConnectionString(secret) => {
                f.debug_tuple("ConnectionString").field(secret).finish()
            }
            Self::Namespace(namespace) => f.debug_tuple("Namespace").field(namespace).finish(),
        }
    }
}

impl Endpoint {
    /// Resolve an endpoint from the two configuration variables.
    ///
    /// # Errors
    ///
    /// [`BusError::NotConfigured`] when neither is set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_bus::connection::Endpoint;
    ///
    /// // A connection string wins when both are set.
    /// let endpoint = Endpoint::from_config(
    ///     Some("Endpoint=sb://real.servicebus.windows.net;SharedAccessKey=k"),
    ///     Some("real"),
    /// )?;
    /// assert!(matches!(endpoint, Endpoint::ConnectionString(_)));
    ///
    /// let endpoint = Endpoint::from_config(None, Some("real"))?;
    /// assert_eq!(endpoint.namespace(), "real.servicebus.windows.net");
    /// # Ok::<(), ihb_bus::BusError>(())
    /// ```
    pub fn from_config(
        connection_string: Option<&str>,
        namespace: Option<&str>,
    ) -> Result<Self, BusError> {
        let connection_string = connection_string
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let namespace = namespace.map(str::trim).filter(|value| !value.is_empty());

        match (connection_string, namespace) {
            (Some(text), _) => Ok(Self::ConnectionString(Secret::new(text))),
            (None, Some(name)) => Ok(Self::Namespace(name.to_string())),
            (None, None) => Err(BusError::NotConfigured),
        }
    }

    /// The fully qualified namespace this endpoint reaches.
    ///
    /// For a namespace short name, `.servicebus.windows.net` is appended
    /// unless it is already there. For a connection string, the host of its `Endpoint`.
    #[must_use]
    pub fn namespace(&self) -> String {
        match self {
            Self::Namespace(name) => {
                if name.contains('.') {
                    name.clone()
                } else {
                    format!("{name}{NAMESPACE_SUFFIX}")
                }
            }
            Self::ConnectionString(secret) => host_of(secret.expose()).unwrap_or_default(),
        }
    }

    /// Whether this endpoint is the local emulator, and may be reached over
    /// plaintext AMQP.
    ///
    /// **This is the only thing that can select an unencrypted connection**,
    /// and it says yes to exactly one shape of input: a connection string
    /// carrying `UseDevelopmentEmulator=true`. A namespace never qualifies —
    /// there is no such thing as a local emulator reached by namespace — and
    /// neither does any connection string without the flag.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_bus::connection::Endpoint;
    ///
    /// let emulator = Endpoint::from_config(
    ///     Some("Endpoint=sb://localhost;SharedAccessKey=k;UseDevelopmentEmulator=true;"),
    ///     None,
    /// )?;
    /// assert!(emulator.is_plaintext());
    ///
    /// // Everything else is TLS, including a connection string that merely
    /// // mentions localhost.
    /// let looks_local = Endpoint::from_config(Some("Endpoint=sb://localhost;SharedAccessKey=k"), None)?;
    /// assert!(!looks_local.is_plaintext());
    /// # Ok::<(), ihb_bus::BusError>(())
    /// ```
    #[must_use]
    pub fn is_plaintext(&self) -> bool {
        let Self::ConnectionString(secret) = self else {
            // A namespace is always a real namespace.
            return false;
        };

        // The flag, case-insensitively, set to exactly `true`. A value of
        // `false`, or anything unrecognised, is not a licence to drop TLS.
        secret
            .expose()
            .split(';')
            .filter_map(|part| part.split_once('='))
            .any(|(key, value)| {
                key.trim().eq_ignore_ascii_case(EMULATOR_FLAG)
                    && value.trim().eq_ignore_ascii_case("true")
            })
    }

    /// The connection string, when there is one.
    ///
    /// Named to be conspicuous: every call is a place a shared access key
    /// leaves this type.
    #[must_use]
    pub fn expose_connection_string(&self) -> Option<&str> {
        match self {
            Self::ConnectionString(secret) => Some(secret.expose()),
            Self::Namespace(_) => None,
        }
    }
}

/// The host of a connection string's `Endpoint=sb://host` part.
fn host_of(connection_string: &str) -> Option<String> {
    let endpoint = connection_string
        .split(';')
        .filter_map(|part| part.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("endpoint"))
        .map(|(_, value)| value.trim())?;

    let without_scheme = endpoint
        .strip_prefix("sb://")
        .or_else(|| endpoint.strip_prefix("amqps://"))
        .or_else(|| endpoint.strip_prefix("amqp://"))
        .unwrap_or(endpoint);

    Some(
        without_scheme
            .trim_end_matches('/')
            .split('/')
            .next()
            .unwrap_or(without_scheme)
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection string of the shape a real deployment uses.
    const PRODUCTION: &str = "Endpoint=sb://inthub-prod.servicebus.windows.net/;\
         SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=NEEDLEKEY123";

    /// The emulator's own, as it prints on startup.
    const EMULATOR: &str = "Endpoint=sb://localhost;\
         SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=SAS_KEY_VALUE;\
         UseDevelopmentEmulator=true;";

    #[test]
    fn a_connection_string_wins_over_a_namespace() {
        // spec/03-message-bus.md §1.1
        let endpoint = Endpoint::from_config(Some(PRODUCTION), Some("other")).unwrap();
        assert!(matches!(endpoint, Endpoint::ConnectionString(_)));
    }

    #[test]
    fn a_namespace_is_completed_with_the_azure_suffix() {
        // spec/03-message-bus.md §1.2
        let endpoint = Endpoint::from_config(None, Some("inthub-prod")).unwrap();
        assert_eq!(endpoint.namespace(), "inthub-prod.servicebus.windows.net");
    }

    #[test]
    fn a_namespace_that_is_already_qualified_is_left_alone() {
        let endpoint =
            Endpoint::from_config(None, Some("inthub-prod.servicebus.windows.net")).unwrap();
        assert_eq!(endpoint.namespace(), "inthub-prod.servicebus.windows.net");
    }

    #[test]
    fn neither_configured_is_an_error() {
        // spec/02-configuration.md §2.1.1
        assert!(matches!(
            Endpoint::from_config(None, None),
            Err(BusError::NotConfigured)
        ));
        assert!(matches!(
            Endpoint::from_config(Some("  "), Some("\t")),
            Err(BusError::NotConfigured)
        ));
    }

    #[test]
    fn only_the_emulator_flag_selects_plaintext() {
        // spec/03-message-bus.md §8.1.1 — the security-critical assertion.
        let emulator = Endpoint::from_config(Some(EMULATOR), None).unwrap();
        assert!(emulator.is_plaintext());
    }

    #[test]
    fn a_production_connection_string_never_selects_plaintext() {
        // spec/03-message-bus.md §8.1.1. A plaintext connection to a real
        // namespace would put clinical messages on the wire unencrypted.
        let endpoint = Endpoint::from_config(Some(PRODUCTION), None).unwrap();
        assert!(!endpoint.is_plaintext());
    }

    #[test]
    fn a_namespace_never_selects_plaintext() {
        // There is no local emulator reached by namespace, so this shape has
        // no legitimate plaintext case at all.
        for name in ["inthub-prod", "localhost", "127.0.0.1", "sb-emulator"] {
            let endpoint = Endpoint::from_config(None, Some(name)).unwrap();
            assert!(!endpoint.is_plaintext(), "{name} selected plaintext");
        }
    }

    #[test]
    fn a_connection_string_that_merely_looks_local_does_not_select_plaintext() {
        // The trap: matching on the host rather than the flag. `localhost`
        // in a connection string is not a licence to drop TLS.
        for text in [
            "Endpoint=sb://localhost;SharedAccessKey=k",
            "Endpoint=sb://127.0.0.1;SharedAccessKey=k",
            "Endpoint=sb://sb-emulator;SharedAccessKey=k",
            "Endpoint=sb://emulator.servicebus.windows.net;SharedAccessKey=k",
        ] {
            let endpoint = Endpoint::from_config(Some(text), None).unwrap();
            assert!(!endpoint.is_plaintext(), "{text} selected plaintext");
        }
    }

    #[test]
    fn the_emulator_flag_set_to_false_does_not_select_plaintext() {
        let text = "Endpoint=sb://localhost;SharedAccessKey=k;UseDevelopmentEmulator=false;";
        let endpoint = Endpoint::from_config(Some(text), None).unwrap();
        assert!(!endpoint.is_plaintext());
    }

    #[test]
    fn the_emulator_flag_is_read_case_insensitively() {
        // Azure's own tooling varies the casing.
        for text in [
            "Endpoint=sb://localhost;usedevelopmentemulator=true",
            "Endpoint=sb://localhost;USEDEVELOPMENTEMULATOR=TRUE",
            "Endpoint=sb://localhost;UseDevelopmentEmulator=True",
        ] {
            let endpoint = Endpoint::from_config(Some(text), None).unwrap();
            assert!(endpoint.is_plaintext(), "{text} did not select plaintext");
        }
    }

    #[test]
    fn a_value_that_merely_contains_the_flag_name_does_not_select_plaintext() {
        // Substring matching would say yes to this; splitting on `;` and `=`
        // says no, which is the right answer.
        let text = "Endpoint=sb://real.servicebus.windows.net;\
                    SharedAccessKeyName=UseDevelopmentEmulator=true";
        let endpoint = Endpoint::from_config(Some(text), None).unwrap();
        assert!(!endpoint.is_plaintext());
    }

    #[test]
    fn the_host_is_read_from_a_connection_string() {
        let endpoint = Endpoint::from_config(Some(PRODUCTION), None).unwrap();
        assert_eq!(endpoint.namespace(), "inthub-prod.servicebus.windows.net");

        let emulator = Endpoint::from_config(Some(EMULATOR), None).unwrap();
        assert_eq!(emulator.namespace(), "localhost");
    }

    #[test]
    fn the_debug_rendering_does_not_print_the_key() {
        // spec/02-configuration.md §10.4
        let endpoint = Endpoint::from_config(Some(PRODUCTION), None).unwrap();
        let rendered = format!("{endpoint:?}");
        assert!(!rendered.contains("NEEDLEKEY123"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn a_namespace_debug_rendering_is_readable() {
        // A namespace is not a secret, and hiding it would make a
        // misconfiguration harder to diagnose for no gain.
        let endpoint = Endpoint::from_config(None, Some("inthub-prod")).unwrap();
        assert!(format!("{endpoint:?}").contains("inthub-prod"));
    }
}
