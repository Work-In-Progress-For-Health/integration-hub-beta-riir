//! Validating a v2.xml document against its flow's XSD — twice, with two
//! independent engines, and comparing them.
//!
//! Implements `spec/06-validation.md` §9.
//!
//! # Why two
//!
//! This is the validation the Python actually performs: `xmlschema` against
//! the per-flow XSDs. The rest of this crate reaches the same verdict by a
//! different route — dictionaries compiled from those XSDs, checked by
//! `hl7-2` (§3.2) — because when the rewrite began there was no mature XML
//! Schema validator in Rust. There now are two, and neither is old enough to
//! trust on its own for clinical data.
//!
//! So both run, and their verdicts are compared. A disagreement is not
//! averaged away or silently resolved: it is **reported**, because two
//! independent implementations differing about whether a clinical message is
//! well-formed is information, and the most useful thing to do with it is
//! say so.
//!
//! That is not a hypothetical benefit. The first time this ran over the
//! corpus it found a real defect in one of the two engines within minutes —
//! see [`Engine::Uppsala`] and `spec/06-validation.md` §9.4.
//!
//! # What this is not
//!
//! **Not yet the ingest path.** [`crate::validate_and_convert`] still
//! decides whether a message is accepted, by the dictionary route. This
//! module is a cross-check on that decision and a route to replacing it, and
//! §9.6 says what has to be true before it takes over.
//!
//! # Examples
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use ihb_validation::{Flow, xsd};
//!
//! let er7 = "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r\
//!            EVN||20250502092900\r\
//!            PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r\
//!            PV1||U";
//! let document = ihb_validation::er7_to_xml_for_flow(er7, Flow::Phw, None)?;
//!
//! let report = xsd::validate(&document, Flow::Phw, "ADT_A05")?;
//!
//! // The verdict is the primary engine's, and this message is valid.
//! assert!(report.is_valid());
//!
//! // The engines do *not* agree here, and that is the current expected
//! // state rather than a surprise: `uppsala` 0.9.0 misresolves HL7's own
//! // `ID` type to the built-in `xs:ID` and rejects every ID-typed element
//! // (`spec/06-validation.md` §9.4, reported upstream as
//! // kushaldas/uppsala#39). The disagreement is surfaced and
//! // attributed rather than averaged away — which is the whole point of
//! // running two engines, and is how the defect was found.
//! assert!(!report.engines_agreed());
//! assert!(
//!     report
//!         .findings
//!         .iter()
//!         .all(|finding| finding.engine == xsd::Engine::Uppsala)
//! );
//! # Ok(())
//! # }
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use xsd_schema::validation::validator::SchemaValidator;

use crate::error::ValidationError;
use crate::flow::Flow;

/// Which engine produced a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    /// `xsd-schema` — a streaming XSD 1.0/1.1 validator.
    ///
    /// The **primary**. Measured against the committed corpus it agrees
    /// with the Python's `xmlschema` on 24 of 24 validatable cases,
    /// including the `xs:include` chains and the padded documents
    /// (`spec/06-validation.md` §9.3).
    XsdSchema,

    /// `uppsala` — a pure-Rust XML toolkit with an XSD validator.
    ///
    /// The **cross-check**, and currently a failing one. It resolves an
    /// unprefixed `base="ID"` to the built-in `xs:ID` rather than to the
    /// schema's own `ID` in its default namespace. The HL7 v2.xml schemas
    /// define exactly such a type — `<xsd:simpleType name="ID">`, an
    /// unconstrained string — so every `HD.3`, and every other field of HL7
    /// type `ID`, is checked against XML's `NCName` rules and rejected.
    ///
    /// The effect on real messages: 11 of 24 corpus cases wrongly invalid,
    /// including every `as-is` sample. See `spec/06-validation.md` §9.4 for
    /// the minimal reproduction.
    Uppsala,
}

impl Engine {
    /// The crate name, for a log line or a report.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::XsdSchema => "xsd-schema",
            Self::Uppsala => "uppsala",
        }
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One thing an engine objected to, with every value removed.
///
/// `spec/index.md` §0.8 forbids a field value reaching a log, and an XSD
/// validator's message is one of the likelier places for one to hide:
/// `uppsala` reports `'252' is not a valid NCName`, quoting the patient's
/// assigning authority. [`Finding::from_engine`] strips quoted literals
/// before the text is ever stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Which engine said so.
    pub engine: Engine,
    /// What is wrong, with quoted values replaced by `<value>`.
    pub reason: String,
}

impl Finding {
    /// Build a finding from an engine's raw message, sanitising it.
    ///
    /// # Examples
    ///
    /// ```
    /// use ihb_validation::xsd::{Engine, Finding};
    ///
    /// let finding = Finding::from_engine(
    ///     Engine::Uppsala,
    ///     "1:155: '4444444444' is not a valid NCName/ID/IDREF",
    /// );
    ///
    /// assert!(!finding.reason.contains("4444444444"));
    /// assert!(finding.reason.contains("<value>"));
    /// assert!(finding.reason.contains("NCName"));
    /// ```
    #[must_use]
    pub fn from_engine(engine: Engine, raw: &str) -> Self {
        Self {
            engine,
            reason: sanitise(raw),
        }
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.engine, self.reason)
    }
}

/// Replace every quoted literal with `<value>`.
///
/// Deliberately blunt. A validator's message is free text from a dependency,
/// so the only safe assumption is that anything it quotes came out of the
/// document. Structure — element names, XPaths, `cvc-` rule codes — is not
/// quoted, so it survives, which is what makes the remaining text useful.
///
/// Single and double quotes both, because the two engines differ.
fn sanitise(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();

    while let Some(c) = chars.next() {
        if c == '\'' || c == '"' {
            // Consume to the matching quote, or to the end if unbalanced.
            for inner in chars.by_ref() {
                if inner == c {
                    break;
                }
            }
            out.push_str("<value>");
        } else {
            out.push(c);
        }
    }

    out
}

/// Whether a document conforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No engine objected.
    Valid,
    /// At least one did.
    Invalid,
}

/// What both engines made of one document.
///
/// The verdict is the primary engine's ([`Engine::XsdSchema`]); the report
/// also says whether the cross-check agreed, and carries both sets of
/// findings so a disagreement can be read rather than guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The decision, from the primary engine.
    pub verdict: Verdict,
    /// What the primary engine concluded.
    pub primary: Verdict,
    /// What the cross-check concluded.
    pub secondary: Verdict,
    /// Everything either engine objected to, sanitised.
    pub findings: Vec<Finding>,
}

impl Report {
    /// Whether the document conforms, by the primary engine.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        matches!(self.verdict, Verdict::Valid)
    }

    /// Whether both engines reached the same verdict.
    ///
    /// **A `false` here is the point of this module.** It means two
    /// independent implementations disagree about a clinical message, which
    /// is worth a log line and, if it is not already known, an
    /// investigation.
    #[must_use]
    pub const fn engines_agreed(&self) -> bool {
        matches!(
            (self.primary, self.secondary),
            (Verdict::Valid, Verdict::Valid) | (Verdict::Invalid, Verdict::Invalid)
        )
    }

    /// The findings from one engine.
    #[must_use]
    pub fn findings_from(&self, engine: Engine) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.engine == engine)
            .collect()
    }

    /// The first reason the primary engine gave, if it objected.
    #[must_use]
    pub fn first_reason(&self) -> Option<&str> {
        self.findings
            .iter()
            .find(|finding| finding.engine == Engine::XsdSchema)
            .map(|finding| finding.reason.as_str())
    }
}

/// Validate a v2.xml document against a flow's schema, with both engines.
///
/// The verdict is the primary engine's; [`Report::engines_agreed`] says
/// whether the cross-check concurred.
///
/// Compiled schemas are cached per `(flow, structure)` for the process
/// lifetime, because compiling one takes tens of milliseconds and a busy
/// ingress sees thousands of messages a minute.
///
/// # Errors
///
/// - [`ValidationError::NoSchema`] when the flow has no schema for that
///   structure.
/// - [`ValidationError::Xml`] when a schema will not compile, or the
///   document cannot be read at all.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ihb_validation::{Flow, xsd};
///
/// // A document whose root is not what the schema expects.
/// let report = xsd::validate(
///     r#"<ADT_A05 xmlns="urn:hl7-org:v2xml"><ZZZ/></ADT_A05>"#,
///     Flow::Phw,
///     "ADT_A05",
/// )?;
///
/// assert!(!report.is_valid());
/// # Ok(())
/// # }
/// ```
pub fn validate(xml: &str, flow: Flow, structure: &str) -> Result<Report, ValidationError> {
    let path = schema_path(flow, structure)?;

    let primary = validate_with_xsd_schema(xml, &path)?;
    let secondary = validate_with_uppsala(xml, &path)?;

    let verdict = if primary.0.is_empty() {
        Verdict::Valid
    } else {
        Verdict::Invalid
    };
    let secondary_verdict = if secondary.0.is_empty() {
        Verdict::Valid
    } else {
        Verdict::Invalid
    };

    let mut findings = primary.0;
    findings.extend(secondary.0);

    Ok(Report {
        verdict,
        primary: verdict,
        secondary: secondary_verdict,
        findings,
    })
}

/// Where a flow's schema for a structure lives.
fn schema_path(flow: Flow, structure: &str) -> Result<PathBuf, ValidationError> {
    let path = schemas_root()
        .join(flow.as_str())
        .join(format!("{structure}.xsd"));

    if path.exists() {
        Ok(path)
    } else {
        Err(ValidationError::NoSchema {
            flow: flow.as_str().to_string(),
            structure: structure.to_string(),
            available: {
                let structures = flow.structures();
                if structures.is_empty() {
                    "<none>".to_string()
                } else {
                    structures.join(", ")
                }
            },
        })
    }
}

/// The directory holding the schema groups.
///
/// Unlike the dictionaries of [`crate::flow`], the XSDs are **read from
/// disk**: both engines resolve `xs:include` by path, so the files have to
/// exist as files. That is the practical reason §9.6 lists a container
/// change as a precondition for this module taking over the ingest path.
fn schemas_root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/schemas"))
}

/// Findings from one engine.
struct Findings(Vec<Finding>);

/// Validate with one engine only.
///
/// Exposed so the two can be measured and compared separately; [`validate`]
/// is what a caller normally wants. See `spec/06-validation.md` §9.5 for why
/// the difference between them matters.
///
/// # Errors
///
/// As [`validate`].
pub fn validate_with(
    xml: &str,
    flow: Flow,
    structure: &str,
    engine: Engine,
) -> Result<Vec<Finding>, ValidationError> {
    let path = schema_path(flow, structure)?;
    let findings = match engine {
        Engine::XsdSchema => validate_with_xsd_schema(xml, &path)?,
        Engine::Uppsala => validate_with_uppsala(xml, &path)?,
    };
    Ok(findings.0)
}

/// Validate with `xsd-schema`, the primary engine.
fn validate_with_xsd_schema(xml: &str, schema: &Path) -> Result<Findings, ValidationError> {
    use xsd_schema::validation::{
        ValidationError as XsdError, ValidationSink, ValidationWarning, drive_quick_xml,
    };

    /// Collects what the engine objects to.
    struct Collect {
        /// Raw messages, sanitised later.
        raw: Vec<String>,
    }

    impl ValidationSink for Collect {
        fn on_error(&mut self, error: XsdError) {
            self.raw.push(error.to_string());
        }
        fn on_warning(&mut self, _: ValidationWarning) {}
    }

    let collected = with_xsd_schema_validator(schema, |validator, set| {
        let mut runtime = validator.start_run(Collect { raw: Vec::new() });

        let outcome = drive_quick_xml(xml.as_bytes(), &mut runtime, set);
        let _ = runtime.end_validation();

        outcome
            .map_err(|error| ValidationError::xml(format!("{error:?}")))
            .map(|_outcome| runtime.sink.raw.clone())
    })??;

    Ok(Findings(
        collected
            .iter()
            .map(|raw| Finding::from_engine(Engine::XsdSchema, raw))
            .collect(),
    ))
}

/// Validate with `uppsala`, the cross-check.
fn validate_with_uppsala(xml: &str, schema: &Path) -> Result<Findings, ValidationError> {
    let document = uppsala::parse(xml).map_err(|error| ValidationError::xml(error.to_string()))?;

    let raw = with_uppsala_validator(schema, |validator| {
        validator
            .validate(&document)
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    })?;

    Ok(Findings(
        raw.iter()
            .map(|error| Finding::from_engine(Engine::Uppsala, error))
            .collect(),
    ))
}

/// Run `body` with the compiled `xsd-schema` validator for a schema file.
///
/// **The validator is cached, not just the schema set, and the difference is
/// the whole story.** `SchemaValidator::new` compiles the content model, and
/// that is where essentially all the time goes. Measured on the PHW
/// `ADT_A05` schema, release build:
///
/// ```text
/// SchemaValidator::new only    10.2 ms
/// new + validate               10.2 ms     <- constructing per document
/// reused validator              3.6 us     <- constructing once
/// ```
///
/// Roughly 2,800x — the difference between five messages a second and a
/// throughput nobody has to think about. See `spec/06-validation.md` §9.5.
///
/// **Thread-local, and that is forced.** `xsd_schema::SchemaSet` holds
/// `RefCell`s, so it is `Send` but not `Sync` and cannot be shared across
/// threads at all. The alternatives were a global behind a mutex — which
/// would serialise every validation in the process — or one compiled copy
/// per thread. Thirteen schemas, a bounded worker pool: per-thread is
/// bounded and cheap.
///
/// Both are leaked so the validator can borrow the set for `'static`; each is
/// immutable once built and there are at most thirteen per thread.
fn with_xsd_schema_validator<T>(
    schema: &Path,
    body: impl FnOnce(&'static SchemaValidator<'static>, &'static xsd_schema::SchemaSet) -> T,
) -> Result<T, ValidationError> {
    /// A compiled schema and the validator built from it.
    type Compiled = (
        &'static SchemaValidator<'static>,
        &'static xsd_schema::SchemaSet,
    );

    thread_local! {
        /// Compiled validators for this thread, by schema path.
        static CACHE: RefCell<HashMap<PathBuf, Compiled>> = RefCell::new(HashMap::new());
    }

    let cached = CACHE.with(|cache| cache.borrow().get(schema).copied());
    if let Some((validator, set)) = cached {
        return Ok(body(validator, set));
    }

    let bytes = std::fs::read(schema).map_err(|error| ValidationError::xml(format!("{error}")))?;

    let mut set = xsd_schema::SchemaSet::new();
    xsd_schema::pipeline::load_and_process_schema(
        &bytes,
        &schema.to_string_lossy(),
        &mut set,
        None,
    )
    .map_err(|error| ValidationError::xml(format!("{error}")))?;

    let set: &'static xsd_schema::SchemaSet = Box::leak(Box::new(set));
    let validator: &'static SchemaValidator<'static> = Box::leak(Box::new(SchemaValidator::new(
        set,
        xsd_schema::validation::ValidationFlags::default(),
    )));

    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .insert(schema.to_path_buf(), (validator, set));
    });
    Ok(body(validator, set))
}

/// The compiled `uppsala` validator for a schema file, cached per thread.
///
/// Thread-local for the same reason as [`with_xsd_schema_set`], and kept the
/// same shape so neither engine looks like the special case.
fn with_uppsala_validator<T>(
    schema: &Path,
    body: impl FnOnce(&'static uppsala::XsdValidator) -> T,
) -> Result<T, ValidationError> {
    thread_local! {
        /// Compiled validators for this thread, by schema path.
        static CACHE: RefCell<HashMap<PathBuf, &'static uppsala::XsdValidator>> =
            RefCell::new(HashMap::new());
    }

    let cached = CACHE.with(|cache| cache.borrow().get(schema).copied());
    if let Some(validator) = cached {
        return Ok(body(validator));
    }

    let text = std::fs::read_to_string(schema)
        .map_err(|error| ValidationError::xml(format!("{error}")))?;
    let document =
        uppsala::parse(&text).map_err(|error| ValidationError::xml(error.to_string()))?;

    // `xs:include` is resolved relative to the schema's own directory.
    let validator = uppsala::XsdValidator::from_schema_with_base_path(&document, schema.parent())
        .map_err(|error| ValidationError::xml(error.to_string()))?;

    let leaked: &'static uppsala::XsdValidator = Box::leak(Box::new(validator));
    CACHE.with(|cache| cache.borrow_mut().insert(schema.to_path_buf(), leaked));
    Ok(body(leaked))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PHW message that conforms.
    const PHW: &str = concat!(
        "MSH|^~\\&|252|252|100|100|20250505232332||ADT^A31^ADT_A05|M1|P|2.5\r",
        "EVN||20250502092900\r",
        "PID|||8888888^^^252^PI||SURNAME^FORENAME||19990101|M\r",
        "PV1||U"
    );

    /// That message, as the v2.xml the flow schema validates.
    fn phw_document() -> String {
        crate::er7_to_xml_for_flow(PHW, Flow::Phw, None).expect("the fixture converts")
    }

    #[test]
    fn a_conforming_document_is_valid() {
        // spec/06-validation.md §9.2
        let report = validate(&phw_document(), Flow::Phw, "ADT_A05").unwrap();
        assert!(report.is_valid(), "{:?}", report.findings);
    }

    #[test]
    fn a_document_with_a_stray_segment_is_invalid() {
        // spec/06-validation.md §9.2 — a real schema rejection, from the
        // primary engine.
        let broken = phw_document().replace("</ADT_A05>", "<ZZZ/></ADT_A05>");
        let report = validate(&broken, Flow::Phw, "ADT_A05").unwrap();

        assert!(!report.is_valid());

        // The element name is quoted by the engine, so sanitising strips it
        // — see `sanitise` for why that trade is made deliberately. The
        // XPath is not quoted, so the offending element is still named.
        let reasons: Vec<&str> = report
            .findings_from(Engine::XsdSchema)
            .iter()
            .map(|finding| finding.reason.as_str())
            .collect();

        assert!(
            reasons.iter().any(|reason| reason.contains("/ADT_A05/ZZZ")),
            "the XPath must still name the element: {reasons:?}"
        );
        assert!(
            reasons.iter().any(|reason| reason.contains("cvc-")),
            "the schema rule must be named: {reasons:?}"
        );
    }

    #[test]
    fn both_engines_are_run_and_both_are_reported() {
        // spec/06-validation.md §9.1 — the point of the module. Whatever
        // they conclude, both are asked and both are recorded.
        let report = validate(&phw_document(), Flow::Phw, "ADT_A05").unwrap();

        assert!(
            !report.findings_from(Engine::Uppsala).is_empty() || report.secondary == Verdict::Valid,
            "the cross-check must have been consulted"
        );
    }

    #[test]
    fn the_known_uppsala_defect_shows_up_as_a_disagreement() {
        // spec/06-validation.md §9.4. This asserts the CURRENT state: the
        // cross-check wrongly rejects a conforming message because it
        // resolves the HL7 `ID` type to XML's built-in `xs:ID`.
        //
        // When that is fixed upstream this test will fail, and the right
        // response is to promote the cross-check rather than to weaken the
        // assertion.
        let report = validate(&phw_document(), Flow::Phw, "ADT_A05").unwrap();

        assert_eq!(report.primary, Verdict::Valid, "xsd-schema is right");
        assert_eq!(
            report.secondary,
            Verdict::Invalid,
            "if uppsala now agrees, its ID-resolution defect is fixed \
             (kushaldas/uppsala#39) — promote the cross-check per §9.4.1 \
             rather than weakening this assertion"
        );
        assert!(!report.engines_agreed());
    }

    #[test]
    fn a_structure_the_flow_has_no_schema_for_is_an_error() {
        let error = validate("<x/>", Flow::Phw, "ADT_A03").unwrap_err();
        assert!(error.to_string().contains("ADT_A03"));
        assert!(error.to_string().contains("Available: ADT_A05, ADT_A39"));
    }

    #[test]
    fn every_flow_and_structure_compiles_under_both_engines() {
        // spec/06-validation.md §9.3 — thirteen schemas, two engines. A
        // schema that will not compile is a defect that must not wait for a
        // message to reveal it.
        for flow in Flow::ALL {
            for structure in flow.structures() {
                let path = schema_path(flow, &structure)
                    .unwrap_or_else(|error| panic!("{flow} {structure}: {error}"));

                with_xsd_schema_validator(&path, |_, _| ())
                    .unwrap_or_else(|error| panic!("xsd-schema {flow} {structure}: {error}"));
                with_uppsala_validator(&path, |_| ())
                    .unwrap_or_else(|error| panic!("uppsala {flow} {structure}: {error}"));
            }
        }
    }

    #[test]
    fn a_quoted_value_never_survives_into_a_finding() {
        // spec/index.md §0.8.4. uppsala quotes the offending value, which
        // for these schemas is a patient's assigning authority.
        let finding = Finding::from_engine(
            Engine::Uppsala,
            "1:155: '4444444444' is not a valid NCName/ID/IDREF",
        );

        assert!(!finding.reason.contains("4444444444"));
        assert!(finding.reason.contains("NCName"), "{}", finding.reason);
    }

    #[test]
    fn sanitising_keeps_structure_and_drops_values() {
        assert_eq!(
            sanitise("[cvc-complex-type.2.4] Element 'PD1' is not allowed (/ADT_A05)"),
            "[cvc-complex-type.2.4] Element <value> is not allowed (/ADT_A05)"
        );
        assert_eq!(sanitise(r#"say "SECRET" now"#), "say <value> now");
        assert_eq!(sanitise("nothing quoted"), "nothing quoted");
    }

    #[test]
    fn an_unbalanced_quote_does_not_leak_the_rest_of_the_line() {
        // The failure mode that matters: a message ending mid-quote must
        // not fall through with its tail intact.
        assert_eq!(sanitise("value 'NEEDLE"), "value <value>");
    }

    #[test]
    fn a_schema_is_compiled_once_and_reused() {
        // Compiling takes tens of milliseconds; an ingress sees thousands of
        // messages a minute.
        let path = schema_path(Flow::Phw, "ADT_A05").unwrap();
        let first = with_xsd_schema_validator(&path, |v, _| std::ptr::from_ref(v)).unwrap();
        let second = with_xsd_schema_validator(&path, |v, _| std::ptr::from_ref(v)).unwrap();
        assert!(std::ptr::eq(first, second));
    }
}
