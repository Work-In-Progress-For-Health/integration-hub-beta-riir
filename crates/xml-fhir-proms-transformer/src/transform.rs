//! WPAS XML in, FHIR message Bundle out.
//!
//! Implements `spec/16-proms-transformer.md` §1 and §2.
//!
//! # The order the slots are filled
//!
//! Every resource is built before the `MessageHeader`, because §6.2's
//! `focus` and `sender` name the entries that turned out to be present —
//! and §3.2 means "present" is not known until each mapper has had its
//! say. The bundle's *entry* order is unaffected: `bundle::Builder` emits
//! in slot order whatever order it was filled in.

use crate::bundle::{Builder, Slot};
use crate::fhir;
use crate::identity::UuidFactory;
use crate::message_type::{RoutingError, resolve};
use crate::parse::{ParseError, event_code, parse};
use crate::reference_data::ReferenceDataResolver;
use crate::resources;

/// Why a payload did not become a bundle.
///
/// Both outcomes abandon the message per
/// [`09-transformers.md`](../../../spec/09-transformers.md) §1.6 — a
/// payload this service cannot read is not one it should acknowledge.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransformError {
    /// The payload was empty or not well-formed (§2.5.6).
    #[error(transparent)]
    Parse(#[from] ParseError),
    /// The `eventCode` named no bundle (§2.3).
    #[error(transparent)]
    Routing(#[from] RoutingError),
}

/// Turn a WPAS payload into a FHIR R4B message Bundle (§1.2).
///
/// `endpoint_override` supplies §5.2's per-board endpoint; pass
/// `|_| None` to use §5.2.2's derived values. `destination_endpoint` is
/// §10.3's `PROMPTLY_DESTINATION_ENDPOINT`; `None` uses the staging default.
///
/// # Errors
///
/// [`TransformError`] for a payload that cannot be read or routed.
pub fn transform(
    payload: &str,
    uuids: &dyn UuidFactory,
    resolver: &dyn ReferenceDataResolver,
    endpoint_override: &dyn Fn(&str) -> Option<String>,
    destination_endpoint: Option<&str>,
) -> Result<serde_json::Value, TransformError> {
    let message = parse(payload)?;
    let message_type = resolve(event_code(&message).as_deref(), &message.root_tag)?;

    // The Referral shape is its own (upstream `#435`): a referrer Practitioner,
    // the consultant's PractitionerRole and Practitioner, the Organization,
    // and no Location. Every other shape keeps one Practitioner and a Location.
    let is_referral = message_type.has_practitioner_role;
    let mut builder = Builder::new(message_type, uuids);
    if is_referral {
        builder = builder.with_profile(fhir::WPAS_IG_MESSAGE_BUNDLE_PROFILE);
    }
    let identities = builder.identities().clone();

    // The optional resources first, so the MessageHeader knows what exists.
    let organization = resources::organization(&message, &identities, is_referral);
    let mut present = vec![Slot::MessageHeader, Slot::Patient, Slot::Clinical];

    if is_referral {
        let referrer = resources::referrer(&message, &identities);
        let consultant = resources::consultant(&message, &identities);

        if referrer.is_some() {
            present.push(Slot::Referrer);
        }
        // The role exists only to carry the consultant's code and specialty
        // to the consultant Practitioner, so it stands or falls with it.
        if consultant.is_some() {
            present.push(Slot::ConsultantRole);
            present.push(Slot::Consultant);
        }
        if organization.is_some() {
            present.push(Slot::Organization);
        }

        builder.put(
            Slot::MessageHeader,
            resources::message_header(
                &message,
                message_type,
                &identities,
                &present,
                endpoint_override,
                destination_endpoint,
            ),
        );
        builder.put(
            Slot::Patient,
            resources::patient(&message, &identities, resolver, true),
        );
        builder.put(
            Slot::Clinical,
            resources::clinical(&message, message_type, &identities, &present),
        );
        builder.put_maybe(Slot::Referrer, referrer);
        if consultant.is_some() {
            builder.put(
                Slot::ConsultantRole,
                resources::consultant_role(&message, &identities, &present),
            );
        }
        builder.put_maybe(Slot::Consultant, consultant);
        builder.put_maybe(Slot::Organization, organization);
    } else {
        let practitioner = resources::practitioner(&message, &identities);
        // §6.11 — always present, unlike the two above (§9.13).
        let location = resources::location(&message, &identities);

        if practitioner.is_some() {
            present.push(Slot::Practitioner);
        }
        if organization.is_some() {
            present.push(Slot::Organization);
        }
        present.push(Slot::Location);

        builder.put(
            Slot::MessageHeader,
            resources::message_header(
                &message,
                message_type,
                &identities,
                &present,
                endpoint_override,
                destination_endpoint,
            ),
        );
        builder.put(
            Slot::Patient,
            resources::patient(&message, &identities, resolver, false),
        );
        builder.put(
            Slot::Clinical,
            resources::clinical(&message, message_type, &identities, &present),
        );
        builder.put_maybe(Slot::Practitioner, practitioner);
        builder.put_maybe(Slot::Organization, organization);
        builder.put(Slot::Location, location);
    }

    Ok(builder.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::SequentialUuids;
    use crate::reference_data::StaticResolver;

    /// The smallest payload that routes.
    const MINIMAL: &str = r"<PromsEventRequest>
        <eventCode>REFERRAL</eventCode>
        <nhsNumber>9434765994</nhsNumber>
      </PromsEventRequest>";

    fn run(payload: &str) -> Result<serde_json::Value, TransformError> {
        transform(
            payload,
            &SequentialUuids::new(),
            &StaticResolver,
            &|_| None,
            None,
        )
    }

    #[test]
    fn a_payload_becomes_a_message_bundle() {
        // spec/16-proms-transformer.md §1.2.
        let bundle = run(MINIMAL).unwrap();

        assert_eq!(bundle["resourceType"], "Bundle");
        assert_eq!(bundle["type"], "message");
        assert_eq!(
            bundle["entry"][0]["resource"]["resourceType"],
            "MessageHeader"
        );
    }

    #[test]
    fn an_unreadable_payload_fails_rather_than_producing_a_bundle() {
        // §2.5.6 and §2.3 — both abandon the message. A bundle built from
        // a payload nobody could read would assert something about a
        // patient on no evidence.
        assert!(matches!(run(""), Err(TransformError::Parse(_))));
        assert!(matches!(
            run("<R><unclosed>"),
            Err(TransformError::Parse(_))
        ));

        let unroutable = run("<PromsEventRequest><eventCode>NOPE</eventCode></PromsEventRequest>");
        assert!(matches!(unroutable, Err(TransformError::Routing(_))));
    }

    fn types_of(bundle: &serde_json::Value) -> Vec<&str> {
        bundle["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["resource"]["resourceType"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn an_absent_resource_shortens_a_referral() {
        // §3.2 — the minimal payload has no referrer, no consultant and no
        // organisation, so those slots go unfilled and a Referral is three
        // entries. Since upstream #435 it has no `Location` either.
        let bundle = run(MINIMAL).unwrap();

        assert_eq!(
            types_of(&bundle),
            vec!["MessageHeader", "Patient", "ServiceRequest"]
        );
        // A Referral claims a bundle profile; nothing else does.
        assert!(
            bundle["meta"]["profile"][0]
                .as_str()
                .unwrap()
                .ends_with("wpas-messagebundle")
        );
    }

    #[test]
    fn a_referral_carries_its_practitioners_in_the_order_of_section_three() {
        // §3, as revised by #435: referrer, then the consultant's role and
        // practitioner, then the organisation.
        let bundle = run(r"<PromsEventRequest>
            <eventCode>REFERRAL</eventCode><system_id>149</system_id><nhsNumber>9434765994</nhsNumber>
            <referrer_code>G1</referrer_code><referrer_name>DR A</referrer_name>
            <consultant_code>C1</consultant_code><clinicianName>Jones, B</clinicianName>
            <consultant_specialty>650100</consultant_specialty><hbCode>X98</hbCode>
          </PromsEventRequest>")
        .unwrap();

        assert_eq!(
            types_of(&bundle),
            vec![
                "MessageHeader",
                "Patient",
                "ServiceRequest",
                "Practitioner",
                "PractitionerRole",
                "Practitioner",
                "Organization",
            ]
        );
        let request = &bundle["entry"][2]["resource"];
        assert_eq!(
            request["requester"]["reference"],
            bundle["entry"][3]["fullUrl"]
        );
        assert_eq!(
            request["performer"][0]["reference"],
            bundle["entry"][5]["fullUrl"]
        );
        // 650 is not on the national specialty list, so none is sent.
        assert!(bundle["entry"][4]["resource"].get("specialty").is_none());
    }

    #[test]
    fn every_other_bundle_keeps_one_practitioner_and_an_always_present_location() {
        // §6.11.0 and §9.13: `map_location` has no guard, so an empty Location
        // is emitted for every non-Referral bundle, carrying only its identity
        // and the fixed `status` (upstream #423).
        let bundle = run(r"<PromsEventRequest>
            <eventCode>SURGERY-PROC</eventCode><nhsNumber>9434765994</nhsNumber>
          </PromsEventRequest>")
        .unwrap();

        assert_eq!(
            types_of(&bundle),
            vec!["MessageHeader", "Patient", "Procedure", "Location"]
        );
        assert!(
            bundle.get("meta").is_none(),
            "only a Referral claims a bundle profile"
        );
        let keys: Vec<&String> = bundle["entry"][3]["resource"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(keys, vec!["id", "meta", "resourceType", "status"]);
    }

    #[test]
    fn every_reference_resolves_within_the_bundle() {
        // §4.3 and §8.4 — a bundle is self-contained, so a reference that
        // named nothing would be a dangling pointer in a clinical message.
        let bundle = run(MINIMAL).unwrap();
        let entries = bundle["entry"].as_array().unwrap();

        let urls: Vec<&str> = entries
            .iter()
            .map(|e| e["fullUrl"].as_str().unwrap())
            .collect();

        let mut checked = 0;
        walk(&bundle, &mut |value| {
            if let Some(reference) = value.get("reference").and_then(serde_json::Value::as_str) {
                assert!(
                    urls.contains(&reference),
                    "{reference} dangles; entries are {urls:?}"
                );
                checked += 1;
            }
        });

        assert!(checked >= 3, "only {checked} references were checked");
    }

    /// Visit every object in a document.
    fn walk(value: &serde_json::Value, visit: &mut impl FnMut(&serde_json::Value)) {
        match value {
            serde_json::Value::Object(map) => {
                visit(value);
                for child in map.values() {
                    walk(child, visit);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    walk(child, visit);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn the_bundle_is_deterministic_for_a_given_factory() {
        // §4.4 — the whole reason the factory is injectable.
        let first = run(MINIMAL).unwrap();
        let second = run(MINIMAL).unwrap();

        assert_eq!(first, second);
    }
}
