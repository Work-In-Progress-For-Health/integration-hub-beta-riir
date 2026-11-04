#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! Core reference data: RISP → MPI.
//!
//! Implements `spec/09-transformers.md` §8.
//!
//! Six coded `PID` fields arrive in the source system's code system and
//! leave in MPI's. The translation table lives in WRDS; this transformer
//! fetches it once per message and translates client-side
//! (`spec/09-transformers.md` §8.5), changing **only** those six fields and
//! leaving every other byte of the message alone.
//!
//! Where it fits: one hop between the `hl7-server` ingress queue and the MPI
//! egress queue, run by [`ihb_transformer::main_loop::run`]. It is the only
//! transformer that makes a network call per message (to WRDS, the Welsh
//! Reference Data Service), which is why the lookup is behind the [`Rows`]
//! trait and why a failure must be an error rather than a pass-through.
//!
//! # Fail closed
//!
//! If WRDS cannot be reached the message is abandoned (§8.6), not forwarded
//! with untranslated codes. A wrong code reaching MPI looks like a valid one.

pub mod config;

use hl7_2::Message;
use ihb_transformer::{TransformError, Transformer};
use wrds_service::{Row, to_code};

use crate::config::WrdsConfig;

/// Trigger events this transformer acts on (§8.1): `MSH-9.2`, trimmed.
///
/// Anything else is returned unchanged with no WRDS call. The ingress queue
/// is expected to carry nothing else, so this is defence in depth, not the
/// routing mechanism.
pub const SUPPORTED_TRIGGERS: [&str; 3] = ["A28", "A31", "A40"];

/// Each translated field as `(path, WRDS type string)` (§8.2).
///
/// The type string is matched against the `type` column of the WRDS rows and
/// is case- and spelling-sensitive (`Marital Status` has a space). Paths for
/// `PID-15.1` and below name the component, not the field, so a `CE.2` free
/// text description beside the code survives the translation.
pub const FIELDS: [(&str, &str); 6] = [
    ("PID-8", "Sex"),
    ("PID-5.5", "Title"),
    ("PID-15.1", "Language"),
    ("PID-16.1", "Marital Status"),
    ("PID-17.1", "Religion"),
    ("PID-22.1", "Ethnicity"),
];

/// Where the translation rows come from.
///
/// A trait so the mapping is tested without a network, and so a test can
/// count the calls §8.5 limits to one.
pub trait Rows: Send {
    /// The rows for a pair of systems (§8.5).
    ///
    /// # Errors
    ///
    /// The lookup's failure, which §8.6 turns into an abandoned message.
    fn fetch(
        &self,
        lookup_table: &str,
        from_system: &str,
        to_system: &str,
    ) -> Result<Vec<Row>, wrds_service::Error>;
}

/// The real source: one `GetResultSet` (§8.5).
///
/// `true` is `ExactMatch`. The call blocks the calling thread, so the binary
/// runs on a multi-thread runtime (see `main.rs`).
impl Rows for wrds_service::Client {
    fn fetch(
        &self,
        lookup_table: &str,
        from_system: &str,
        to_system: &str,
    ) -> Result<Vec<Row>, wrds_service::Error> {
        self.get_result_set_blocking(
            lookup_table,
            &[("FromSystem", from_system), ("ToSystem", to_system)],
            &["FromCode", "type", "ToCode"],
            true,
        )
    }
}

/// The core reference mapping (§8).
///
/// Stateless between messages: the rows are fetched afresh for each one
/// (§8.5), so a change to the WRDS table is seen on the next message and no
/// cache can serve a stale code.
#[derive(Debug)]
pub struct CoreReference<R = wrds_service::Client> {
    /// Where rows come from; a real client, or a fake in tests.
    source: R,
    /// `WRDS_LOOKUP_TABLE_NAME`: the WRDS table the `GetResultSet` runs against.
    lookup_table: String,
}

impl CoreReference {
    /// A transformer over a real WRDS client.
    ///
    /// # Errors
    ///
    /// The client's [`wrds_service::Error::Tls`], for a certificate that
    /// cannot be loaded.
    pub fn from_config(config: &WrdsConfig) -> Result<Self, wrds_service::Error> {
        let client = wrds_service::Client::new(
            config.endpoint_url.clone(),
            config.timeout,
            config.client_cert_path.as_deref(),
            config.local_fixture_path.clone(),
        )?;
        Ok(Self::new(client, config.lookup_table.clone()))
    }
}

impl<R: Rows> CoreReference<R> {
    /// A transformer over any source of rows, for tests and alternative
    /// back ends. `lookup_table` is passed to every [`Rows::fetch`].
    pub const fn new(source: R, lookup_table: String) -> Self {
        Self {
            source,
            lookup_table,
        }
    }

    /// Translate the six fields of one message (§8.2–§8.5).
    ///
    /// Returns the *re-parsed flat* message on every path past the trigger
    /// check, not a clone of the input; the trigger check returns the input
    /// itself so an out-of-scope message is byte-identical (§8.1).
    ///
    /// # Errors
    ///
    /// [`TransformError::Parse`] if the message cannot be re-parsed flat;
    /// [`TransformError::Rejected`] when the WRDS lookup fails (§8.6). The
    /// text names the failure class, never a field value.
    fn map(&self, message: &Message) -> Result<Message, TransformError> {
        // §8.3 — flat, so `PID` is found whatever structure carried it. The
        // Python's `.pid` shortcut returned an empty proxy for the nested
        // `PATIENT` group of an A40 (`ADT_A39`), so every field read empty.
        let mut flat = ihb_validation::parse(&message.to_er7())
            .map_err(|error| TransformError::Parse(error.to_string()))?;

        let trigger = ihb_fields::read(&flat, "MSH-9.2");
        tracing::info!(trigger = trigger.trim(), "processing a message");
        if !SUPPORTED_TRIGGERS.contains(&trigger.trim()) {
            return Ok(message.clone()); // §8.1 — not ours; untouched.
        }

        if !flat.segments().any(|segment| segment.name == "PID") {
            tracing::warn!("PID segment not found, skipping mapping");
            return Ok(flat);
        }

        // §8.4 — an empty field, or the explicit null, takes no part.
        let wanted: Vec<(&str, &str, String)> = FIELDS
            .iter()
            .filter(|(path, _)| !ihb_fields::is_explicit_null(&flat, path))
            .map(|(path, kind)| (*path, *kind, ihb_fields::read(&flat, path)))
            .filter(|(_, _, value)| !value.is_empty())
            .collect();
        if wanted.is_empty() {
            return Ok(flat); // §8.5 — no call at all.
        }

        let from_system = ihb_fields::read(&flat, "MSH-3.1");
        let to_system = ihb_fields::read(&flat, "MSH-5.1");
        tracing::info!(
            from_system,
            to_system,
            lookup_table = %self.lookup_table,
            fields = wanted.len(),
            "requesting WRDS mappings"
        );

        let rows = self
            .source
            .fetch(&self.lookup_table, &from_system, &to_system)
            // `Rejected`, not a silent pass-through: forwarding untranslated
            // codes would look valid to MPI (§8.6).
            .map_err(|error| {
                TransformError::Rejected(format!("Core reference lookup failed: {error}"))
            })?;

        for (path, kind, value) in wanted {
            // §8.4 — no match, or an empty `ToCode`, clears the field.
            flat.set(path, &to_code(&rows, &value, kind))?;
        }
        Ok(flat)
    }
}

impl<R: Rows> Transformer for CoreReference<R> {
    /// `CoreReference` (§8.8); appears in audit text and span names.
    fn name(&self) -> &'static str {
        "CoreReference"
    }

    fn transform(&mut self, message: &Message) -> Result<Message, TransformError> {
        self.map(message)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// The Python's `_build_message`.
    fn message(msh_9: &str, pid: &[(usize, &str)]) -> String {
        let last = pid.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let fields: Vec<&str> = (1..=last)
            .map(|n| pid.iter().find(|(i, _)| *i == n).map_or("", |(_, v)| *v))
            .collect();
        format!(
            "MSH|^~\\&|349|349|100|100|20250624162400||{msh_9}|123456789|P|2.5|||NE|NE\rPID|{}\r",
            fields.join("|")
        )
    }

    fn all_populated() -> String {
        message(
            "ADT^A28",
            &[
                (1, "1"),
                (2, "1000000001^^^^NH"),
                (3, "1000000001^^^^NH"),
                (5, "TEST^TEST^^^Mr."),
                (7, "20000101000000"),
                (8, "1"),
                (15, "EN^English"),
                (16, "11^Single"),
                (17, "22^SomeReligion"),
                (22, "33^SomeEthnicity"),
            ],
        )
    }

    fn rows() -> Vec<Row> {
        [
            ("1", "Sex", "M"),
            ("Mr.", "Title", "Mr"),
            ("EN", "Language", "eng"),
            ("11", "Marital Status", "S"),
            ("22", "Religion", "REL22"),
            ("33", "Ethnicity", "ETH33"),
        ]
        .iter()
        .map(|(from, kind, to)| {
            Row::from([
                ("FromCode".to_string(), (*from).to_string()),
                ("Type".to_string(), (*kind).to_string()),
                ("ToCode".to_string(), (*to).to_string()),
            ])
        })
        .collect()
    }

    /// A source that records every call.
    struct Fake {
        rows: Result<Vec<Row>, String>,
        calls: Mutex<Vec<(String, String, String)>>,
    }

    impl Fake {
        fn with(rows: Vec<Row>) -> Self {
            Self {
                rows: Ok(rows),
                calls: Mutex::default(),
            }
        }
        fn failing() -> Self {
            Self {
                rows: Err("boom".to_string()),
                calls: Mutex::default(),
            }
        }
    }

    impl Rows for &Fake {
        fn fetch(
            &self,
            table: &str,
            from: &str,
            to: &str,
        ) -> Result<Vec<Row>, wrds_service::Error> {
            self.calls
                .lock()
                .unwrap()
                .push((table.to_string(), from.to_string(), to.to_string()));
            self.rows.clone().map_err(wrds_service::Error::Connection)
        }
    }

    fn run(fake: &Fake, input: &str) -> Result<Message, TransformError> {
        let parsed = ihb_validation::parse(input).unwrap();
        CoreReference::new(fake, "FioranoCodeTranslation".to_string()).transform(&parsed)
    }

    fn read(message: &Message, path: &str) -> String {
        ihb_fields::read(message, path)
    }

    #[test]
    fn all_six_fields_are_translated() {
        // §8.2.
        let fake = Fake::with(rows());
        let out = run(&fake, &all_populated()).unwrap();

        assert_eq!(read(&out, "PID-8"), "M");
        assert_eq!(read(&out, "PID-5.5"), "Mr");
        assert_eq!(read(&out, "PID-15.1"), "eng");
        assert_eq!(read(&out, "PID-16.1"), "S");
        assert_eq!(read(&out, "PID-17.1"), "REL22");
        assert_eq!(read(&out, "PID-22.1"), "ETH33");
    }

    #[test]
    fn only_the_component_is_replaced_not_the_whole_field() {
        // §8.2 — `CE.2`'s free text survives.
        let fake = Fake::with(rows());
        let out = run(&fake, &all_populated()).unwrap();

        assert_eq!(read(&out, "PID-15.2"), "English");
        assert_eq!(read(&out, "PID-16.2"), "Single");
        assert_eq!(read(&out, "PID-5.1"), "TEST");
    }

    #[test]
    fn unrelated_fields_are_untouched() {
        // §8.2 — compare the whole message, with only the six edits undone.
        let fake = Fake::with(rows());
        let out = run(&fake, &all_populated()).unwrap();

        assert_eq!(read(&out, "PID-3"), "1000000001^^^^NH");
        assert_eq!(read(&out, "PID-7"), "20000101000000");
        assert_eq!(read(&out, "MSH-10"), "123456789");
        assert_eq!(read(&out, "PID-5.4"), "");
    }

    #[test]
    fn one_lookup_with_the_systems_from_msh() {
        // §8.5.
        let fake = Fake::with(rows());
        run(&fake, &all_populated()).unwrap();

        let calls = fake.calls.lock().unwrap();
        assert_eq!(
            *calls,
            [(
                "FioranoCodeTranslation".to_string(),
                "349".to_string(),
                "100".to_string()
            )]
        );
    }

    #[test]
    fn all_fields_empty_skips_the_call() {
        // §8.5.
        let fake = Fake::failing();
        let input = message(
            "ADT^A31",
            &[
                (1, "1"),
                (3, "1000000001^^^^NH"),
                (5, "TEST^TEST"),
                (7, "20000101000000"),
            ],
        );
        let out = run(&fake, &input).unwrap();

        assert!(fake.calls.lock().unwrap().is_empty());
        assert_eq!(
            out.to_er7(),
            ihb_validation::parse(&input).unwrap().to_er7()
        );
    }

    #[test]
    fn an_explicit_null_is_left_alone_and_makes_no_call() {
        // §8.4.
        let fake = Fake::failing();
        let input = message(
            "ADT^A40",
            &[
                (1, "1"),
                (3, "1000000001^^^^NH"),
                (5, "TEST^TEST"),
                (8, "\"\""),
            ],
        );
        let out = run(&fake, &input).unwrap();

        assert!(fake.calls.lock().unwrap().is_empty());
        assert!(ihb_fields::is_explicit_null(&out, "PID-8"));
    }

    #[test]
    fn no_matching_row_clears_the_field() {
        // §8.4.
        let fake = Fake::with(vec![]);
        let out = run(&fake, &all_populated()).unwrap();

        assert_eq!(read(&out, "PID-8"), "");
        assert_eq!(read(&out, "PID-5.5"), "");
    }

    #[test]
    fn an_empty_to_code_clears_the_field() {
        // §8.4.
        let mut table = rows();
        table[0].insert("ToCode".to_string(), String::new());
        let fake = Fake::with(table);
        let out = run(&fake, &all_populated()).unwrap();

        assert_eq!(read(&out, "PID-8"), "");
        assert_eq!(
            read(&out, "PID-16.1"),
            "S",
            "the others are still translated"
        );
    }

    #[test]
    fn an_out_of_scope_trigger_passes_through_without_a_call() {
        // §8.1.
        let fake = Fake::failing();
        let input = message(
            "ADT^A04",
            &[
                (1, "1"),
                (3, "1000000001^^^^NH"),
                (5, "TEST^TEST"),
                (8, "1"),
            ],
        );
        let out = run(&fake, &input).unwrap();

        assert!(fake.calls.lock().unwrap().is_empty());
        assert_eq!(read(&out, "PID-8"), "1");
    }

    #[test]
    fn a_message_without_pid_is_returned_unchanged() {
        // §8.3.
        let fake = Fake::failing();
        let input = "MSH|^~\\&|349|349|100|100|20250624162400||ADT^A28|123456789|P|2.5|||NE|NE\r";
        let out = run(&fake, input).unwrap();

        assert!(fake.calls.lock().unwrap().is_empty());
        assert_eq!(out.to_er7(), ihb_validation::parse(input).unwrap().to_er7());
    }

    #[test]
    fn a_merge_trigger_with_a_nested_pid_is_translated() {
        // §8.3 — the case the Python's `.pid` shortcut got silently wrong.
        let fake = Fake::with(rows());
        let input = "MSH|^~\\&|349|349|100|100|20250624162400||ADT^A40^ADT_A39|123456789|P|2.5|||NE|NE\r\
                     PID|1||1000000001^^^^NH||TEST^TEST||20000101|1\r\
                     MRG|1000000002^^^^NH\r";
        let out = run(&fake, input).unwrap();

        assert_eq!(read(&out, "PID-8"), "M");
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_lookup_failure_abandons_the_message() {
        // §8.6 — never forward untranslated codes.
        let fake = Fake::failing();
        let error = run(&fake, &all_populated()).unwrap_err();

        assert!(matches!(error, TransformError::Rejected(_)));
        assert!(
            error.to_string().contains("Core reference lookup failed"),
            "{error}"
        );
    }

    #[test]
    fn the_error_does_not_carry_a_field_value() {
        // spec/index.md §0.8 — a failure names a class, never a value.
        let fake = Fake::failing();
        let error = run(&fake, &all_populated()).unwrap_err().to_string();
        assert!(!error.contains("1000000001"), "{error}");
        assert!(!error.contains("SomeReligion"), "{error}");
    }

    #[test]
    fn the_name_is_core_reference() {
        // §8.8.
        let fake = Fake::with(vec![]);
        let transformer = CoreReference::new(&fake, String::new());
        assert_eq!(Transformer::name(&transformer), "CoreReference");
    }
}
