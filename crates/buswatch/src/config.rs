//! What one BusWatch is.
//!
//! Implements `spec/02-configuration.md` §9.1 and
//! `spec/14-buswatch.md` §2.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ihb_config::{ConfigErrors, Reader};

/// The default connection string (§2.1).
///
/// Right for a host-run process and **wrong inside Compose**, where it is
/// overridden to `sb://sb-emulator`. Kept as the default because BusWatch is
/// most often run from a terminal against a local emulator.
pub const DEFAULT_CONNECTION_STRING: &str = "Endpoint=sb://localhost;\
     SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=SAS_KEY_VALUE;\
     UseDevelopmentEmulator=true;";

/// Messages peeked per queue.
pub const DEFAULT_PEEK_COUNT: usize = 25;

/// Messages scanned when resolving a sequence number (§5.1).
pub const DEFAULT_DETAIL_SEARCH_LIMIT: usize = 250;

/// How many queues are refreshed at once (§3.3).
pub const DEFAULT_PARALLELISM: usize = 2;

/// One BusWatch's configuration.
///
/// # Examples
///
/// ```
/// use buswatch::config::Config;
///
/// let config = Config::from_pairs([] as [(&str, &str); 0])
///     .expect("every value has a default");
///
/// assert_eq!(config.peek_count, 25);
/// assert!(config.queue_names.is_empty(), "blank means discover");
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the HTTP listener binds.
    pub listen: SocketAddr,
    /// `SERVICEBUS_CONNECTION_STRING` — note the spelling (§2).
    pub connection_string: String,
    /// `BUSWATCH_QUEUE_NAMES`. Empty means discover (§2.2).
    pub queue_names: Vec<String>,
    /// `BUSWATCH_PEEK_COUNT`.
    pub peek_count: usize,
    /// `BUSWATCH_DETAIL_SEARCH_LIMIT`.
    pub detail_search_limit: usize,
    /// How many queues refresh at once.
    pub parallelism: usize,
}

impl Config {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Every problem found, together.
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

    /// The shared body of the two constructors.
    fn read(mut reader: Reader) -> Result<Self, ConfigErrors> {
        let host = reader.string_or("HOST", "127.0.0.1");
        let port = reader.integer_or("PORT", 8000_u16);

        let config = Self {
            listen: SocketAddr::new(
                host.parse::<IpAddr>()
                    .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port,
            ),
            // §2 — `SERVICEBUS_`, without the underscore the rest of the
            // estate uses. Preserved so an existing `buswatch.env` keeps
            // working; renaming it would be tidier and would break a file
            // somebody has on their machine.
            connection_string: reader
                .string_or("SERVICEBUS_CONNECTION_STRING", DEFAULT_CONNECTION_STRING),
            queue_names: reader.list("BUSWATCH_QUEUE_NAMES"),
            peek_count: reader
                .optional_positive_integer("BUSWATCH_PEEK_COUNT")
                .unwrap_or(DEFAULT_PEEK_COUNT),
            detail_search_limit: reader
                .optional_positive_integer("BUSWATCH_DETAIL_SEARCH_LIMIT")
                .unwrap_or(DEFAULT_DETAIL_SEARCH_LIMIT),
            // The Python's spelling, kept for the reason
            // `SERVICEBUS_CONNECTION_STRING` is: an existing `buswatch.env`
            // has this name in it, and a tidier one would silently ignore
            // a value somebody set deliberately after hitting §6.1's quota.
            parallelism: reader
                .optional_positive_integer("BUSWATCH_MAX_PARALLEL_QUEUE_FETCHES")
                .unwrap_or(DEFAULT_PARALLELISM),
        };

        reader.finish()?;
        Ok(config)
    }
}

/// The queue names in an emulator configuration (§2.2).
///
/// Reads `UserConfig.Namespaces[].Queues[].Name`. Used when
/// `BUSWATCH_QUEUE_NAMES` is blank, so a developer who adds a queue to the
/// emulator sees it without editing a second file.
///
/// # Examples
///
/// ```
/// use buswatch::config::discover_queues;
///
/// let json = r#"{"UserConfig":{"Namespaces":[{"Queues":[
///     {"Name":"one"},{"Name":"two"}
/// ]}]}}"#;
///
/// assert_eq!(discover_queues(json), ["one", "two"]);
/// ```
///
/// Anything unreadable yields nothing rather than failing: BusWatch with no
/// queues is a page saying so, which is more useful than a process that
/// will not start.
///
/// ```
/// use buswatch::config::discover_queues;
///
/// assert!(discover_queues("not json").is_empty());
/// assert!(discover_queues("{}").is_empty());
/// ```
#[must_use]
pub fn discover_queues(emulator_config: &str) -> Vec<String> {
    let Ok(document) = serde_json::from_str::<serde_json::Value>(emulator_config) else {
        return Vec::new();
    };

    let Some(namespaces) = document
        .get("UserConfig")
        .and_then(|user| user.get("Namespaces"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    namespaces
        .iter()
        .filter_map(|namespace| namespace.get("Queues")?.as_array())
        .flatten()
        .filter_map(|queue| Some(queue.get("Name")?.as_str()?.to_string()))
        .collect()
}

/// The session-enabled queues in the emulator configuration.
///
/// A session-enabled queue cannot be read with a plain receiver, so BusWatch
/// has to know which ones they are before it peeks
/// (`spec/14-buswatch.md` §4.4). Reads
/// `UserConfig.Namespaces[].Queues[].Properties.RequiresSession`.
///
/// Never fails, for the reason [`discover_queues`] does not: a wrong answer
/// here costs a fallback, and refusing to start costs a developer an hour.
///
/// # Examples
///
/// ```
/// use buswatch::config::discover_session_queues;
///
/// let config = r#"{"UserConfig":{"Namespaces":[{"Queues":[
///     {"Name":"with","Properties":{"RequiresSession":true}},
///     {"Name":"without","Properties":{"RequiresSession":false}},
///     {"Name":"unsaid"}
/// ]}]}}"#;
///
/// let sessions = discover_session_queues(config);
///
/// assert!(sessions.contains("with"));
/// assert!(!sessions.contains("without"), "false is not a session queue");
/// assert!(!sessions.contains("unsaid"), "and neither is silence");
/// ```
#[must_use]
pub fn discover_session_queues(emulator_config: &str) -> BTreeSet<String> {
    let Ok(document) = serde_json::from_str::<serde_json::Value>(emulator_config) else {
        return BTreeSet::new();
    };

    let Some(namespaces) = document
        .get("UserConfig")
        .and_then(|user| user.get("Namespaces"))
        .and_then(serde_json::Value::as_array)
    else {
        return BTreeSet::new();
    };

    namespaces
        .iter()
        .filter_map(|namespace| namespace.get("Queues")?.as_array())
        .flatten()
        .filter_map(|queue| {
            let requires = queue
                .get("Properties")?
                .get("RequiresSession")?
                .as_bool()
                .unwrap_or(false);
            requires.then(|| queue.get("Name")?.as_str().map(ToString::to_string))?
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The emulator configuration the local stack actually uses.
    const EMULATOR: &str = include_str!("../../../local/ServiceBusEmulatorConfig.json");

    #[test]
    fn discovery_finds_every_queue_in_the_real_configuration() {
        // spec/14-buswatch.md §2.2 and §8.1 — against the real file, so a
        // queue added to the stack is a queue BusWatch shows.
        let queues = discover_queues(EMULATOR);

        assert!(queues.len() >= 9, "found {}: {queues:?}", queues.len());
        assert!(queues.contains(&"local-inthub-phw-transformer-ingress".to_string()));
        assert!(queues.contains(&"local-inthub-messagestore".to_string()));
        assert!(queues.contains(&"local-inthub-priority-messagequeue".to_string()));
    }

    #[test]
    fn the_real_configuration_names_exactly_two_non_session_queues() {
        // §4.4, against the real file — and this is the fact that made the
        // end-to-end test of `ihb-e2e` work: every other queue in the local
        // stack requires a session, so a plain receiver on one of them
        // fails rather than reading nothing.
        let sessions = discover_session_queues(EMULATOR);
        let all = discover_queues(EMULATOR);

        let plain: Vec<&String> = all.iter().filter(|q| !sessions.contains(*q)).collect();

        assert_eq!(
            plain,
            vec![
                &"local-inthub-mpi-mock-receiver".to_string(),
                &"local-inthub-messagestore".to_string(),
            ],
            "the non-session queues of spec/01 §4.1"
        );
    }

    #[test]
    fn discovery_never_fails_the_process() {
        // A BusWatch with no queues is a page saying so. A BusWatch that
        // will not start is a developer wondering why.
        for input in ["", "not json", "{}", "[]", r#"{"UserConfig":{}}"#] {
            assert!(discover_queues(input).is_empty(), "{input:?}");
        }
    }

    #[test]
    fn the_connection_variable_keeps_its_own_spelling() {
        // §2 — `SERVICEBUS_`, not `SERVICE_BUS_`. Renaming it would be
        // tidier and would break an existing `buswatch.env`.
        let config =
            Config::from_pairs([("SERVICEBUS_CONNECTION_STRING", "Endpoint=sb://x;")]).unwrap();
        assert_eq!(config.connection_string, "Endpoint=sb://x;");

        // The estate's spelling is deliberately *not* read.
        let ignored =
            Config::from_pairs([("SERVICE_BUS_CONNECTION_STRING", "Endpoint=sb://x;")]).unwrap();
        assert_eq!(ignored.connection_string, DEFAULT_CONNECTION_STRING);
    }

    #[test]
    fn the_defaults_are_the_ones_in_section_nine_one() {
        let config = Config::from_pairs([] as [(&str, &str); 0]).unwrap();

        assert_eq!(config.peek_count, DEFAULT_PEEK_COUNT);
        assert_eq!(config.detail_search_limit, DEFAULT_DETAIL_SEARCH_LIMIT);
        assert!(config.queue_names.is_empty());
        assert!(
            config
                .connection_string
                .contains("UseDevelopmentEmulator=true")
        );
    }

    #[test]
    fn explicit_queue_names_replace_discovery() {
        let config = Config::from_pairs([("BUSWATCH_QUEUE_NAMES", "a, b ,c")]).unwrap();

        assert_eq!(config.queue_names, ["a", "b", "c"]);
    }
}
