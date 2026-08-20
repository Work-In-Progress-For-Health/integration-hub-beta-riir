//! Turning a raw variable value into something typed.
//!
//! These are free functions taking values rather than variable names, so
//! they are testable without touching the process environment
//! (`spec/02-configuration.md` §10.5). [`crate::Reader`] is what pairs them
//! with a name.

/// A value if it is present, `None` if it is absent or blank.
///
/// This is `spec/02-configuration.md` §1.1 and §1.2 in one function, and
/// every other reader in this crate goes through it. Trailing and leading
/// whitespace is stripped; what is left, if anything, is the value.
///
/// # Examples
///
/// ```
/// use ihb_config::present;
///
/// assert_eq!(present(Some("phw-to-mpi")), Some("phw-to-mpi"));
/// assert_eq!(present(Some("  2575  ")), Some("2575"));
/// assert_eq!(present(Some("   ")), None);
/// assert_eq!(present(Some("")), None);
/// assert_eq!(present(None), None);
/// ```
#[must_use]
pub fn present(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|value| !value.is_empty())
}

/// Read a value as a boolean.
///
/// `false`, `0`, `no` and `off` — in any case, after trimming — are false.
/// **Any other non-blank value is true**, which is deliberately lopsided:
/// the Python's two boolean readers disagreed with each other, and this is
/// the union of what they accepted (`spec/02-configuration.md` §1.5).
///
/// An absent or blank value is `None`, so the caller supplies the default
/// rather than this function guessing one.
///
/// # Examples
///
/// ```
/// use ihb_config::parse_bool;
///
/// assert_eq!(parse_bool(Some("false")), Some(false));
/// assert_eq!(parse_bool(Some("OFF")), Some(false));
/// assert_eq!(parse_bool(Some("0")), Some(false));
/// assert_eq!(parse_bool(Some("true")), Some(true));
/// // Anything unrecognised is true, not an error.
/// assert_eq!(parse_bool(Some("yes please")), Some(true));
/// assert_eq!(parse_bool(Some("  ")), None);
/// ```
#[must_use]
pub fn parse_bool(raw: Option<&str>) -> Option<bool> {
    present(raw).map(|value| {
        !matches!(
            value.to_ascii_lowercase().as_str(),
            "false" | "0" | "no" | "off"
        )
    })
}

/// Split a comma-separated value into its non-empty, trimmed elements.
///
/// An absent, blank, or all-separators value yields an empty vector; whether
/// that is acceptable is the caller's decision, per
/// `spec/02-configuration.md` §1.7.
///
/// # Examples
///
/// ```
/// use ihb_config::parse_csv;
///
/// assert_eq!(parse_csv(Some("245,212, 192 ,224")), ["245", "212", "192", "224"]);
/// // Empty elements are dropped, not preserved as empty strings.
/// assert_eq!(parse_csv(Some("a,,b,")), ["a", "b"]);
/// assert!(parse_csv(Some(" , , ")).is_empty());
/// assert!(parse_csv(None).is_empty());
/// ```
#[must_use]
pub fn parse_csv(raw: Option<&str>) -> Vec<String> {
    present(raw)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|element| !element.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_is_the_same_as_absent() {
        // spec/02-configuration.md §1.1
        assert_eq!(present(Some("")), None);
        assert_eq!(present(Some(" \t\n")), None);
        assert_eq!(present(None), None);
    }

    #[test]
    fn values_are_trimmed() {
        // spec/02-configuration.md §1.2
        assert_eq!(present(Some("\t phw \n")), Some("phw"));
    }

    #[test]
    fn only_four_spellings_mean_false() {
        // spec/02-configuration.md §1.5
        for text in ["false", "FALSE", "False", "0", "no", "NO", "off", "OFF"] {
            assert_eq!(parse_bool(Some(text)), Some(false), "{text}");
        }
        for text in ["true", "1", "yes", "on", "disabled", "nope"] {
            assert_eq!(parse_bool(Some(text)), Some(true), "{text}");
        }
    }

    #[test]
    fn a_blank_boolean_has_no_opinion() {
        // spec/02-configuration.md §1.5 — the caller supplies the default.
        assert_eq!(parse_bool(Some("  ")), None);
        assert_eq!(parse_bool(None), None);
    }

    #[test]
    fn a_list_drops_empty_elements() {
        // spec/02-configuration.md §1.7
        assert_eq!(
            parse_csv(Some("ADT_A05, ,ADT_A39,")),
            ["ADT_A05", "ADT_A39"]
        );
    }

    #[test]
    fn a_list_of_nothing_is_empty_not_one_empty_element() {
        // spec/02-configuration.md §1.7 — the trap `"".split(',')` sets.
        assert!(parse_csv(Some("")).is_empty());
        assert!(parse_csv(Some(",")).is_empty());
    }
}
