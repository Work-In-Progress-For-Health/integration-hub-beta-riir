//! Choosing the parameters of `dbo.prQueueIdUpdate` from an `A31` or `A40`.
//!
//! Implements `spec/10-senders.md` §7.8. Upstream:
//! `db_sender/handlers/queue_idupd_handler.py` (`#455`).
//!
//! A pure function from the message to either the six parameters or a
//! rejection, so every rule is tested without SQL Server. **No identifier
//! appears in a rejection**: the description names the *authority* that was
//! missing, never a value (`§7.5`).

use ihb_bus::metadata::Properties;

use crate::ids::{mrg_identifiers, pid_identifier_by_authority};
use crate::msh::{MshInfo, UNKNOWN};

/// Dead-letter reason: an identifier or authority wider than its column.
pub const VALUE_TOO_LONG: &str = "IdentifierTooLong";

/// The widest identifier `dbo.prQueueIdUpdate` accepts: `varchar(20)`.
pub const MAX_IDENTIFIER_LENGTH: usize = 20;

/// The widest assigning authority it accepts: `varchar(10)`.
pub const MAX_AUTHORITY_LENGTH: usize = 10;

/// Dead-letter reason: neither `A31` nor `A40`.
pub const UNSUPPORTED_TRIGGER: &str = "UnsupportedTriggerEvent";
/// Dead-letter reason: `A40` with no `MRG-1` identifier.
pub const MISSING_OLD_IDENTIFIER: &str = "MissingOldIdentifier";
/// Dead-letter reason: `A40` with no `MRG-1` assigning authority.
pub const MISSING_OLD_AUTHORITY: &str = "MissingOldAssigningAuthority";
/// Dead-letter reason: `A40` whose `PID-3` has no identifier of that authority.
pub const MISSING_NEW_IDENTIFIER: &str = "MissingNewIdentifier";
/// Dead-letter reason: `A31` with no identifier of the source system's authority.
pub const MISSING_PROVIDER_IDENTIFIER: &str = "MissingLimsIdentifier";
/// Dead-letter reason: `A31` with no NHS number.
pub const MISSING_NHS_IDENTIFIER: &str = "MissingNhsIdentifier";

/// The assigning authority of an NHS number.
pub const NHS_AUTHORITY: &str = "NHS";

/// The six parameters of `dbo.prQueueIdUpdate`, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdUpdate {
    /// `@ProviderPatientIdentifier`: the local id (the LIMS id for `A31`;
    /// `NULL` for `A40`).
    pub provider_identifier: Option<String>,
    /// `@ProviderAssigningAuthority`.
    pub provider_authority: Option<String>,
    /// `@OldIdentifier`: the merged-away id (`A40`; `NULL` for `A31`).
    pub old_identifier: Option<String>,
    /// `@OldIdentifierAssigningAuthority`.
    pub old_authority: Option<String>,
    /// `@NewIdentifier`: the NHS number (`A31`) or the surviving id (`A40`).
    pub new_identifier: String,
    /// `@NewIdentifierAssigningAuthority`.
    pub new_authority: String,
}

/// A message that can never be applied: dead-letter it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    /// One of the reason constants of this module.
    pub reason: &'static str,
    /// What was missing, for an operator. Never a value from the message.
    pub description: String,
}

/// A [`Rejected`] with a fixed reason.
fn reject(reason: &'static str, description: impl Into<String>) -> Rejected {
    Rejected {
        reason,
        description: description.into(),
    }
}

/// The source system an `A31` came from: the Service Bus `SourceSystem`
/// property, else `MSH-3.1`, else the configured default
/// (`_resolve_source_system`).
fn source_system(properties: &Properties, msh: &MshInfo, default: &str) -> String {
    ["source_system", "SourceSystem"]
        .iter()
        .find_map(|key| properties.get(*key))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let app = msh.sending_app.trim();
            (!app.is_empty()).then(|| app.to_string())
        })
        .unwrap_or_else(|| default.to_string())
}

/// Plan the call for `body`.
///
/// - `A40` (merge): the old id and authority come from `MRG-1`; the new id is
///   the `PID-3` identifier **of the same authority**; the provider id is
///   `NULL`.
/// - `A31` (NHS number allocation): the provider id is the `PID-3` identifier
///   of the source system's authority, and the new id is the `NHS` one.
///
/// # Errors
///
/// [`Rejected`] for any other trigger or a missing identifier.
///
/// # Examples
///
/// ```
/// use db_sender::idupdate::plan;
/// use db_sender::msh::extract_msh_for_logging;
///
/// let a31 = "MSH|^~\\&|328|328|||2026||ADT^A31^ADT_A05|C1|P|2.5\rPID|1||555^^^328~9999999999^^^NHS";
/// let msh = extract_msh_for_logging(a31);
/// let update = plan(a31, &Default::default(), &msh, "328")?;
/// assert_eq!(update.provider_identifier.as_deref(), Some("555"));
/// assert_eq!(update.new_identifier, "9999999999");
/// assert_eq!(update.new_authority, "NHS");
/// # Ok::<(), db_sender::idupdate::Rejected>(())
/// ```
pub fn plan(
    body: &str,
    properties: &Properties,
    msh: &MshInfo,
    default_source_system: &str,
) -> Result<IdUpdate, Rejected> {
    let trigger = if msh.trigger_event.is_empty() {
        UNKNOWN
    } else {
        msh.trigger_event.as_str()
    };
    let update = match trigger {
        "A40" => plan_merge(body),
        "A31" => plan_allocation(body, properties, msh, default_source_system),
        other => Err(reject(
            UNSUPPORTED_TRIGGER,
            format!("Trigger event {other} is not supported for Queue ID Update"),
        )),
    }?;
    update.check_widths()?;
    Ok(update)
}

impl IdUpdate {
    /// Refuse a value the procedure's parameters would silently cut.
    ///
    /// SQL Server truncates a longer string to a `varchar(n)` parameter
    /// without an error, so an over-long identifier would be stored as a
    /// *different, shorter* identifier (`spec/10-senders.md` §7.8.1,
    /// `tests/against_sql_server.rs`). The description names the field and
    /// the limit, never the value.
    fn check_widths(&self) -> Result<(), Rejected> {
        let identifiers = [
            ("provider identifier", self.provider_identifier.as_deref()),
            ("old identifier", self.old_identifier.as_deref()),
            ("new identifier", Some(self.new_identifier.as_str())),
        ];
        let authorities = [
            (
                "provider assigning authority",
                self.provider_authority.as_deref(),
            ),
            ("old assigning authority", self.old_authority.as_deref()),
            ("new assigning authority", Some(self.new_authority.as_str())),
        ];
        for (name, value) in identifiers {
            if value.is_some_and(|v| v.len() > MAX_IDENTIFIER_LENGTH) {
                return Err(reject(
                    VALUE_TOO_LONG,
                    format!("The {name} is longer than {MAX_IDENTIFIER_LENGTH} characters"),
                ));
            }
        }
        for (name, value) in authorities {
            if value.is_some_and(|v| v.len() > MAX_AUTHORITY_LENGTH) {
                return Err(reject(
                    VALUE_TOO_LONG,
                    format!("The {name} is longer than {MAX_AUTHORITY_LENGTH} characters"),
                ));
            }
        }
        Ok(())
    }
}

/// The `A40` branch of [`plan`].
fn plan_merge(body: &str) -> Result<IdUpdate, Rejected> {
    let (old_id, old_authority) = mrg_identifiers(body);
    let old_id = old_id.ok_or_else(|| reject(MISSING_OLD_IDENTIFIER, "Missing old identifier"))?;
    let old_authority = old_authority
        .ok_or_else(|| reject(MISSING_OLD_AUTHORITY, "Missing old assigning authority"))?;
    let new_id = pid_identifier_by_authority(body, &old_authority)
        .ok_or_else(|| reject(MISSING_NEW_IDENTIFIER, "Missing new identifier"))?;

    Ok(IdUpdate {
        provider_identifier: None,
        provider_authority: Some(old_authority.clone()),
        old_identifier: Some(old_id),
        old_authority: Some(old_authority.clone()),
        new_identifier: new_id,
        new_authority: old_authority,
    })
}

/// The `A31` branch of [`plan`].
fn plan_allocation(
    body: &str,
    properties: &Properties,
    msh: &MshInfo,
    default_source_system: &str,
) -> Result<IdUpdate, Rejected> {
    let source = source_system(properties, msh, default_source_system);
    let provider_id = pid_identifier_by_authority(body, &source).ok_or_else(|| {
        reject(
            MISSING_PROVIDER_IDENTIFIER,
            format!("Missing identifier with authority {source}"),
        )
    })?;
    let nhs = pid_identifier_by_authority(body, NHS_AUTHORITY)
        .ok_or_else(|| reject(MISSING_NHS_IDENTIFIER, "Missing NHS identifier"))?;

    Ok(IdUpdate {
        provider_identifier: Some(provider_id),
        provider_authority: Some(source),
        old_identifier: None,
        old_authority: None,
        new_identifier: nhs,
        new_authority: NHS_AUTHORITY.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msh::extract_msh_for_logging;

    const A31: &str = "MSH|^~\\&|328|328|||2026||ADT^A31^ADT_A05|C1|P|2.5\r\
                       PID|1||555^^^328~9999999999^^^NHS||SURNAME^FORENAME";
    const A40: &str = "MSH|^~\\&|328|328|||2026||ADT^A40^ADT_A39|C2|P|2.5\r\
                       PID|1||7777777777^^^NHS~555^^^328\rMRG|1111111111^^^NHS^NH";

    fn run(body: &str, properties: &Properties) -> Result<IdUpdate, Rejected> {
        plan(body, properties, &extract_msh_for_logging(body), "328")
    }

    fn with(key: &str, value: &str) -> Properties {
        std::iter::once((key.to_string(), value.to_string())).collect()
    }

    #[test]
    fn an_a31_links_the_provider_id_to_the_nhs_number() {
        // spec/10-senders.md §7.8 (upstream test_queue_idupd_handler, A31).
        let update = run(A31, &Properties::default()).unwrap();
        assert_eq!(
            update,
            IdUpdate {
                provider_identifier: Some("555".into()),
                provider_authority: Some("328".into()),
                old_identifier: None,
                old_authority: None,
                new_identifier: "9999999999".into(),
                new_authority: "NHS".into(),
            }
        );
    }

    #[test]
    fn an_a40_merges_into_the_surviving_id_of_the_same_authority() {
        let update = run(A40, &Properties::default()).unwrap();
        assert_eq!(
            update,
            IdUpdate {
                provider_identifier: None,
                provider_authority: Some("NHS".into()),
                old_identifier: Some("1111111111".into()),
                old_authority: Some("NHS".into()),
                new_identifier: "7777777777".into(),
                new_authority: "NHS".into(),
            }
        );
    }

    #[test]
    fn the_source_system_is_the_property_then_msh_3_then_the_default() {
        // §7.8: three sources, in this order. The identifier for 999 exists, so
        // the plan succeeds only if that authority was the one chosen.
        let body = "MSH|^~\\&|777|777|||2026||ADT^A31|C|P|2.5\r\
                    PID|1||111^^^999~222^^^777~333^^^328~9^^^NHS";
        let by_property = run(body, &with("SourceSystem", "999")).unwrap();
        assert_eq!(by_property.provider_identifier.as_deref(), Some("111"));
        let by_snake = run(body, &with("source_system", " 999 ")).unwrap();
        assert_eq!(by_snake.provider_authority.as_deref(), Some("999"));
        let by_msh = run(body, &Properties::default()).unwrap();
        assert_eq!(by_msh.provider_identifier.as_deref(), Some("222"));

        let no_app = "MSH|^~\\&|||||2026||ADT^A31|C|P|2.5\rPID|1||333^^^328~9^^^NHS";
        let by_default = run(no_app, &with("SourceSystem", "  ")).unwrap();
        assert_eq!(by_default.provider_identifier.as_deref(), Some("333"));
    }

    #[test]
    fn any_other_trigger_is_unsupported() {
        for trigger in ["A28", "A08", "UNKNOWN"] {
            let body = format!("MSH|^~\\&|328|328|||2026||ADT^{trigger}|C|P|2.5\rPID|1||5^^^328");
            let error = run(&body, &Properties::default()).unwrap_err();
            assert_eq!(error.reason, UNSUPPORTED_TRIGGER, "{trigger}");
        }
        let none = run("not hl7", &Properties::default()).unwrap_err();
        assert_eq!(none.reason, UNSUPPORTED_TRIGGER);
        assert_eq!(
            none.description,
            "Trigger event UNKNOWN is not supported for Queue ID Update"
        );
    }

    #[test]
    fn each_missing_identifier_has_its_own_reason() {
        let a40 = |tail: &str| format!("MSH|^~\\&|328|328|||2026||ADT^A40|C|P|2.5\r{tail}");
        let cases = [
            (a40("PID|1||7^^^NHS"), MISSING_OLD_IDENTIFIER),
            (a40("PID|1||7^^^NHS\rMRG|^^^NHS"), MISSING_OLD_IDENTIFIER),
            (a40("PID|1||7^^^NHS\rMRG|1111"), MISSING_OLD_AUTHORITY),
            (
                a40("PID|1||7^^^328\rMRG|1111^^^NHS"),
                MISSING_NEW_IDENTIFIER,
            ),
            (
                "MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\rPID|1||9^^^NHS".to_string(),
                MISSING_PROVIDER_IDENTIFIER,
            ),
            (
                "MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\rPID|1||5^^^328".to_string(),
                MISSING_NHS_IDENTIFIER,
            ),
        ];
        for (body, reason) in cases {
            assert_eq!(
                run(&body, &Properties::default()).unwrap_err().reason,
                reason,
                "{body}"
            );
        }
    }

    #[test]
    fn a_value_wider_than_its_column_is_refused_not_truncated() {
        // spec/10-senders.md §7.8.1: SQL Server would cut it silently.
        let long = "1".repeat(21);
        let body =
            format!("MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\rPID|1||555^^^328~{long}^^^NHS");
        let error = run(&body, &Properties::default()).unwrap_err();
        assert_eq!(error.reason, VALUE_TOO_LONG);
        assert_eq!(
            error.description,
            "The new identifier is longer than 20 characters"
        );
        assert!(!error.description.contains(&long));

        // Exactly the width is fine.
        let fits = "1".repeat(20);
        let body =
            format!("MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\rPID|1||555^^^328~{fits}^^^NHS");
        assert!(run(&body, &Properties::default()).is_ok());

        // An authority too: here the source system, from MSH-3.
        let body =
            "MSH|^~\\&|ELEVENCHARS|x|||2026||ADT^A31|C|P|2.5\rPID|1||5^^^ELEVENCHARS~9^^^NHS";
        let error = run(body, &Properties::default()).unwrap_err();
        assert_eq!(
            error.description,
            "The provider assigning authority is longer than 10 characters"
        );
    }

    #[test]
    fn a_rejection_never_contains_an_identifier() {
        // spec/10-senders.md §7.5: the description names an authority, not a value.
        let error = run(
            "MSH|^~\\&|328|328|||2026||ADT^A31|C|P|2.5\rPID|1||5551234567^^^NHS",
            &Properties::default(),
        )
        .unwrap_err();
        assert_eq!(error.description, "Missing identifier with authority 328");
        assert!(!error.description.contains("5551234567"));
    }
}
