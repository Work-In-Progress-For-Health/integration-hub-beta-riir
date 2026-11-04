//! The two `MappingType` values.
//!
//! Implements `spec/09-transformers.md` §10.7.
//!
//! Kept in this service rather than a shared library, as upstream
//! (`app_config.py`): the values are a contract with the Terraform
//! subscription filters (`user.MappingType = '...'`) and must be changeable
//! without rebuilding any other image.

use ihb_config::{ConfigErrors, Reader};

/// `MAPPING_TYPE_NHS_ID_UPDATE`'s default (§10.7). Must equal the value in
/// the Terraform subscription filter, or the classified message is published
/// and then matched by no subscription.
pub const DEFAULT_NHS_ID_UPDATE: &str = "LIMS_NHS_ID_UPDATE";

/// `MAPPING_TYPE_A40`'s default (§10.7). Same contract with the subscription
/// filter as [`DEFAULT_NHS_ID_UPDATE`].
pub const DEFAULT_A40: &str = "ADT_A40";

/// The `MappingType` property values the classifier emits.
///
/// # Examples
///
/// ```
/// use hl7_lims_wrrs_transformer::MappingTypes;
///
/// let types = MappingTypes::from_pairs([("MAPPING_TYPE_A40", "X_A40")])?;
/// assert_eq!(types.nhs_id_update, "LIMS_NHS_ID_UPDATE");
/// assert_eq!(types.a40, "X_A40");
/// # Ok::<(), ihb_config::ConfigErrors>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingTypes {
    /// For the LIMS/NHS ID Update rule (`§10.2`).
    pub nhs_id_update: String,
    /// For the `A40` rule (`§10.2`).
    pub a40: String,
}

impl Default for MappingTypes {
    /// The defaults of §10.7 (`LIMS_NHS_ID_UPDATE`, `ADT_A40`).
    fn default() -> Self {
        Self {
            nhs_id_update: DEFAULT_NHS_ID_UPDATE.to_string(),
            a40: DEFAULT_A40.to_string(),
        }
    }
}

impl MappingTypes {
    /// Read from the process environment.
    ///
    /// # Errors
    ///
    /// Equal values (`§10.7`).
    pub fn from_env() -> Result<Self, ConfigErrors> {
        Self::read(Reader::from_env())
    }

    /// Read from explicit pairs, for tests and examples.
    ///
    /// # Errors
    ///
    /// Equal values (`§10.7`).
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, ConfigErrors>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self::read(Reader::from_pairs(pairs))
    }

    /// The shared body. `string_or` treats blank as absent (`§10.7`), as the
    /// Python's `(os.getenv(..) or "").strip() or default` did.
    fn read(mut reader: Reader) -> Result<Self, ConfigErrors> {
        let types = Self {
            nhs_id_update: reader.string_or("MAPPING_TYPE_NHS_ID_UPDATE", DEFAULT_NHS_ID_UPDATE),
            a40: reader.string_or("MAPPING_TYPE_A40", DEFAULT_A40),
        };

        // Upstream raises ValueError: identical values would give two paths
        // one Service Bus message id and duplicate detection would drop a
        // copy (`§10.4`).
        if types.nhs_id_update == types.a40 {
            reader.conflict(format!(
                "MAPPING_TYPE_NHS_ID_UPDATE and MAPPING_TYPE_A40 must be different values, \
                 both resolved to '{}'",
                types.a40
            ));
        }

        reader.finish_with(types)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        // §10.7 (upstream test_defaults).
        let types = MappingTypes::from_pairs(Vec::<(String, String)>::new()).unwrap();
        assert_eq!(types, MappingTypes::default());
        assert_eq!(types.nhs_id_update, "LIMS_NHS_ID_UPDATE");
        assert_eq!(types.a40, "ADT_A40");
    }

    #[test]
    fn overrides() {
        // §10.7 (test_env_overrides).
        let types = MappingTypes::from_pairs([
            ("MAPPING_TYPE_NHS_ID_UPDATE", "CUSTOM_NHS"),
            ("MAPPING_TYPE_A40", "CUSTOM_A40"),
        ])
        .unwrap();
        assert_eq!(types.nhs_id_update, "CUSTOM_NHS");
        assert_eq!(types.a40, "CUSTOM_A40");
    }

    #[test]
    fn blank_falls_back_to_the_default() {
        // §10.7 (test_blank_override_falls_back_to_default).
        let types = MappingTypes::from_pairs([("MAPPING_TYPE_A40", "   ")]).unwrap();
        assert_eq!(types.a40, "ADT_A40");
    }

    #[test]
    fn identical_values_are_rejected() {
        // §10.7 (test_identical_values_are_rejected).
        let errors = MappingTypes::from_pairs([
            ("MAPPING_TYPE_NHS_ID_UPDATE", "SAME"),
            ("MAPPING_TYPE_A40", "SAME"),
        ])
        .unwrap_err();
        assert!(errors.to_string().contains("must be different"), "{errors}");
    }
}
