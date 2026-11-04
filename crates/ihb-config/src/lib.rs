//! Reading a service's configuration out of the environment.
//!
//! Implements `spec/02-configuration.md` §1 and §10.
//!
//! Every Integration Hub service is configured by environment variables and
//! nothing else — no files, no defaults baked into an image, no service
//! discovery. This crate is the one place that turns a variable into a
//! value, so the rules that turn out to matter are stated once.
//!
//! # This crate is a facade
//!
//! The implementation lives in [`env_reader`], which was extracted from
//! this workspace and published: none of the three rules below is specific
//! to the Integration Hub. What stays here is which clauses they implement,
//! and the one place where §1.5 asks for something the general crate does
//! not do by default.
//!
//! # Three rules worth knowing before reading anything else
//!
//! **Blank is absent.** A variable set to `""`, or to spaces, means the
//! same thing as an unset variable. Container definitions produce blank
//! values routinely — a templated value whose source was empty, an `.env`
//! line with nothing after the `=` — and treating those as "configured, to
//! the empty string" produces failures a long way from their cause. The
//! Python does this in most places; here it is uniform, including for
//! integers, where the Python would have called `int("")` and crashed.
//!
//! **Every problem is reported at once.** [`Reader`] accumulates errors
//! rather than returning the first, so a service with four missing
//! variables says so once instead of over four restarts.
//!
//! **A secret never prints.** [`Secret`] holds one and renders as
//! `<redacted>` through both [`Debug`] and [`std::fmt::Display`], because a
//! configuration struct ends up in a log line eventually and a connection
//! string with a shared access key in it must not be what is there.
//!
//! # The one divergence from the general crate
//!
//! `spec/02-configuration.md` §1.5 fixes booleans as the **union of what
//! the Python's two disagreeing readers accepted**: `false`, `0`, `no` and
//! `off` are false, and *any other non-blank value is true*. So
//! `HL7_LOG_REDACTION_ENABLED=disabled` is true, and a deployment carrying
//! that value depends on it.
//!
//! [`env_reader`] reads booleans strictly by default, for the good reason
//! that "not false-ish, therefore true" is a footgun in new code. This
//! workspace wants the lopsided rule, so every call site here uses
//! [`Reader::lenient_bool_or`] and [`parse_bool`] — named, rather
//! than aliased back to `bool_or`, so the lopsidedness is visible where it
//! is relied on.
//!
//! # Examples
//!
//! ```
//! use ihb_config::Reader;
//!
//! let mut reader = Reader::from_pairs([
//!     ("WORKFLOW_ID", "phw-to-mpi"),
//!     ("PORT", "2575"),
//!     ("HL7_VERSION", "   "), // blank: the same as absent
//! ]);
//!
//! let workflow: String = reader.required_string("WORKFLOW_ID");
//! let port: u16 = reader.integer_or("PORT", 2575);
//! let version: Option<String> = reader.optional_string("HL7_VERSION");
//!
//! reader.finish().expect("the configuration is complete");
//! assert_eq!(workflow, "phw-to-mpi");
//! assert_eq!(port, 2575);
//! assert_eq!(version, None);
//! ```

#![forbid(unsafe_code)]
// A test asserts; asserting is what `unwrap` and `panic` are for. The lints
// stay on for everything the container actually runs.
// See spec/17-testing-and-quality.md §2.3.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub use env_reader::{
    Error as ConfigError, Errors as ConfigErrors, Reader, Secret, parse_bool_lenient as parse_bool,
    parse_csv, present,
};

/// What `MESSAGE_STORE_ENABLED` and `MESSAGE_STORE_QUEUE_NAME` add up to.
///
/// `spec/02-configuration.md` §2.1.2.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageStore {
    /// The queue to write to: set only when the store is switched on **and**
    /// a queue name is configured.
    pub queue: Option<String>,
    /// A queue name that is configured but ignored because the store is
    /// switched off. §2.1.2c: the changed default makes this easy to produce
    /// by accident, so a service warns about it at startup rather than
    /// quietly writing no audit trail.
    pub unused_queue: Option<String>,
}

/// Read the message-store settings.
///
/// `MESSAGE_STORE_ENABLED` defaults to **false** (upstream `387b20a`). Both
/// variables are always read, so a blank or malformed one is reported
/// alongside every other configuration problem rather than only when the
/// store happens to be on.
///
/// # Examples
///
/// ```
/// use ihb_config::{Reader, message_store};
///
/// let reader = Reader::from_pairs([("MESSAGE_STORE_QUEUE_NAME", "store")]);
/// let store = message_store(&reader);
/// assert_eq!(store.queue, None, "off by default");
/// assert_eq!(store.unused_queue.as_deref(), Some("store"), "and worth a warning");
///
/// let reader = Reader::from_pairs([
///     ("MESSAGE_STORE_QUEUE_NAME", "store"),
///     ("MESSAGE_STORE_ENABLED", "true"),
/// ]);
/// assert_eq!(message_store(&reader).queue.as_deref(), Some("store"));
/// ```
#[must_use]
pub fn message_store(reader: &Reader) -> MessageStore {
    let enabled = reader.lenient_bool_or("MESSAGE_STORE_ENABLED", false);
    let queue = reader.optional_string("MESSAGE_STORE_QUEUE_NAME");
    if enabled {
        MessageStore {
            queue,
            unused_queue: None,
        }
    } else {
        MessageStore {
            queue: None,
            unused_queue: queue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_is_off_unless_enabled() {
        // spec/02-configuration.md §2.1.2 — the default flipped upstream.
        let queue = ("MESSAGE_STORE_QUEUE_NAME", "store");
        for (enabled, on) in [
            (None, false),
            (Some("false"), false),
            (Some("0"), false),
            (Some("true"), true),
            (Some("TRUE"), true),
            (Some("1"), true),
        ] {
            let mut pairs = vec![queue];
            if let Some(value) = enabled {
                pairs.push(("MESSAGE_STORE_ENABLED", value));
            }
            let store = message_store(&Reader::from_pairs(pairs));
            assert_eq!(store.queue.is_some(), on, "{enabled:?}");
            // §2.1.2c — exactly one of the two carries the name.
            assert_eq!(store.unused_queue.is_some(), !on, "{enabled:?}");
        }
    }

    #[test]
    fn enabled_without_a_queue_is_no_store_and_no_warning() {
        let store = message_store(&Reader::from_pairs([("MESSAGE_STORE_ENABLED", "true")]));
        assert_eq!(store, MessageStore::default());
    }

    #[test]
    fn blank_is_absent() {
        // spec/02-configuration.md §1.1 — the rule every reader goes
        // through, asserted here because it is this workspace's clause.
        assert_eq!(present(Some("")), None);
        assert_eq!(present(Some(" \t\n")), None);
        assert_eq!(present(None), None);
    }

    #[test]
    fn a_blank_integer_is_absent_rather_than_a_parse_failure() {
        // spec/02-configuration.md §1.1 — the difference from the Python,
        // which would have called `int("")`.
        let mut reader = Reader::from_pairs([("A", "  ")]);
        assert_eq!(reader.optional_integer::<u32>("A"), None);
        assert!(reader.finish().is_ok());
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        // spec/02-configuration.md §1.4
        let mut reader = Reader::from_pairs([("PORT", "two")]);
        let _ = reader.required_string("WORKFLOW_ID");
        let _ = reader.required_string("MICROSERVICE_ID");
        let _: u16 = reader.integer_or("PORT", 2575);

        let errors = reader.finish().unwrap_err();
        assert_eq!(errors.len(), 3);
        assert_eq!(errors.to_string().lines().count(), 3);
    }

    #[test]
    fn booleans_keep_the_lopsided_python_rule() {
        // spec/02-configuration.md §1.5 — THE reason this facade renames
        // rather than re-exports. A deployment carrying
        // `HL7_LOG_REDACTION_ENABLED=disabled` depends on this being true,
        // and `env-reader`'s strict `bool_or` would answer differently.
        for text in ["false", "FALSE", "0", "no", "off"] {
            assert_eq!(parse_bool(Some(text)), Some(false), "{text}");
        }
        for text in ["true", "1", "yes", "on", "disabled", "nope"] {
            assert_eq!(parse_bool(Some(text)), Some(true), "{text}");
        }

        let reader = Reader::from_pairs([("FLAG", "disabled")]);
        assert!(
            reader.lenient_bool_or("FLAG", false),
            "§1.5: anything not false-ish is true"
        );
    }

    #[test]
    fn a_secret_never_prints() {
        // spec/02-configuration.md §10.4
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Config {
            namespace: String,
            connection_string: Secret,
        }

        let printed = format!(
            "{:?}",
            Config {
                namespace: "local-inthub".into(),
                connection_string: Secret::new("Endpoint=sb://x;SharedAccessKey=hunter2"),
            }
        );

        assert!(printed.contains("local-inthub"));
        assert!(!printed.contains("hunter2"));
    }

    #[test]
    fn a_list_drops_empty_elements() {
        // spec/02-configuration.md §1.7
        assert_eq!(
            parse_csv(Some("245,212, 192 ,224")),
            ["245", "212", "192", "224"]
        );
        assert_eq!(parse_csv(Some(" , , ")), [] as [std::string::String; 0]);
    }
}
