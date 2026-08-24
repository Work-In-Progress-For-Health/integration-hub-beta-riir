//! The nine resource mappers.
//!
//! Implements `spec/16-proms-transformer.md` §6.2 to §6.11.
//!
//! # Everything here can decline
//!
//! §6.0.2: a resource whose every source field is absent is **omitted from
//! the bundle** rather than emitted empty. So the mappers that can decline
//! return `Option`, and [`crate::bundle::Builder::put_maybe`] turns a
//! `None` into a missing entry — which is what shortens a minimal
//! referral from seven entries to five.
//!
//! # And nothing here invents a value
//!
//! §6.0.1. An absent or unparseable source leaves its element unset. The
//! exceptions are all §9's findings rather than choices made here:
//! §5.2.2's placeholder endpoint, §6.6's `end` equal to `start`, and the
//! fixed statuses of §9.5.

use serde_json::{Map, Value, json};

use crate::bundle::{Identities, Slot};
use crate::fhir;
use crate::mapping::{join_display, split_person_name, to_fhir_date, to_fhir_datetime};
use crate::message_type::{Clinical, MessageType};
use crate::parse::PromsMessage;
use crate::reference_data::{ReferenceDataResolver, dha_code_name, nhs_certification_display};
use crate::source_system::{endpoint_for, lookup, pas_identifier_system};

/// A resource's `meta.profile`.
fn profile_meta(profile: &str) -> Value {
    json!({ "profile": [profile] })
}

/// Insert `key` only when `value` is present, so an absent source leaves
/// the element unset rather than null (§6.0.1).
fn put(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        object.insert(key.to_string(), value);
    }
}

/// A `Reference` to another entry (§4.3).
fn reference(identities: &Identities, slot: Slot, resource_type: &str) -> Option<Value> {
    Some(json!({
        "reference": identities.reference(slot)?,
        "type": resource_type,
    }))
}

/// §6.2's `MessageHeader`.
///
/// Always present — FHIR requires it of a `message` bundle (§3.1).
#[must_use]
pub fn message_header(
    message: &PromsMessage,
    message_type: &MessageType,
    identities: &Identities,
    present: &[Slot],
    endpoint_override: &dyn Fn(&str) -> Option<String>,
) -> Value {
    let system_id = system_id_of(message);

    let mut source = Map::new();
    source.insert(
        "endpoint".to_string(),
        json!(endpoint_for(&system_id, endpoint_override)),
    );
    // §5.2.1 — omitted for an undocumented board, unlike the endpoint.
    if let Some(name) = lookup(&system_id).and_then(|board| board.name) {
        source.insert("name".to_string(), json!(name));
    }

    // §6.2's focus: the patient first, then whichever of the other slots
    // this bundle actually carries, in a fixed order.
    let mut focus = vec![json!({
        "reference": identities.reference(Slot::Patient).unwrap_or_default(),
        "type": "Patient",
        "display": join_display(&[
            message.get_or_empty(&["patientFirstname", "FORENAME", "forename"]),
            message.get_or_empty(&["patientSurname", "SURNAME", "surname"]),
        ]),
    })];
    // Drop a null display rather than emitting one.
    if focus[0]["display"].is_null() {
        focus[0].as_object_mut().map(|o| o.remove("display"));
    }

    for (slot, resource_type) in [
        (Slot::Clinical, message_type.clinical.resource_type()),
        (Slot::PractitionerRole, "PractitionerRole"),
    ] {
        if present.contains(&slot)
            && let Some(entry) = reference(identities, slot, resource_type)
        {
            focus.push(entry);
        }
    }

    let mut header = Map::new();
    header.insert("resourceType".to_string(), json!("MessageHeader"));
    header.insert(
        "id".to_string(),
        json!(identities.get(Slot::MessageHeader).unwrap_or_default()),
    );
    header.insert(
        "meta".to_string(),
        profile_meta(fhir::MESSAGE_HEADER_PROFILE),
    );
    header.insert(
        "eventCoding".to_string(),
        json!({
            "system": crate::message_type::EVENT_SYSTEM,
            "code": message
                .get(&["eventCode", "event_code", "EVENT_CODE"])
                .unwrap_or(message_type.event_code),
            "display": message_type.event_display,
        }),
    );
    header.insert("source".to_string(), Value::Object(source));
    header.insert(
        "destination".to_string(),
        json!([{
            "name": fhir::PROMPTLY_DESTINATION_NAME,
            "endpoint": fhir::PROMPTLY_ENDPOINT,
        }]),
    );
    header.insert(
        "definition".to_string(),
        json!(format!(
            "{}{}",
            crate::message_type::DEFINITION_BASE,
            message_type.definition_suffix
        )),
    );

    // §6.2 — the sender names the Organization entry, when there is one.
    if present.contains(&Slot::Organization) {
        let mut sender = Map::new();
        sender.insert(
            "reference".to_string(),
            json!(identities.reference(Slot::Organization).unwrap_or_default()),
        );
        sender.insert("type".to_string(), json!("Organization"));
        put(
            &mut sender,
            "display",
            message
                .get(&["referrer_org", "referrerOrg"])
                .map(|v| json!(v)),
        );
        header.insert("sender".to_string(), Value::Object(sender));
    }

    header.insert("focus".to_string(), json!(focus));
    Value::Object(header)
}

/// §6.3's `Patient`.
///
/// Always present: a bundle about a patient with no patient is not a
/// bundle, and every fixture carries at least an NHS number.
#[must_use]
pub fn patient(
    message: &PromsMessage,
    identities: &Identities,
    resolver: &dyn ReferenceDataResolver,
) -> Value {
    let system_id = system_id_of(message);

    let mut identifiers = Vec::new();

    if let Some(nhs_number) = message.get(&["nhsNumber", "NHS_NUMBER"]) {
        let mut identifier = Map::new();
        identifier.insert("system".to_string(), json!(fhir::NHS_NUMBER_SYSTEM));
        identifier.insert("value".to_string(), json!(nhs_number));

        if let Some(certification) = message.get(&["NHS_CERTIFICATION", "nhsCertification"]) {
            let mut coding = Map::new();
            coding.insert(
                "system".to_string(),
                json!(fhir::NHS_NUMBER_VERIFICATION_SYSTEM),
            );
            coding.insert("code".to_string(), json!(certification));
            // §7.1.2 — the code itself when unrecognised (§9.10).
            put(
                &mut coding,
                "display",
                nhs_certification_display(certification).map(|d| json!(d)),
            );

            identifier.insert(
                "extension".to_string(),
                json!([{
                    "url": fhir::NHS_NUMBER_VERIFICATION_EXTENSION,
                    "valueCodeableConcept": { "coding": [Value::Object(coding)] },
                }]),
            );
        }

        identifiers.push(Value::Object(identifier));
    }

    // §6.3 — omitted unless the board has a documented PAS system, rather
    // than filing a real identifier under a guessed namespace.
    if let Some(unit_number) = message.get(&["crn", "UNIT_NUMBER", "unitNumber"])
        && let Some(system) = pas_identifier_system(&system_id)
    {
        identifiers.push(json!({ "system": system, "value": unit_number }));
    }

    let mut patient = Map::new();
    patient.insert("resourceType".to_string(), json!("Patient"));
    patient.insert(
        "id".to_string(),
        json!(identities.get(Slot::Patient).unwrap_or_default()),
    );
    patient.insert("meta".to_string(), profile_meta(fhir::PATIENT_PROFILE));

    if !identifiers.is_empty() {
        patient.insert("identifier".to_string(), json!(identifiers));
    }

    put(
        &mut patient,
        "name",
        patient_name(message).map(|n| json!([n])),
    );

    put(
        &mut patient,
        "gender",
        resolver
            .resolve_gender(message.get_or_empty(&["gender", "SEX", "sex"]), &system_id)
            .map(|g| json!(g)),
    );

    put(
        &mut patient,
        "birthDate",
        to_fhir_date(message.get_or_empty(&["dob", "BIRTHDATE", "birthDate"])).map(|d| json!(d)),
    );

    put(
        &mut patient,
        "address",
        patient_address(message).map(|a| json!([a])),
    );

    // §6.3.2 — the resolved code when there is one, the raw display text
    // otherwise, so a language is not silently lost. §9.9: with the static
    // resolver this never produces anything.
    let language_code = resolver.resolve_language(
        message.get_or_empty(&[
            "preferred_spoken_language_code",
            "PREFERRED_LANGUAGE",
            "preferredLanguage",
        ]),
        &system_id,
    );
    let language_display = message.get(&["spoken_language", "spokenLanguage"]);

    if language_code.is_some() || language_display.is_some() {
        let mut coding = Map::new();
        coding.insert("system".to_string(), json!(fhir::HUMAN_LANGUAGE_SYSTEM));
        put(&mut coding, "code", language_code.map(|c| json!(c)));
        put(&mut coding, "display", language_display.map(|d| json!(d)));

        patient.insert(
            "communication".to_string(),
            json!([{
                "language": { "coding": [Value::Object(coding)] },
                "preferred": true,
            }]),
        );
    }

    Value::Object(patient)
}

/// §6.3's `Patient.name`.
///
/// Omitted entirely unless a family **or** a first given name is present
/// (§6.3.1) — a name carrying only a title is not a name.
fn patient_name(message: &PromsMessage) -> Option<Value> {
    let family = message.get(&["patientSurname", "SURNAME", "surname"]);
    let first = message.get(&["patientFirstname", "FORENAME", "forename"]);
    let middle = message.get(&["patientMiddlename", "patientMiddleName"]);
    let title = message.get(&["patientTitle", "TITLE", "title"]);

    if family.is_none() && first.is_none() {
        return None;
    }

    let given: Vec<&str> = [first, middle].into_iter().flatten().collect();

    let mut name = Map::new();
    name.insert("use".to_string(), json!("official"));
    put(&mut name, "family", family.map(|f| json!(f)));
    if !given.is_empty() {
        name.insert("given".to_string(), json!(given));
    }
    put(&mut name, "prefix", title.map(|t| json!([t])));

    Some(Value::Object(name))
}

/// §6.3's `Patient.address`.
fn patient_address(message: &PromsMessage) -> Option<Value> {
    let postal_code = message.get(&["postCode", "POSTCODE", "postalCode"]);
    let building = message.get(&["buildingName", "ADDRESS_1"]);
    let street = message.get(&["streetRoadName", "streetroadname"]);
    let city = message.get(&["postTown", "ADDRESS_2"]);

    if postal_code.is_none() && building.is_none() && street.is_none() && city.is_none() {
        return None;
    }

    let mut address = Map::new();

    // §6.3 — building and street joined into one line.
    let line: Vec<&str> = [building, street].into_iter().flatten().collect();
    if !line.is_empty() {
        address.insert("line".to_string(), json!([line.join(", ")]));
    }

    put(&mut address, "city", city.map(|c| json!(c)));
    put(&mut address, "postalCode", postal_code.map(|p| json!(p)));

    Some(Value::Object(address))
}

/// §6.4's `ServiceRequest`.
#[must_use]
pub fn service_request(message: &PromsMessage, identities: &Identities, present: &[Slot]) -> Value {
    let mut request = Map::new();
    request.insert("resourceType".to_string(), json!("ServiceRequest"));
    request.insert(
        "id".to_string(),
        json!(identities.get(Slot::Clinical).unwrap_or_default()),
    );
    request.insert(
        "meta".to_string(),
        profile_meta(fhir::SERVICE_REQUEST_PROFILE),
    );
    request.insert("status".to_string(), json!(fhir::SERVICE_REQUEST_STATUS));
    request.insert("intent".to_string(), json!(fhir::SERVICE_REQUEST_INTENT));
    put(
        &mut request,
        "subject",
        reference(identities, Slot::Patient, "Patient"),
    );

    // §9.5 — no source field was specified; `pathway` is the best available.
    if let Some(identifier) = message.get(&["pathway", "unique_code", "uniqueCode", "UNIQUE_ID"]) {
        request.insert(
            "identifier".to_string(),
            json!([{
                "system": fhir::SERVICE_REQUEST_IDENTIFIER_SYSTEM,
                "value": identifier,
            }]),
        );
    }

    if let Some(pathway) = message.get(&["eventPathway", "event_pathway"]) {
        request.insert(
            "code".to_string(),
            json!({ "coding": [{ "code": pathway, "display": "Referral" }] }),
        );
    }

    put(
        &mut request,
        "occurrenceDateTime",
        to_fhir_datetime(message.get_or_empty(&["eventDate", "event_date"])).map(|d| json!(d)),
    );

    if present.contains(&Slot::PractitionerRole) {
        put(
            &mut request,
            "requester",
            reference(identities, Slot::PractitionerRole, "PractitionerRole"),
        );
    }

    Value::Object(request)
}

/// §6.5's `Procedure`.
#[must_use]
pub fn procedure(message: &PromsMessage, identities: &Identities) -> Value {
    let mut procedure = Map::new();
    procedure.insert("resourceType".to_string(), json!("Procedure"));
    procedure.insert(
        "id".to_string(),
        json!(identities.get(Slot::Clinical).unwrap_or_default()),
    );
    procedure.insert("meta".to_string(), profile_meta(fhir::PROCEDURE_PROFILE));
    // §9.5 — the spreadsheet says `Yes`, which is not a FHIR EventStatus.
    procedure.insert("status".to_string(), json!(fhir::PROCEDURE_STATUS));
    put(
        &mut procedure,
        "subject",
        reference(identities, Slot::Patient, "Patient"),
    );

    if let Some(pathway) = message.get(&["eventPathway", "event_pathway"]) {
        procedure.insert(
            "code".to_string(),
            json!({ "coding": [{ "code": pathway, "display": "Procedure performed" }] }),
        );
    }

    put(
        &mut procedure,
        "performedDateTime",
        appointment_datetime(message).map(|d| json!(d)),
    );

    Value::Object(procedure)
}

/// §6.6's `Appointment`.
#[must_use]
pub fn appointment(
    message: &PromsMessage,
    message_type: &MessageType,
    identities: &Identities,
    present: &[Slot],
) -> Value {
    let mut participants = vec![json!({
        "actor": reference(identities, Slot::Patient, "Patient"),
        "status": fhir::APPOINTMENT_PARTICIPANT_STATUS,
    })];

    if present.contains(&Slot::Practitioner) {
        participants.push(json!({
            "actor": reference(identities, Slot::Practitioner, "Practitioner"),
            "status": fhir::APPOINTMENT_PARTICIPANT_STATUS,
        }));
    }

    let mut appointment = Map::new();
    appointment.insert("resourceType".to_string(), json!("Appointment"));
    appointment.insert(
        "id".to_string(),
        json!(identities.get(Slot::Clinical).unwrap_or_default()),
    );
    appointment.insert("meta".to_string(), profile_meta(fhir::APPOINTMENT_PROFILE));
    appointment.insert(
        "status".to_string(),
        json!(message_type.appointment_status.unwrap_or("booked")),
    );
    appointment.insert("participant".to_string(), json!(participants));

    if let Some(service_type) =
        message.get(&["main_specialty_name", "mainSpecialtyName", "specialty_name"])
    {
        appointment.insert(
            "serviceType".to_string(),
            json!([{ "coding": [{ "display": service_type }] }]),
        );
    }

    // §6.6 and §9.5 — WPAS supplies no duration, so `end` equals `start`.
    // A zero-length appointment is what a downstream calendar receives.
    if let Some(start) = appointment_datetime(message) {
        appointment.insert("start".to_string(), json!(start));
        appointment.insert("end".to_string(), json!(start));
    }

    Value::Object(appointment)
}

/// §6.7's `Encounter`.
#[must_use]
pub fn encounter(message_type: &MessageType, identities: &Identities) -> Value {
    let (code, display, status) = fhir::ENCOUNTER_CLASSES
        .iter()
        .find(|(name, ..)| *name == message_type.name)
        .map_or(
            fhir::ENCOUNTER_CLASS_DEFAULT,
            |(_, code, display, status)| (*code, *display, *status),
        );

    let mut encounter = Map::new();
    encounter.insert("resourceType".to_string(), json!("Encounter"));
    encounter.insert(
        "id".to_string(),
        json!(identities.get(Slot::Clinical).unwrap_or_default()),
    );
    encounter.insert("meta".to_string(), profile_meta(fhir::ENCOUNTER_PROFILE));
    encounter.insert("status".to_string(), json!(status));
    encounter.insert(
        "class".to_string(),
        json!({ "system": fhir::ENCOUNTER_CLASS_SYSTEM, "code": code, "display": display }),
    );
    put(
        &mut encounter,
        "subject",
        reference(identities, Slot::Patient, "Patient"),
    );

    Value::Object(encounter)
}

/// §6.8's `PractitionerRole`.
#[must_use]
pub fn practitioner_role(
    message: &PromsMessage,
    identities: &Identities,
    present: &[Slot],
) -> Value {
    let mut role = Map::new();
    role.insert("resourceType".to_string(), json!("PractitionerRole"));
    role.insert(
        "id".to_string(),
        json!(identities.get(Slot::PractitionerRole).unwrap_or_default()),
    );
    role.insert(
        "meta".to_string(),
        profile_meta(fhir::PRACTITIONER_ROLE_PROFILE),
    );

    // §9.5 — no source field was specified.
    if let Some(identifier) = message.get(&["activityNotekey", "activity_notekey"]) {
        role.insert(
            "identifier".to_string(),
            json!([{
                "system": fhir::PRACTITIONER_ROLE_IDENTIFIER_SYSTEM,
                "value": identifier,
            }]),
        );
    }

    if present.contains(&Slot::Practitioner) {
        put(
            &mut role,
            "practitioner",
            reference(identities, Slot::Practitioner, "Practitioner"),
        );
    }
    if present.contains(&Slot::Organization) {
        put(
            &mut role,
            "organization",
            reference(identities, Slot::Organization, "Organization"),
        );
    }

    let specialty_code = message.get(&["consultant_specialty", "consultantSpecialty"]);
    let specialty_name = message.get(&["specialty_name", "specialtyName", "main_specialty_name"]);

    if specialty_code.is_some() || specialty_name.is_some() {
        let mut coding = Map::new();
        put(&mut coding, "code", specialty_code.map(|c| json!(c)));
        put(&mut coding, "display", specialty_name.map(|d| json!(d)));
        role.insert(
            "specialty".to_string(),
            json!([{ "coding": [Value::Object(coding)] }]),
        );
    }

    if present.contains(&Slot::Location) {
        role.insert(
            "location".to_string(),
            json!([reference(identities, Slot::Location, "Location")]),
        );
    }

    Value::Object(role)
}

/// §6.9's `Practitioner`.
///
/// `None` when neither an identifier nor a name is available (§6.9.1).
#[must_use]
pub fn practitioner(
    message: &PromsMessage,
    message_type: &MessageType,
    identities: &Identities,
) -> Option<Value> {
    // §6.9 — the source fields depend on the bundle.
    let is_referral = message_type.code == "REFERRAL";

    let identifier = if is_referral {
        message.get(&["referrer_code", "referrerCode"])
    } else {
        message.get(&["consultant_code", "consultantCode"])
    };
    let name_source = if is_referral {
        message.get(&["referrer_name", "referrerName"])
    } else {
        message.get(&["clinicianName", "clinician_name", "CONS_NAME"])
    };

    if identifier.is_none() && name_source.is_none() {
        return None;
    }

    let mut practitioner = Map::new();
    practitioner.insert("resourceType".to_string(), json!("Practitioner"));
    practitioner.insert(
        "id".to_string(),
        json!(identities.get(Slot::Practitioner).unwrap_or_default()),
    );
    practitioner.insert("meta".to_string(), profile_meta(fhir::PRACTITIONER_PROFILE));

    // §9.6 — the GMC system, for values that are plainly not GMC numbers.
    if let Some(value) = identifier {
        practitioner.insert(
            "identifier".to_string(),
            json!([{ "system": fhir::GMC_NUMBER_SYSTEM, "value": value }]),
        );
    }

    if let Some(raw) = name_source {
        let split = split_person_name(raw);
        if !split.is_empty() {
            let mut name = Map::new();
            if !split.family.is_empty() {
                name.insert("family".to_string(), json!(split.family));
            }
            if !split.given.is_empty() {
                name.insert("given".to_string(), json!(split.given));
            }
            if !split.prefixes.is_empty() {
                name.insert("prefix".to_string(), json!(split.prefixes));
            }
            practitioner.insert("name".to_string(), json!([Value::Object(name)]));
        }
    }

    Some(Value::Object(practitioner))
}

/// §6.10's `Organization`.
///
/// `None` when neither `referrer_org` nor a DHA code is present (§6.10.1).
#[must_use]
pub fn organization(message: &PromsMessage, identities: &Identities) -> Option<Value> {
    let referrer_org = message.get(&["referrer_org", "referrerOrg"]);
    let dha_code = message.get(&["dhaCode", "DHA_CODE", "dha_code"]);

    if referrer_org.is_none() && dha_code.is_none() {
        return None;
    }

    let mut organization = Map::new();
    organization.insert("resourceType".to_string(), json!("Organization"));
    organization.insert(
        "id".to_string(),
        json!(identities.get(Slot::Organization).unwrap_or_default()),
    );
    organization.insert("meta".to_string(), profile_meta(fhir::ORGANIZATION_PROFILE));

    if let Some(code) = dha_code {
        organization.insert(
            "identifier".to_string(),
            json!([{ "system": fhir::ODS_ORGANISATION_CODE_SYSTEM, "value": code }]),
        );
    }

    // §6.10.1 — the payload's value first, then the lookup. `referrer_org`
    // is present-but-empty in real SIT payloads, which §2.5.2 makes
    // indistinguishable from absent, so the fallback is what runs.
    let name = referrer_org
        .map(ToString::to_string)
        .or_else(|| dha_code.and_then(dha_code_name));
    put(&mut organization, "name", name.map(|n| json!(n)));

    Some(Value::Object(organization))
}

/// §6.11's `Location`.
///
/// **Always present**, unlike [`practitioner`] and [`organization`]. The
/// Python's `map_location` returns a `Location` unconditionally, so a
/// bundle with no location data at all still carries an entry holding
/// nothing but an id and a profile.
///
/// That is measurable: `EXPLICIT_EVENT_CODE_MESSAGE` has no
/// `referrer_location` and no `referrer_postcode`, and its bundle still
/// ends with a `Location`. Returning `None` here — which reads as the
/// obvious symmetry with the two mappers above — drops an entry the
/// Python emits. See §9.13.
#[must_use]
pub fn location(message: &PromsMessage, identities: &Identities) -> Value {
    let referrer_location = message.get(&["referrer_location", "referrerLocation"]);
    let postcode = message.get(&["referrer_postcode", "referrerPostcode"]);

    let mut location = Map::new();
    location.insert("resourceType".to_string(), json!("Location"));
    location.insert(
        "id".to_string(),
        json!(identities.get(Slot::Location).unwrap_or_default()),
    );
    location.insert("meta".to_string(), profile_meta(fhir::LOCATION_PROFILE));

    if let Some(raw) = referrer_location {
        location.insert(
            "identifier".to_string(),
            json!([{ "system": fhir::LOCATION_IDENTIFIER_SYSTEM, "value": raw }]),
        );

        // §6.11.1 and §9.5 — the first comma-delimited segment, because
        // the spreadsheet marks the name's source as "??".
        let first = raw.split(',').next().unwrap_or_default().trim();
        if !first.is_empty() {
            location.insert("name".to_string(), json!(first));
        }
    }

    if let Some(postcode) = postcode {
        location.insert("address".to_string(), json!({ "postalCode": postcode }));
    }

    Value::Object(location)
}

/// The `SYSTEM_ID`, falling back to `hbCode` (§6.2).
fn system_id_of(message: &PromsMessage) -> String {
    message
        .get(&["system_id", "SYSTEM_ID", "systemId"])
        .or_else(|| message.get(&["hbCode", "hb_code", "HB_CODE"]))
        .unwrap_or_default()
        .to_string()
}

/// `appointmentDate` and `appointmentTime`, concatenated (§6.5, §6.6).
fn appointment_datetime(message: &PromsMessage) -> Option<String> {
    let date = message.get_or_empty(&["appointmentDate", "appointment_date"]);
    let at = message.get_or_empty(&["appointmentTime", "appointment_time"]);

    let combined = format!("{date} {at}");
    let combined = combined.trim();

    if combined.is_empty() {
        return None;
    }

    to_fhir_datetime(combined)
}

/// The clinical resource for a bundle (§2.2).
#[must_use]
pub fn clinical(
    message: &PromsMessage,
    message_type: &MessageType,
    identities: &Identities,
    present: &[Slot],
) -> Value {
    match message_type.clinical {
        Clinical::ServiceRequest => service_request(message, identities, present),
        Clinical::Procedure => procedure(message, identities),
        Clinical::Appointment => appointment(message, message_type, identities, present),
        Clinical::Encounter => encounter(message_type, identities),
    }
}
