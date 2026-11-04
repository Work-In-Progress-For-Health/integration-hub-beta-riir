//! `(HL7 version, message structure)` to XSD path.
//!
//! Implements `spec/06-validation.md` §10.4. Ported from upstream
//! `hl7_message_processor/schema_resolver.py` (`c17b134`).
//!
//! Two trees use different file naming — standard `<ver>/<ver>_<STRUCT>.xsd`,
//! custom `<ver>/<STRUCT>_<ver>.xsd` — so the index is built by scanning
//! directories and normalising names rather than by formatting a path, which
//! would get one of the two wrong.
//!
//! # Examples
//!
//! ```
//! use ihb_validation::resolver::normalize_version;
//!
//! assert_eq!(normalize_version("  2.5.1 "), "2_5_1");
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::ValidationError;

/// `fields`, `segments` and `types` share a directory with structures but are
/// not structures (`spec/06-validation.md` §10.4.2).
const NON_STRUCTURE_STEMS: [&str; 3] = ["fields", "segments", "types"];

/// `2.5.1` becomes `2_5_1` (`spec/06-validation.md` §10.4.1).
#[must_use]
pub fn normalize_version(raw: &str) -> String {
    raw.trim().replace('.', "_")
}

/// `<ver>_<STRUCT>` to `STRUCT`.
fn from_standard(version_key: &str, stem: &str) -> Option<String> {
    let rest = stem.strip_prefix(version_key)?.strip_prefix('_')?;
    (!rest.is_empty() && !NON_STRUCTURE_STEMS.contains(&rest)).then(|| rest.to_owned())
}

/// `<STRUCT>_<ver>` to `STRUCT`.
fn from_custom(version_key: &str, stem: &str) -> Option<String> {
    let rest = stem.strip_suffix(version_key)?.strip_suffix('_')?;
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// `(version key, structure)` to XSD path.
type Index = HashMap<(String, String), PathBuf>;

/// Scan `root/<version>/*.xsd`, naming each structure with `structure_of`.
fn build_index(root: &Path, structure_of: fn(&str, &str) -> Option<String>) -> Index {
    let mut index = Index::new();
    let Ok(versions) = std::fs::read_dir(root) else {
        return index;
    };
    for version in versions.flatten().filter(|e| e.path().is_dir()) {
        let key = version.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(version.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_some_and(|e| e == "xsd")
                && let Some(structure) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|stem| structure_of(&key, stem))
            {
                index.insert((key.clone(), structure), path);
            }
        }
    }
    index
}

/// The `2_5_1` custom schemas, compiled in (`spec/06-validation.md` §11.2).
///
/// The directory-scanning [`SchemaResolver`] needs the files at run time,
/// which a container does not have (§9.6); these two are self-contained and
/// small, so they travel in the binary.
#[must_use]
pub fn custom_schema(version: &str, structure: &str) -> Option<&'static str> {
    if normalize_version(version) != "2_5_1" {
        return None;
    }
    match structure {
        "ORU_R01" => Some(include_str!("../schemas/2_5_1/ORU_R01_2_5_1.xsd")),
        "OMG_O19" => Some(include_str!("../schemas/2_5_1/OMG_O19_2_5_1.xsd")),
        _ => None,
    }
}

/// Resolves a version and structure to an XSD in one of two directories.
#[derive(Debug)]
pub struct SchemaResolver {
    /// Standard schemas, `<ver>/<ver>_<STRUCT>.xsd`.
    schemas_dir: PathBuf,
    /// Custom schemas, `<ver>/<STRUCT>_<ver>.xsd`.
    custom_dir: PathBuf,
}

impl SchemaResolver {
    /// A resolver over `schemas_dir` (standard) and `custom_dir` (custom).
    /// Neither need exist: a missing directory just has nothing in it
    /// (`spec/06-validation.md` §10.4.4).
    #[must_use]
    pub fn new(schemas_dir: impl Into<PathBuf>, custom_dir: impl Into<PathBuf>) -> Self {
        Self {
            schemas_dir: schemas_dir.into(),
            custom_dir: custom_dir.into(),
        }
    }

    /// The XSD for `(version, structure)`: custom first, then standard
    /// (`spec/06-validation.md` §10.4.3).
    ///
    /// The directories are scanned on each call; the caller resolves once per
    /// message at most and a cache here would go stale in tests for no gain.
    ///
    /// # Errors
    ///
    /// [`ValidationError`] naming both directories if neither has a match
    /// (`spec/06-validation.md` §10.4.4).
    pub fn resolve(&self, version: &str, structure: &str) -> Result<PathBuf, ValidationError> {
        let key = normalize_version(version);
        let wanted = (key.clone(), structure.to_owned());
        build_index(&self.custom_dir, from_custom)
            .remove(&wanted)
            .or_else(|| build_index(&self.schemas_dir, from_standard).remove(&wanted))
            .ok_or_else(|| {
                ValidationError::schema(
                    structure,
                    format!(
                        "No schema found for HL7 version '{version}' (normalised '{key}') and structure \
                         '{structure}' under {} or {}",
                        self.schemas_dir.display(),
                        self.custom_dir.display()
                    ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    //! Ported from upstream `tests/test_schema_resolver.py`.
    use super::*;

    /// `spec/06-validation.md` §11.2.
    #[test]
    fn the_two_custom_schemas_are_embedded_and_nothing_else_is() {
        assert!(custom_schema("2.5.1", "ORU_R01").is_some_and(|x| x.contains("ORU_R01")));
        assert!(custom_schema(" 2_5_1 ", "OMG_O19").is_some());
        assert!(custom_schema("2.5", "ORU_R01").is_none());
        assert!(custom_schema("2.5.1", "ADT_A05").is_none());
    }
    use std::fs;

    struct Fixture {
        root: PathBuf,
        resolver: SchemaResolver,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("ihb-resolver-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let std_dir = root.join("schemas/2_5_1");
            let cus_dir = root.join("custom_schemas/2_5_1");
            fs::create_dir_all(&std_dir).unwrap();
            fs::create_dir_all(&cus_dir).unwrap();
            for f in [
                "2_5_1_ADT_A05",
                "2_5_1_ORU_R01",
                "2_5_1_fields",
                "2_5_1_segments",
                "2_5_1_types",
            ] {
                fs::write(std_dir.join(format!("{f}.xsd")), "<xsd:schema/>").unwrap();
            }
            fs::write(cus_dir.join("ORU_R01_2_5_1.xsd"), "<xsd:schema/>").unwrap();
            let resolver = SchemaResolver::new(root.join("schemas"), root.join("custom_schemas"));
            Self { root, resolver }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn dots_become_underscores() {
        assert_eq!(normalize_version("2.5.1"), "2_5_1");
    }

    #[test]
    fn version_is_trimmed() {
        assert_eq!(normalize_version("  2.5.1  "), "2_5_1");
    }

    #[test]
    fn resolves_standard_when_no_custom() {
        let f = Fixture::new("std");
        let p = f.resolver.resolve("2.5.1", "ADT_A05").unwrap();
        assert_eq!(p, f.root.join("schemas/2_5_1/2_5_1_ADT_A05.xsd"));
    }

    #[test]
    fn prefers_custom_when_present() {
        let f = Fixture::new("cus");
        let p = f.resolver.resolve("2.5.1", "ORU_R01").unwrap();
        assert_eq!(p, f.root.join("custom_schemas/2_5_1/ORU_R01_2_5_1.xsd"));
    }

    #[test]
    fn non_structure_files_are_not_indexed() {
        let f = Fixture::new("nonstruct");
        assert!(f.resolver.resolve("2.5.1", "fields").is_err());
    }

    #[test]
    fn no_match_is_an_error_naming_both_directories() {
        let f = Fixture::new("nomatch");
        let e = f
            .resolver
            .resolve("2.5.1", "NOPE_N00")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("normalised '2_5_1'")
                && e.contains("NOPE_N00")
                && e.contains("custom_schemas"),
            "{e}"
        );
    }

    #[test]
    fn unknown_version_is_an_error() {
        let f = Fixture::new("ver");
        assert!(f.resolver.resolve("9.9.9", "ADT_A05").is_err());
    }

    #[test]
    fn missing_directories_do_not_panic() {
        let r = SchemaResolver::new("/does/not/exist", "/also/missing");
        assert!(r.resolve("2.5.1", "ADT_A05").is_err());
    }

    /// `spec/06-validation.md` §10.5.1: the vendored `2_5_1` tree resolves by
    /// the custom naming convention.
    #[test]
    fn vendored_custom_schemas_resolve() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas");
        let r = SchemaResolver::new(dir.join("none"), &dir);
        assert!(
            r.resolve("2.5.1", "OMG_O19")
                .unwrap()
                .ends_with("2_5_1/OMG_O19_2_5_1.xsd")
        );
        assert!(r.resolve("2.5.1", "ORU_R01").is_ok());
    }
}
