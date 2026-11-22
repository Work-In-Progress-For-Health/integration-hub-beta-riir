//! The six per-flow schema dictionaries, and how a message is matched to
//! one.
//!
//! Implements `spec/06-validation.md` §2 and §3.
//!
//! Each NHS Wales source system speaks its own dialect of HL7 v2 — a
//! different release, a different set of message structures, different
//! cardinalities on the same segments. The Python validated the v2.xml
//! rendering of a message against that flow's XSDs with `xmlschema`. Rust
//! has no comparable XML Schema validator, so the XSDs are compiled offline
//! into `hl7-2` dictionaries by `cargo xtask build-dictionaries`, and
//! validation becomes `hl7-2`'s own schema-shape validation.
//!
//! The dictionaries are **embedded in the binary**: no file is read at run
//! time, and a container image needs no schema directory. `cargo xtask
//! verify-dictionaries` fails the build if a committed dictionary has
//! drifted from the XSDs it came from.
//!
//! # Examples
//!
//! ```
//! use ihb_validation::flow::Flow;
//!
//! let flow: Flow = "phw".parse().expect("phw is a schema group");
//! assert_eq!(flow.as_str(), "phw");
//! assert_eq!(flow.structures(), ["ADT_A05", "ADT_A39"]);
//!
//! // `mpi` is not a schema group; it selects field rules, not a schema.
//! assert!("mpi".parse::<Flow>().is_err());
//! ```

use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use hl7_2::Dictionary;

use crate::error::ValidationError;

/// One flow's compiled schema dictionary, embedded at build time.
macro_rules! dictionary_json {
    ($name:literal) => {
        include_str!(concat!("../dictionaries/", $name, ".json"))
    };
}

/// A schema group: one source system's dialect.
///
/// The value of `HL7_VALIDATION_FLOW`, minus `mpi` — which selects custom
/// field validation and routing properties rather than a schema, and
/// suppresses schema validation entirely
/// (`spec/01-architecture.md` §2.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Public Health Wales. HL7 2.5.
    Phw,
    /// Paris. HL7 2.5.1.
    Paris,
    /// ChemoCare. HL7 2.4.
    Chemo,
    /// PIMS. HL7 2.3.1.
    Pims,
    /// Mosaiq. HL7 2.5.
    Mosaiq,
    /// Welsh Reference Data Service. HL7 2.5.
    Wds,
}

impl Flow {
    /// Every schema group, in the order `spec/06-validation.md` §3.1 lists
    /// them.
    pub const ALL: [Self; 6] = [
        Self::Phw,
        Self::Paris,
        Self::Chemo,
        Self::Pims,
        Self::Mosaiq,
        Self::Wds,
    ];

    /// The name this flow is configured under.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Phw => "phw",
            Self::Paris => "paris",
            Self::Chemo => "chemo",
            Self::Pims => "pims",
            Self::Mosaiq => "mosaiq",
            Self::Wds => "wds",
        }
    }

    /// The HL7 release this flow's schemas describe.
    ///
    /// Informational: the dictionary carries it, and nothing branches on it.
    /// Worth having because "which release does PIMS speak?" is asked often
    /// enough (`spec/06-validation.md` §3.1).
    #[must_use]
    pub const fn release(self) -> &'static str {
        match self {
            Self::Phw | Self::Mosaiq | Self::Wds => "2.5",
            Self::Paris => "2.5.1",
            Self::Chemo => "2.4",
            Self::Pims => "2.3.1",
        }
    }

    /// This flow's compiled dictionary, parsed once and shared.
    ///
    /// # Panics
    ///
    /// Panics if a committed dictionary does not parse. That is a build-time
    /// defect — `cargo xtask verify-dictionaries` loads every one of them —
    /// and a service that cannot validate its flow must not start pretending
    /// it can.
    #[must_use]
    // A committed dictionary that does not parse is a build-time defect —
    // `cargo xtask verify-dictionaries` loads every one of them in CI — and
    // a service that cannot validate its flow must stop rather than start
    // pretending it can. See spec/17-testing-and-quality.md §2.3.
    pub fn dictionary(self) -> Arc<Dictionary> {
        /// One cache slot per flow.
        static CACHE: [OnceLock<Arc<Dictionary>>; 6] = [
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
        ];

        // `index()` is always below `CACHE.len()`; if it were not, the
        // dictionary is built without caching, never indexed out of range.
        let Some(slot) = CACHE.get(self.index()) else {
            return self.build_dictionary();
        };
        slot.get_or_init(|| self.build_dictionary()).clone()
    }

    /// Parse the embedded dictionary for this flow.
    ///
    /// Panics, deliberately, as `dictionary` documents.
    #[allow(
        clippy::panic,
        reason = "an unparseable committed dictionary is unrecoverable"
    )]
    fn build_dictionary(self) -> Arc<Dictionary> {
        let json = self.dictionary_json();
        let dictionary = Dictionary::from_json(json, self.as_str()).unwrap_or_else(|error| {
            panic!(
                "the committed {} dictionary does not parse: {error}. \
                 Run `cargo xtask build-dictionaries`.",
                self.as_str()
            )
        });
        Arc::new(dictionary)
    }

    /// The message structures this flow has a schema for, sorted.
    ///
    /// What [`ValidationError::NoSchema`] lists when a message asks for one
    /// this flow does not have.
    #[must_use]
    pub fn structures(self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .dictionary()
            .structure_ids()
            .map(str::to_string)
            .collect();
        ids.sort();
        ids
    }

    /// Whether this flow can validate a message of the given structure.
    #[must_use]
    pub fn has_structure(self, structure: &str) -> bool {
        self.dictionary().structure(structure).is_some()
    }

    /// This flow's slot in [`Flow::ALL`], and in the dictionary cache.
    const fn index(self) -> usize {
        match self {
            Self::Phw => 0,
            Self::Paris => 1,
            Self::Chemo => 2,
            Self::Pims => 3,
            Self::Mosaiq => 4,
            Self::Wds => 5,
        }
    }

    /// The embedded JSON for this flow.
    const fn dictionary_json(self) -> &'static str {
        match self {
            Self::Phw => dictionary_json!("phw"),
            Self::Paris => dictionary_json!("paris"),
            Self::Chemo => dictionary_json!("chemo"),
            Self::Pims => dictionary_json!("pims"),
            Self::Mosaiq => dictionary_json!("mosaiq"),
            Self::Wds => dictionary_json!("wds"),
        }
    }
}

impl FromStr for Flow {
    type Err = ValidationError;

    /// Parse a `HL7_VALIDATION_FLOW` value.
    ///
    /// # Errors
    ///
    /// [`ValidationError::NoSchema`] naming every group there is, when the
    /// value is not one. `mpi` fails here deliberately: it is a valid
    /// configuration value that selects no schema, and a caller must handle
    /// that case rather than have it silently resolve to something.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|flow| flow.as_str() == value.trim())
            .ok_or_else(|| ValidationError::NoSchema {
                flow: value.trim().to_string(),
                structure: "*".to_string(),
                available: Self::ALL
                    .iter()
                    .map(|flow| flow.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
    }
}

impl std::fmt::Display for Flow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_flow_has_a_dictionary_that_loads() {
        // spec/06-validation.md §3.2.5 — embedded, parsed once, no file
        // read at run time. A committed dictionary that does not load is a
        // build-time defect this catches.
        for flow in Flow::ALL {
            let dictionary = flow.dictionary();
            assert_eq!(dictionary.name(), flow.as_str());
            assert!(
                !flow.structures().is_empty(),
                "{flow} has no message structures"
            );
        }
    }

    #[test]
    fn a_dictionary_is_parsed_once_and_shared() {
        // spec/06-validation.md §3.2.5 — cached for the process lifetime.
        let first = Flow::Phw.dictionary();
        let second = Flow::Phw.dictionary();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn the_flows_carry_the_structures_the_specification_lists() {
        // spec/06-validation.md §3.1
        assert_eq!(Flow::Phw.structures(), ["ADT_A05", "ADT_A39"]);
        assert_eq!(Flow::Paris.structures(), ["ADT_A05", "ADT_A39"]);
        assert_eq!(Flow::Chemo.structures(), ["ADT_A05", "ADT_A39"]);
        assert_eq!(Flow::Mosaiq.structures(), ["ADT_A05", "ADT_A39"]);
        assert_eq!(Flow::Wds.structures(), ["ADT_A05", "ADT_A39"]);
        assert_eq!(Flow::Pims.structures(), ["ADT_A01", "ADT_A39", "ADT_A40"]);
    }

    #[test]
    fn the_flows_carry_the_releases_the_specification_lists() {
        // spec/06-validation.md §3.1
        assert_eq!(Flow::Phw.release(), "2.5");
        assert_eq!(Flow::Paris.release(), "2.5.1");
        assert_eq!(Flow::Chemo.release(), "2.4");
        assert_eq!(Flow::Pims.release(), "2.3.1");
    }

    #[test]
    fn a_configured_name_resolves_to_its_flow() {
        for flow in Flow::ALL {
            assert_eq!(flow.as_str().parse::<Flow>().unwrap(), flow);
        }
        assert_eq!("  phw  ".parse::<Flow>().unwrap(), Flow::Phw);
    }

    #[test]
    fn mpi_is_not_a_schema_group() {
        // spec/06-validation.md §3.1.1, spec/01-architecture.md §2.2.1.
        // A valid configuration value that selects no schema; the caller
        // must handle it rather than have it resolve to something.
        let error = "mpi".parse::<Flow>().unwrap_err();
        assert!(error.to_string().contains("phw, paris, chemo"));
    }

    #[test]
    fn an_unknown_flow_lists_the_real_ones() {
        let error = "nonsense".parse::<Flow>().unwrap_err();
        assert!(error.to_string().contains("nonsense"));
        assert!(error.to_string().contains("wds"));
    }

    #[test]
    fn a_structure_is_recognised_or_not() {
        // spec/06-validation.md §3.1.2
        assert!(Flow::Phw.has_structure("ADT_A05"));
        assert!(!Flow::Phw.has_structure("ADT_A03"));
        assert!(Flow::Pims.has_structure("ADT_A40"));
        assert!(!Flow::Phw.has_structure("ADT_A40"));
    }

    #[test]
    fn the_dictionaries_carry_cardinality_not_just_types() {
        // spec/06-validation.md §3.2 — the whole reason the XSD conversion
        // is usable: `required` and `repeats` come from the schema, which is
        // what makes schema-shape validation equivalent to what xmlschema
        // was checking.
        let dictionary = Flow::Phw.dictionary();
        let pid3 = dictionary.field_cardinality("PID", 3);
        assert!(pid3.repeats, "PID-3 repeats in the PHW schema");

        // MSH-7 does not repeat, so the two are told apart rather than
        // everything being reported as repeating.
        assert!(!dictionary.field_cardinality("MSH", 7).repeats);
    }
}
