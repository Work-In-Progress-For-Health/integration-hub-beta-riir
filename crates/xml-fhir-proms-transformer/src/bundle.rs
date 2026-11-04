//! Assembling the FHIR message Bundle.
//!
//! Implements `spec/16-proms-transformer.md` §1.2, §3 and §4.
//!
//! # Slots, not entries
//!
//! §3's table lists the entries a bundle carries **when every source field
//! is populated**. A resource whose source data is absent is omitted
//! (§3.2), so the entry array is shorter and the positions of everything
//! after it shift.
//!
//! This module models that as **slots**: a fixed sequence per bundle shape,
//! each given a UUID up front, each either filled or not. The entries are
//! the filled slots in slot order.
//!
//! That is exactly what the Python does, and it is measurable in its
//! output: in a minimal referral the `Location` carries the **seventh**
//! UUID and sits at the **fifth** entry.

use serde_json::json;

use crate::identity::{UuidFactory, urn_uuid};
use crate::message_type::{Clinical, MessageType};

/// A position in a bundle's shape (§3).
///
/// Ordered (`Ord`) in §3's canonical order, which is also the order UUIDs are
/// allocated and entries are emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    /// Always first — FHIR requires it of a `message` bundle (§3.1).
    MessageHeader,
    /// The patient.
    Patient,
    /// The bundle's clinical resource, whichever §2.2 selects.
    Clinical,
    /// Present in the Referral bundle only.
    PractitionerRole,
    /// The referring clinician or consultant.
    Practitioner,
    /// The sending health board.
    Organization,
    /// Where it happened.
    Location,
}

impl Slot {
    /// The FHIR resource type at this slot, given the bundle's shape.
    #[must_use]
    pub const fn resource_type(self, clinical: Clinical) -> &'static str {
        match self {
            Self::MessageHeader => "MessageHeader",
            Self::Patient => "Patient",
            Self::Clinical => clinical.resource_type(),
            Self::PractitionerRole => "PractitionerRole",
            Self::Practitioner => "Practitioner",
            Self::Organization => "Organization",
            Self::Location => "Location",
        }
    }
}

/// The slots of a bundle, in §3's order.
///
/// The Referral shape is the only one carrying a `PractitionerRole`.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::bundle::{slots_for, Slot};
/// use xml_fhir_proms_transformer::message_type::resolve;
///
/// let referral = resolve(Some("REFERRAL"), "").unwrap();
/// assert_eq!(slots_for(referral).len(), 7);
/// assert_eq!(slots_for(referral)[0], Slot::MessageHeader);
///
/// // Every other shape is the same without the PractitionerRole.
/// let surgery = resolve(Some("SURGERY"), "").unwrap();
/// assert_eq!(slots_for(surgery).len(), 6);
/// assert!(!slots_for(surgery).contains(&Slot::PractitionerRole));
/// ```
#[must_use]
pub fn slots_for(message_type: &MessageType) -> Vec<Slot> {
    let mut slots = vec![Slot::MessageHeader, Slot::Patient, Slot::Clinical];

    if message_type.has_practitioner_role {
        slots.push(Slot::PractitionerRole);
    }

    slots.extend([Slot::Practitioner, Slot::Organization, Slot::Location]);
    slots
}

/// Every slot's identity, allocated before anything is built (§4.1).
///
/// The allocation happens in slot order and covers **every** slot, so the
/// numbering is stable whatever turns out to be absent. See this module's
/// documentation for why that is not an accident.
#[derive(Debug, Clone)]
pub struct Identities {
    /// One UUID per slot, in slot order.
    assigned: Vec<(Slot, String)>,
}

impl Identities {
    /// Allocate one UUID per slot, in order (§4.1).
    ///
    /// Draws exactly `slots_for(message_type).len()` values from `uuids`,
    /// whether or not each slot is later filled; with a deterministic factory
    /// the n-th slot always carries the n-th value.
    #[must_use]
    pub fn allocate(message_type: &MessageType, uuids: &dyn UuidFactory) -> Self {
        Self {
            assigned: slots_for(message_type)
                .into_iter()
                .map(|slot| (slot, uuids.next()))
                .collect(),
        }
    }

    /// This slot's bare hyphenated UUID, or `None` for a slot this bundle
    /// shape does not have (e.g. `PractitionerRole` outside a Referral).
    #[must_use]
    pub fn get(&self, slot: Slot) -> Option<&str> {
        self.assigned
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, uuid)| uuid.as_str())
    }

    /// This slot's `urn:uuid:` reference (§4.3), or `None` for a slot this
    /// bundle shape does not have. Callers must also check the slot is
    /// *filled* before referencing it, or the bundle carries a dangling
    /// reference.
    #[must_use]
    pub fn reference(&self, slot: Slot) -> Option<String> {
        self.get(slot).map(urn_uuid)
    }

    /// The slots this bundle shape has, in §3's order (filled or not).
    #[must_use]
    pub fn slots(&self) -> Vec<Slot> {
        self.assigned.iter().map(|(slot, _)| *slot).collect()
    }
}

/// A bundle being assembled: identities are allocated up front, resources
/// are then added slot by slot, and the entries are the filled slots in slot
/// order (see the module documentation).
#[derive(Debug)]
pub struct Builder {
    /// The identities allocated for this bundle.
    identities: Identities,
    /// The resources built so far, by slot.
    resources: Vec<(Slot, serde_json::Value)>,
}

impl Builder {
    /// Start a bundle for this message type, allocating every slot's UUID
    /// immediately (§4.1) so later omissions cannot renumber the rest.
    #[must_use]
    pub fn new(message_type: &MessageType, uuids: &dyn UuidFactory) -> Self {
        Self {
            identities: Identities::allocate(message_type, uuids),
            resources: Vec::new(),
        }
    }

    /// The identities, for a mapper that needs to reference another slot.
    #[must_use]
    pub const fn identities(&self) -> &Identities {
        &self.identities
    }

    /// Fill a slot.
    ///
    /// A slot left unfilled is omitted from the bundle (§3.2), which is
    /// how an absent `Practitioner` shortens the entry array without
    /// disturbing anything's UUID.
    pub fn put(&mut self, slot: Slot, resource: serde_json::Value) {
        self.resources.push((slot, resource));
    }

    /// Fill a slot, or leave it empty.
    ///
    /// The shape most mappers have: they return `None` when every source
    /// field is absent.
    pub fn put_maybe(&mut self, slot: Slot, resource: Option<serde_json::Value>) {
        if let Some(resource) = resource {
            self.put(slot, resource);
        }
    }

    /// The finished bundle (§1.2).
    ///
    /// Entries are the filled slots in slot order, each with the
    /// `urn:uuid:` `fullUrl` of §4.2.
    #[must_use]
    pub fn finish(self) -> serde_json::Value {
        let entries: Vec<serde_json::Value> = self
            .identities
            .slots()
            .into_iter()
            .filter_map(|slot| {
                let resource = self
                    .resources
                    .iter()
                    .find(|(s, _)| *s == slot)
                    .map(|(_, r)| r.clone())?;
                let uuid = self.identities.get(slot)?;

                Some(json!({
                    "fullUrl": urn_uuid(uuid),
                    "resource": resource,
                }))
            })
            .collect();

        json!({
            "resourceType": "Bundle",
            "type": "message",
            "entry": entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::SequentialUuids;
    use crate::message_type::resolve;

    fn referral() -> &'static MessageType {
        resolve(Some("REFERRAL"), "").unwrap()
    }

    fn surgery() -> &'static MessageType {
        resolve(Some("SURGERY"), "").unwrap()
    }

    #[test]
    fn the_referral_shape_is_the_only_one_with_a_practitioner_role() {
        // spec/16-proms-transformer.md §3 and §8.3.
        assert_eq!(
            slots_for(referral()),
            vec![
                Slot::MessageHeader,
                Slot::Patient,
                Slot::Clinical,
                Slot::PractitionerRole,
                Slot::Practitioner,
                Slot::Organization,
                Slot::Location,
            ]
        );

        for code in ["SURGERY", "PREOP", "INPATIENT", "CANCELLED", "PREREAD"] {
            let slots = slots_for(resolve(Some(code), "").unwrap());
            assert_eq!(slots.len(), 6, "{code}");
            assert!(!slots.contains(&Slot::PractitionerRole), "{code}");
        }
    }

    #[test]
    fn the_message_header_is_always_first() {
        // §3.1 — FHIR requires it of a `message` bundle.
        for code in [
            "REFERRAL",
            "SURGERY",
            "PREOP",
            "INPATIENT",
            "CANCELLED",
            "PREREAD",
        ] {
            let slots = slots_for(resolve(Some(code), "").unwrap());
            assert_eq!(slots[0], Slot::MessageHeader, "{code}");
            assert_eq!(slots[1], Slot::Patient, "{code}");
        }
    }

    #[test]
    fn every_slot_is_given_a_uuid_before_anything_is_built() {
        // §4.1, and the reason the numbering has gaps.
        let uuids = SequentialUuids::new();
        let identities = Identities::allocate(referral(), &uuids);

        assert_eq!(uuids.issued(), 7, "one per slot, whatever is later absent");
        assert_eq!(
            identities.get(Slot::MessageHeader),
            Some("00000000-0000-4000-8000-000000000001")
        );
        assert_eq!(
            identities.get(Slot::Location),
            Some("00000000-0000-4000-8000-000000000007")
        );
    }

    #[test]
    fn an_absent_resource_shortens_the_entries_without_moving_any_uuid() {
        // §3.2 and §3.3 — THE behaviour of this module, and the measured
        // shape of the Python's `MINIMAL_REFERRAL_MESSAGE`: the `Location`
        // carries the **seventh** UUID and sits at the **fifth** entry.
        let uuids = SequentialUuids::new();
        let mut builder = Builder::new(referral(), &uuids);

        builder.put(
            Slot::MessageHeader,
            json!({"resourceType": "MessageHeader"}),
        );
        builder.put(Slot::Patient, json!({"resourceType": "Patient"}));
        builder.put(Slot::Clinical, json!({"resourceType": "ServiceRequest"}));
        builder.put(
            Slot::PractitionerRole,
            json!({"resourceType": "PractitionerRole"}),
        );
        // No Practitioner and no Organization — the minimal referral.
        builder.put_maybe(Slot::Practitioner, None);
        builder.put_maybe(Slot::Organization, None);
        builder.put(Slot::Location, json!({"resourceType": "Location"}));

        let bundle = builder.finish();
        let entries = bundle["entry"].as_array().unwrap();

        assert_eq!(entries.len(), 5);
        assert_eq!(entries[4]["resource"]["resourceType"], "Location");
        assert_eq!(
            entries[4]["fullUrl"], "urn:uuid:00000000-0000-4000-8000-000000000007",
            "the Location keeps the seventh UUID despite being the fifth entry"
        );
    }

    #[test]
    fn the_envelope_is_a_message_bundle() {
        // §1.2.
        let bundle = Builder::new(surgery(), &SequentialUuids::new()).finish();

        assert_eq!(bundle["resourceType"], "Bundle");
        assert_eq!(bundle["type"], "message");
        assert!(bundle["entry"].is_array());
    }

    #[test]
    fn every_entry_carries_the_urn_full_url_of_section_four_two() {
        // §4.2.
        let uuids = SequentialUuids::new();
        let mut builder = Builder::new(surgery(), &uuids);
        builder.put(
            Slot::MessageHeader,
            json!({"resourceType": "MessageHeader"}),
        );
        builder.put(Slot::Patient, json!({"resourceType": "Patient"}));

        let bundle = builder.finish();

        for entry in bundle["entry"].as_array().unwrap() {
            let full_url = entry["fullUrl"].as_str().unwrap();
            assert!(full_url.starts_with("urn:uuid:"), "{full_url}");
            // §4.3 — no server-assigned identifier anywhere.
            assert!(!full_url.contains("http"), "{full_url}");
        }
    }

    #[test]
    fn entries_come_out_in_slot_order_whatever_order_they_were_put_in() {
        // §3 — the order is the bundle's shape, not the order a builder
        // happened to fill it. A mapper that needs the Patient's UUID to
        // build the MessageHeader has to run second, and that must not
        // reorder the bundle.
        let uuids = SequentialUuids::new();
        let mut builder = Builder::new(referral(), &uuids);

        builder.put(Slot::Location, json!({"resourceType": "Location"}));
        builder.put(Slot::Patient, json!({"resourceType": "Patient"}));
        builder.put(
            Slot::MessageHeader,
            json!({"resourceType": "MessageHeader"}),
        );
        builder.put(Slot::Clinical, json!({"resourceType": "ServiceRequest"}));

        let bundle = builder.finish();
        let types: Vec<&str> = bundle["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["resource"]["resourceType"].as_str().unwrap())
            .collect();

        assert_eq!(
            types,
            vec!["MessageHeader", "Patient", "ServiceRequest", "Location"]
        );
    }

    #[test]
    fn a_reference_resolves_to_an_entry_in_the_same_bundle() {
        // §4.3 and §8.4 — a bundle is self-contained, so every reference
        // has to name a `fullUrl` that is present.
        let uuids = SequentialUuids::new();
        let mut builder = Builder::new(referral(), &uuids);

        let patient = builder.identities().reference(Slot::Patient).unwrap();

        builder.put(
            Slot::MessageHeader,
            json!({"resourceType": "MessageHeader", "focus": [{"reference": patient}]}),
        );
        builder.put(Slot::Patient, json!({"resourceType": "Patient"}));

        let bundle = builder.finish();
        let urls: Vec<&str> = bundle["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["fullUrl"].as_str().unwrap())
            .collect();

        let reference = bundle["entry"][0]["resource"]["focus"][0]["reference"]
            .as_str()
            .unwrap();

        assert!(urls.contains(&reference), "{reference} dangles: {urls:?}");
    }

    #[test]
    fn the_clinical_slot_takes_its_type_from_the_message_type() {
        // §2.2 — one slot, six possible resource types.
        assert_eq!(
            Slot::Clinical.resource_type(Clinical::ServiceRequest),
            "ServiceRequest"
        );
        assert_eq!(
            Slot::Clinical.resource_type(Clinical::Procedure),
            "Procedure"
        );
        assert_eq!(
            Slot::Clinical.resource_type(Clinical::Appointment),
            "Appointment"
        );
        assert_eq!(
            Slot::Clinical.resource_type(Clinical::Encounter),
            "Encounter"
        );

        // Every other slot ignores it.
        assert_eq!(Slot::Patient.resource_type(Clinical::Procedure), "Patient");
    }

    #[test]
    fn an_empty_bundle_is_still_a_well_formed_message_bundle() {
        // Not reachable through the real builders — the MessageHeader and
        // Patient are always filled — but a bundle with no entries must
        // not be a panic or a missing key.
        let bundle = Builder::new(referral(), &SequentialUuids::new()).finish();

        assert_eq!(bundle["entry"].as_array().unwrap().len(), 0);
        assert_eq!(bundle["type"], "message");
    }
}
