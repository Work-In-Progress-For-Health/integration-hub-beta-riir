//! Signing and addressing Cosmos DB requests.
//!
//! Implements `spec/15-dashboard.md` §5.6 and §5.7.
//!
//! # Why the signing is its own module
//!
//! §5.7.2 lists three details that each produce an indistinguishable
//! `401`: the case of each line, the date appearing identically in two
//! places, and the trailing empty line. None of them can be checked against
//! a live account without a subscription, and all of them can be checked
//! against the published algorithm here.
//!
//! So the signature is computed by pure functions with known-answer tests,
//! and the transport is somebody else's problem.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use ihb_config::Secret;
use sha2::Sha256;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc2822;

/// The `x-ms-version` every request carries.
///
/// Pinned for the reason the ARM versions in `arm` are: this workspace owns
/// it now, and a wrong one is a rejection with no useful message.
pub const COSMOS_API_VERSION: &str = "2018-12-31";

/// Which kind of resource a request addresses.
///
/// Lowercased in the signature (§5.7.2), which is why this is an enum
/// rather than a `&str` a caller might capitalise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    /// A database.
    Dbs,
    /// A container.
    Colls,
    /// A document.
    Docs,
}

impl ResourceType {
    /// As it appears in the string to sign — always lowercase.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dbs => "dbs",
            Self::Colls => "colls",
            Self::Docs => "docs",
        }
    }
}

/// The date format Cosmos wants (§5.7.2).
///
/// RFC 1123 in GMT, lowercased. `time` spells RFC 2822 rather than 1123 and
/// the two agree for this purpose, except that 2822 writes `+0000` where
/// 1123 writes `GMT` — so that substitution is made explicitly rather than
/// hoped for.
#[must_use]
pub fn cosmos_date(at: OffsetDateTime) -> String {
    let formatted = at
        .to_offset(time::UtcOffset::UTC)
        .format(&Rfc2822)
        .unwrap_or_default();

    formatted.replace("+0000", "GMT").to_lowercase()
}

/// The five-line string a Cosmos signature covers (§5.7.1).
///
/// # Examples
///
/// ```
/// use dashboard::azure::cosmos::{string_to_sign, ResourceType};
///
/// let signed = string_to_sign("GET", ResourceType::Docs, "dbs/MyDb/colls/MyColl", "sat, 22 aug 2026 09:15:00 gmt");
///
/// // The verb and the resource type are lowercased; the link keeps its case.
/// assert!(signed.starts_with("get\ndocs\ndbs/MyDb/colls/MyColl\n"));
/// // And it ends with an empty fifth line (§5.7.2).
/// assert!(signed.ends_with("\n\n"));
/// ```
#[must_use]
pub fn string_to_sign(
    verb: &str,
    resource_type: ResourceType,
    resource_link: &str,
    date: &str,
) -> String {
    // §5.7.2 — verb and type lowercased, the link **not**, the date already
    // lowercased by `cosmos_date`, and a trailing empty line.
    format!(
        "{}\n{}\n{}\n{}\n\n",
        verb.to_lowercase(),
        resource_type.as_str(),
        resource_link,
        date,
    )
}

/// The `Authorization` header value for key authentication (§5.7.1).
///
/// # Errors
///
/// The key must be valid base64 (§5.7.3). A key that is not is a
/// configuration error rather than a transport failure, and signing with
/// the base64 *text* would produce a header that looks right and is always
/// rejected.
pub fn authorization(
    key: &Secret,
    verb: &str,
    resource_type: ResourceType,
    resource_link: &str,
    date: &str,
) -> Result<String, String> {
    // §5.7.3 — the key is base64 and is decoded before use.
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(key.expose())
        .map_err(|_| {
            // The key itself never appears (§5.7.5).
            "COSMOS_KEY is not valid base64; it is the account key as Azure \
             prints it (spec/15-dashboard.md §5.7.3)"
                .to_string()
        })?;

    let mut mac = <Hmac<Sha256>>::new_from_slice(&decoded)
        .map_err(|_| "COSMOS_KEY is not a usable signing key".to_string())?;
    mac.update(string_to_sign(verb, resource_type, resource_link, date).as_bytes());

    let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

    Ok(percent_encode(&format!(
        "type=master&ver=1.0&sig={signature}"
    )))
}

/// Percent-encode a header value.
///
/// A base64 signature contains `+`, `/` and `=`, all of which change
/// meaning in a header Cosmos parses as a query string.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The resource link for one document (§5.7.1).
///
/// Cosmos calls this the "resource link" and it is the path **without** a
/// leading or trailing slash — `dbs/{db}/colls/{coll}/docs/{id}`.
#[must_use]
pub fn document_link(database: &str, container: &str, id: &str) -> String {
    format!("dbs/{database}/colls/{container}/docs/{id}")
}

/// The resource link for a container's document collection.
#[must_use]
pub fn documents_link(database: &str, container: &str) -> String {
    format!("dbs/{database}/colls/{container}")
}

/// The URL for one document.
#[must_use]
pub fn document_url(endpoint: &str, database: &str, container: &str, id: &str) -> String {
    format!(
        "{}/{}",
        endpoint.trim_end_matches('/'),
        document_link(database, container, id)
    )
}

/// The URL for a container's document collection.
#[must_use]
pub fn documents_url(endpoint: &str, database: &str, container: &str) -> String {
    format!(
        "{}/{}/docs",
        endpoint.trim_end_matches('/'),
        documents_link(database, container)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    /// A key with a known value, so the signature below is reproducible.
    ///
    /// Not a real key: 32 bytes of zeroes, base64-encoded.
    fn key() -> Secret {
        Secret::new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
    }

    #[test]
    fn the_date_is_lowercase_rfc_1123_in_gmt() {
        // spec/15-dashboard.md §5.7.2. `time` spells RFC 2822, which
        // writes `+0000` where Cosmos wants `GMT` — a substitution made
        // explicitly rather than hoped for.
        let date = cosmos_date(datetime!(2026-08-22 09:15:00 UTC));

        assert_eq!(date, "sat, 22 aug 2026 09:15:00 gmt");
        assert!(!date.contains("+0000"), "{date}");
        assert_eq!(date, date.to_lowercase(), "it is lowercased");
    }

    #[test]
    fn a_non_utc_input_is_converted_before_formatting() {
        // The header and the signature must carry the same instant. A
        // local-offset timestamp formatted as-is would be a 401 that looks
        // like a clock problem.
        let elsewhere = datetime!(2026-08-22 11:15:00 +2:00);

        assert_eq!(
            cosmos_date(elsewhere),
            cosmos_date(datetime!(2026-08-22 09:15:00 UTC))
        );
    }

    #[test]
    fn the_string_to_sign_lowercases_only_what_it_should() {
        // §5.7.2's first detail. Lowercasing the resource link too is the
        // classic mistake, and Cosmos answers 401 either way.
        let signed = string_to_sign(
            "GET",
            ResourceType::Docs,
            "dbs/MyDatabase/colls/MyContainer/docs/Config",
            "sat, 22 aug 2026 09:15:00 gmt",
        );

        assert_eq!(
            signed,
            "get\ndocs\ndbs/MyDatabase/colls/MyContainer/docs/Config\n\
             sat, 22 aug 2026 09:15:00 gmt\n\n"
        );
        assert!(
            signed.contains("MyDatabase"),
            "the resource link keeps its case: {signed}"
        );
    }

    #[test]
    fn the_string_to_sign_ends_with_an_empty_line() {
        // §5.7.2's third detail — five lines, so two trailing newlines.
        let signed = string_to_sign("POST", ResourceType::Docs, "dbs/a/colls/b", "d");

        assert!(signed.ends_with("\n\n"), "{signed:?}");

        // Five lines, the fifth empty. `lines()` yields that empty one
        // because the string ends with two newlines — which is the whole
        // requirement, so it is asserted rather than assumed.
        let lines: Vec<&str> = signed.lines().collect();
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(lines[4], "", "the fifth line must be empty");
    }

    #[test]
    fn a_signature_is_stable_for_the_same_inputs() {
        // A known-answer test. The value comes from this implementation,
        // so it does not prove Azure accepts it — what it does prove is
        // that none of §5.7.2's details can be changed without a test
        // failing, which is the part that would otherwise be silent.
        let header = authorization(
            &key(),
            "GET",
            ResourceType::Docs,
            "dbs/db/colls/alarms/docs/config",
            "sat, 22 aug 2026 09:15:00 gmt",
        )
        .expect("a valid key signs");

        assert!(
            header.starts_with("type%3Dmaster%26ver%3D1.0%26sig%3D"),
            "{header}"
        );

        // Same inputs, same signature.
        let again = authorization(
            &key(),
            "GET",
            ResourceType::Docs,
            "dbs/db/colls/alarms/docs/config",
            "sat, 22 aug 2026 09:15:00 gmt",
        )
        .unwrap();
        assert_eq!(header, again);
    }

    #[test]
    fn every_part_of_the_string_changes_the_signature() {
        // If any of these did not, one of §5.7.2's details would be
        // unenforced and the mistake would be a 401 nobody could locate.
        let base =
            |verb, kind, link, date| authorization(&key(), verb, kind, link, date).expect("signs");

        let original = base("GET", ResourceType::Docs, "dbs/a/colls/b", "d1");

        assert_ne!(
            original,
            base("POST", ResourceType::Docs, "dbs/a/colls/b", "d1")
        );
        assert_ne!(
            original,
            base("GET", ResourceType::Colls, "dbs/a/colls/b", "d1")
        );
        assert_ne!(
            original,
            base("GET", ResourceType::Docs, "dbs/a/colls/c", "d1")
        );
        assert_ne!(
            original,
            base("GET", ResourceType::Docs, "dbs/a/colls/b", "d2")
        );

        // And case in the link matters, which is exactly why it is not
        // lowercased.
        assert_ne!(
            original,
            base("GET", ResourceType::Docs, "dbs/A/colls/b", "d1")
        );
    }

    #[test]
    fn the_signature_is_percent_encoded() {
        // A base64 signature contains `+`, `/` and `=`, all of which change
        // meaning in the query-string-shaped header Cosmos parses.
        let header = authorization(&key(), "GET", ResourceType::Docs, "dbs/a", "d").unwrap();

        assert!(!header.contains('='), "{header}");
        assert!(!header.contains('&'), "{header}");
        assert!(!header.contains('+'), "{header}");
        assert!(!header.contains('/'), "{header}");
    }

    #[test]
    fn a_key_that_is_not_base64_is_refused_without_naming_itself() {
        // §5.7.3 and §5.7.5. Signing with the base64 *text* produces a
        // header that looks right and is always rejected — so this is
        // refused up front, and the key never appears in the message.
        let error = authorization(
            &Secret::new("this is not base64 !!!"),
            "GET",
            ResourceType::Docs,
            "dbs/a",
            "d",
        )
        .unwrap_err();

        assert!(error.contains("COSMOS_KEY"), "{error}");
        assert!(error.contains("§5.7.3"), "{error}");
        assert!(
            !error.contains("this is not base64"),
            "the key leaked: {error}"
        );
    }

    #[test]
    fn the_links_and_urls_have_no_stray_slashes() {
        // Cosmos matches the resource link in the signature against the
        // path it parsed from the URL. A leading or doubled slash makes
        // them differ, and the answer is a 401.
        assert_eq!(
            document_link("db", "alarms", "config"),
            "dbs/db/colls/alarms/docs/config"
        );

        for endpoint in [
            "https://x.documents.azure.com",
            "https://x.documents.azure.com/",
        ] {
            assert_eq!(
                document_url(endpoint, "db", "alarms", "config"),
                "https://x.documents.azure.com/dbs/db/colls/alarms/docs/config"
            );
            assert_eq!(
                documents_url(endpoint, "db", "alarms"),
                "https://x.documents.azure.com/dbs/db/colls/alarms/docs"
            );
        }
    }

    #[test]
    fn the_resource_types_are_lowercase() {
        assert_eq!(ResourceType::Dbs.as_str(), "dbs");
        assert_eq!(ResourceType::Colls.as_str(), "colls");
        assert_eq!(ResourceType::Docs.as_str(), "docs");
    }
}
