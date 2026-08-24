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

use crate::bundle::{Builder, Slot, slots_for};
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
/// `|_| None` to use the documented values and §5.2.2's placeholder.
///
/// # Errors
///
/// [`TransformError`] for a payload that cannot be read or routed.
pub fn transform(
    payload: &str,
    uuids: &dyn UuidFactory,
    resolver: &dyn ReferenceDataResolver,
    endpoint_override: &dyn Fn(&str) -> Option<String>,
) -> Result<serde_json::Value, TransformError> {
    let message = parse(payload)?;
    let message_type = resolve(event_code(&message).as_deref(), &message.root_tag)?;

    let mut builder = Builder::new(message_type, uuids);

    // The optional resources first, so the MessageHeader knows what exists.
    let practitioner = resources::practitioner(&message, message_type, builder.identities());
    let organization = resources::organization(&message, builder.identities());
    // §6.11 — always present, unlike the two above (§9.13).
    let location = resources::location(&message, builder.identities());

    let mut present = vec![Slot::MessageHeader, Slot::Patient, Slot::Clinical];
    if slots_for(message_type).contains(&Slot::PractitionerRole) {
        present.push(Slot::PractitionerRole);
    }
    if practitioner.is_some() {
        present.push(Slot::Practitioner);
    }
    if organization.is_some() {
        present.push(Slot::Organization);
    }
    present.push(Slot::Location);

    let identities = builder.identities().clone();

    builder.put(
        Slot::MessageHeader,
        resources::message_header(
            &message,
            message_type,
            &identities,
            &present,
            endpoint_override,
        ),
    );
    builder.put(
        Slot::Patient,
        resources::patient(&message, &identities, resolver),
    );
    builder.put(
        Slot::Clinical,
        resources::clinical(&message, message_type, &identities, &present),
    );

    if present.contains(&Slot::PractitionerRole) {
        builder.put(
            Slot::PractitionerRole,
            resources::practitioner_role(&message, &identities, &present),
        );
    }

    builder.put_maybe(Slot::Practitioner, practitioner);
    builder.put_maybe(Slot::Organization, organization);
    builder.put(Slot::Location, location);

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
        transform(payload, &SequentialUuids::new(), &StaticResolver, &|_| None)
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

    #[test]
    fn an_absent_resource_shortens_the_bundle() {
        // §3.2 — the minimal payload has no referrer and no organisation,
        // so those two slots go unfilled.
        //
        // The `Location` is still here, and that is not an oversight:
        // §6.11.0 and §9.13 — `map_location` has no guard where the two
        // mappers beside it do, so an empty `Location` is emitted for
        // every bundle. This assertion is what the golden comparison
        // corrected; it read as the obvious symmetry and was wrong.
        let bundle = run(MINIMAL).unwrap();
        let types: Vec<&str> = bundle["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["resource"]["resourceType"].as_str().unwrap())
            .collect();

        assert_eq!(
            types,
            vec![
                "MessageHeader",
                "Patient",
                "ServiceRequest",
                "PractitionerRole",
                "Location",
            ]
        );

        // And it carries nothing but its identity.
        let location = &bundle["entry"][4]["resource"];
        let keys: Vec<&String> = location.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["id", "meta", "resourceType"]);
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
