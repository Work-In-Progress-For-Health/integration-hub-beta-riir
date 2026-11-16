//! The nine resource mappers.
//!
//! Implements `spec/16-proms-transformer.md` §6.2 to §6.11, as revised by
//! upstream #346, #370 and #423 (§10).
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
//! §5.2.2's id-derived endpoint, §6.6's `end` equal to `start`, and the
//! fixed statuses of §9.5.

use serde_json::{Map, Value, json};

use crate::bundle::{Identities, Slot};
use crate::fhir;
use crate::mapping::{join_display, to_fhir_date, to_fhir_datetime};
use crate::message_type::{Clinical, MessageType};
use crate::parse::PromsMessage;
use crate::reference_data::{ReferenceDataResolver, dha_code_name, nhs_certification_display};
use crate::source_system::{SourceSystem, endpoint_for, lookup, pas_identifier_system};

/// A resource's `meta.profile`: the single FHIR profile URL it claims
/// conformance to (the constants live in [`crate::fhir`]).
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

/// A `Reference` to another entry (§4.3), or `None` when that slot has no
/// identity in this bundle shape. Callers also test the slot is *present*,
/// so a reference never points at an omitted entry.
fn reference(identities: &Identities, slot: Slot, resource_type: &str) -> Option<Value> {
    Some(json!({
        "reference": identities.reference(slot)?,
        "type": resource_type,
    }))
}

/// The sending health board (§5): the board named by `system_id`, falling back
/// to `hbCode`, the same lookup the header's `source` and the patient's PAS
/// identifier use, so every resource agrees on the board.
fn board_of(message: &PromsMessage) -> Option<SourceSystem> {
    lookup(&system_id_of(message))
}

/// The sending health board's name (§6.10, §6.2 `sender.display`).
///
/// The board (by `system_id`, else `hbCode`) if it is one of the seven;
/// otherwise the ODS-code lookup on `hb_code`. [`dha_code_name`] echoes an
/// unknown code back (§7.1.2), so a result equal to the code is discarded
/// rather than published as a name (§9.10). Upstream `#435` dropped the
/// requirement that `hb_code` be present: a known board is enough.
///
/// # Examples
///
/// ```
/// use xml_fhir_proms_transformer::parse::parse;
/// use xml_fhir_proms_transformer::resources::health_board_name;
///
/// let message = parse(
///     "<R><system_id>149</system_id><hbCode>X98</hbCode></R>",
/// ).unwrap();
/// assert_eq!(
///     health_board_name(&message).as_deref(),
///     Some("Hywel Dda University Health Board")
/// );
///
/// // The board is enough without an hbCode.
/// let bare = parse("<R><system_id>149</system_id></R>").unwrap();
/// assert_eq!(
///     health_board_name(&bare).as_deref(),
///     Some("Hywel Dda University Health Board")
/// );
///
/// // An unknown board and an unknown code is no name at all.
/// let unknown = parse("<R><system_id>999</system_id><hbCode>X98</hbCode></R>").unwrap();
/// assert_eq!(health_board_name(&unknown), None);
/// ```
#[must_use]
pub fn health_board_name(message: &PromsMessage) -> Option<String> {
    if let Some(board) = board_of(message) {
        return Some(board.name.to_string());
    }
    let hb_code = message.get(&["hb_code", "hbCode", "HB_CODE"])?;
    dha_code_name(hb_code).filter(|n| !n.is_empty() && n != hb_code)
}

/// §6.2's `MessageHeader`.
///
/// Always present — FHIR requires it of a `message` bundle (§3.1).
///
/// `destination_endpoint` is the `PROMPTLY_DESTINATION_ENDPOINT` value
/// (§10.3), read once by the caller so this function stays pure; `None`
/// uses [`fhir::PROMPTLY_ENDPOINT`].
#[must_use]
pub fn message_header(
    message: &PromsMessage,
    message_type: &MessageType,
    identities: &Identities,
    present: &[Slot],
    endpoint_override: &dyn Fn(&str) -> Option<String>,
    destination_endpoint: Option<&str>,
) -> Value {
    let system_id = system_id_of(message);

    let mut source = Map::new();
    source.insert(
        "endpoint".to_string(),
        json!(endpoint_for(&system_id, endpoint_override)),
    );
    // §5.2.1 — the name only for a known board, unlike the endpoint.
    if let Some(board) = lookup(&system_id) {
        source.insert("name".to_string(), json!(board.name));
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

    // The Referral's practitioners follow the ServiceRequest (upstream `#435`).
    let is_referral = message_type.has_practitioner_role;
    let mut linked: Vec<(Slot, &str)> =
        vec![(Slot::Clinical, message_type.clinical.resource_type())];
    if is_referral {
        linked.push((Slot::Referrer, "Practitioner"));
        linked.push((Slot::ConsultantRole, "PractitionerRole"));
    }
    for (slot, resource_type) in linked {
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
    // A Referral claims the WPAS IG's profile and event system, and its event
    // codes are lowercase; every other bundle is unchanged (upstream `#435`).
    header.insert(
        "meta".to_string(),
        profile_meta(if is_referral {
            fhir::WPAS_IG_MESSAGE_HEADER_PROFILE
        } else {
            fhir::MESSAGE_HEADER_PROFILE
        }),
    );
    header.insert(
        "eventCoding".to_string(),
        json!({
            "system": if is_referral {
                fhir::WPAS_IG_EVENT_SYSTEM
            } else {
                crate::message_type::EVENT_SYSTEM
            },
            // Upstream #346: the raw routing field, aliases included, so a
            // legacy bare `SURGERY` stays `SURGERY` rather than becoming
            // the canonical `SURGERY-PROC` (§10.2). A Referral's is the
            // canonical code, lowercased.
            "code": if is_referral {
                message_type.event_code.to_ascii_lowercase()
            } else {
                message
                    .get(&[
                        "eventCode", "event_code", "EVENT_CODE",
                        "MESSAGE_TYPE", "messageType", "MSG_TYPE", "msgType",
                    ])
                    .unwrap_or(message_type.event_code)
                    .to_string()
            },
            "display": message_type.event_display,
        }),
    );
    header.insert("source".to_string(), Value::Object(source));
    header.insert(
        "destination".to_string(),
        json!([{
            "name": fhir::PROMPTLY_DESTINATION_NAME,
            "endpoint": destination_endpoint.unwrap_or(fhir::PROMPTLY_ENDPOINT),
        }]),
    );
    // The WPAS IG has no MessageDefinition.
    if !is_referral {
        header.insert(
            "definition".to_string(),
            json!(format!(
                "{}{}",
                crate::message_type::DEFINITION_BASE,
                message_type.definition_suffix
            )),
        );
    }

    // §6.2 — the sender names the Organization entry, when there is one.
    if present.contains(&Slot::Organization) {
        let mut sender = Map::new();
        sender.insert(
            "reference".to_string(),
            json!(identities.reference(Slot::Organization).unwrap_or_default()),
        );
        sender.insert("type".to_string(), json!("Organization"));
        // #423: the health board's name (the same as `Organization.name`),
        // no longer the free-text `referrer_org`.
        put(
            &mut sender,
            "display",
            health_board_name(message).map(|n| json!(n)),
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
    is_referral: bool,
) -> Value {
    let system_id = system_id_of(message);

    let mut identifiers = Vec::new();

    if let Some(nhs_number) = message.get(&["nhsNumber", "NHS_NUMBER"]) {
        let mut identifier = Map::new();
        identifier.insert("system".to_string(), json!(fhir::NHS_NUMBER_SYSTEM));
        identifier.insert("value".to_string(), json!(nhs_number));

        // The WPAS IG Patient profile omits the verification status
        // (upstream `#440`, story 621936); every other bundle keeps it.
        if !is_referral
            && let Some(certification) = message.get(&["NHS_CERTIFICATION", "nhsCertification"])
        {
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
    patient.insert(
        "meta".to_string(),
        profile_meta(if is_referral {
            fhir::WPAS_IG_PATIENT_PROFILE
        } else {
            fhir::PATIENT_PROFILE
        }),
    );
    patient.insert("active".to_string(), json!(true));

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

    put(&mut patient, "telecom", patient_telecom(message));

    // §6.3 — `deceased*` is always considered since #423 (the Python's
    // `include_deceased` switch is gone).
    match deceased(message) {
        Some(Deceased::At(date_time)) => {
            patient.insert("deceasedDateTime".to_string(), json!(date_time));
        }
        Some(Deceased::Unknown) => {
            patient.insert("deceasedBoolean".to_string(), json!(true));
        }
        None => {}
    }

    put(&mut patient, "extension", ethnic_category(message));

    // §6.3.2 — spoken language as sent, under the WPAS system; the
    // reference-data resolver is no longer consulted for it (upstream
    // #423), only for gender.
    put(&mut patient, "communication", communication(message));

    Value::Object(patient)
}

/// §6.3's `Patient.name`.
///
/// Omitted entirely unless a family **or** a first given name is present
/// (§6.3.1). Upstream #423: first and middle names are **one**
/// space-joined `given` value, and the title is no longer a prefix.
fn patient_name(message: &PromsMessage) -> Option<Value> {
    let family = message.get(&["patientSurname", "SURNAME", "surname"]);
    let first = message.get(&["patientFirstname", "FORENAME", "forename"]);
    let middle = message.get(&["patientMiddlename", "patientMiddleName"]);

    if family.is_none() && first.is_none() {
        return None;
    }

    // `" ".join(p for p in (first, middle) if p)` upstream.
    let given: Vec<&str> = [first, middle].into_iter().flatten().collect();
    let given = given.join(" ");

    // No `use`: upstream `#439` dropped `official`. The title is a prefix
    // again (it had been dropped by `#423`).
    let mut name = Map::new();
    put(&mut name, "family", family.map(|f| json!(f)));
    if !given.is_empty() {
        name.insert("given".to_string(), json!([given]));
    }
    if let Some(title) = message.get(&["patientTitle"]) {
        name.insert("prefix".to_string(), json!([title]));
    }

    Some(Value::Object(name))
}

/// §6.3's `Patient.address`: `None` unless at least one address part is
/// present, so a patient with no address has no `address` element at all.
fn patient_address(message: &PromsMessage) -> Option<Value> {
    let postal_code = message.get(&["postCode", "POSTCODE", "postalCode"]);
    let building = message.get(&["buildingName", "ADDRESS_1"]);
    let street = message.get(&["streetRoadName", "streetroadname"]);
    let city = message.get(&["postTown", "ADDRESS_2"]);
    // Upstream #346: `postalCounty` is the FHIR `district`.
    let district = message.get(&["postalCounty", "postal_county"]);

    if postal_code.is_none()
        && building.is_none()
        && street.is_none()
        && city.is_none()
        && district.is_none()
    {
        return None;
    }

    let mut address = Map::new();

    // §6.3 — building and street joined into one line.
    let line: Vec<&str> = [building, street].into_iter().flatten().collect();
    if !line.is_empty() {
        address.insert("line".to_string(), json!([line.join(", ")]));
    }

    put(&mut address, "city", city.map(|c| json!(c)));
    put(&mut address, "district", district.map(|d| json!(d)));
    put(&mut address, "postalCode", postal_code.map(|p| json!(p)));

    Some(Value::Object(address))
}

/// What §6.3.3 makes of a death date. The `Unknown` arm exists so a patient
/// recorded as deceased is never silently emitted as alive just because the
/// date text was unreadable.
enum Deceased {
    /// A parseable date: `deceasedDateTime`.
    At(String),
    /// Non-empty but unparseable: deceased, date unknown.
    Unknown,
}

/// §6.3.3's `deceased`.
///
/// `DOD`, then the legacy `DEATHDATE`. Absent (which §2.5.2 makes the same
/// as empty) yields nothing. A non-empty value that does not parse is
/// `Unknown` — `deceasedBoolean = true` — so a patient is never silently
/// alive because of a date format (upstream `_deceased`).
fn deceased(message: &PromsMessage) -> Option<Deceased> {
    let raw = message.get(&["DOD", "dod", "DEATHDATE", "deathDate"])?;

    Some(to_fhir_datetime(raw).map_or(Deceased::Unknown, Deceased::At))
}

/// One `Patient.telecom` source: WPAS aliases, `system`, optional `use`.
type TelecomField = (&'static [&'static str], &'static str, Option<&'static str>);

/// §6.3's `Patient.telecom`: one `ContactPoint` per populated field.
///
/// The table's order is upstream's `_TELECOM_FIELDS` and is the output
/// order. `email` carries no `use`.
fn patient_telecom(message: &PromsMessage) -> Option<Value> {
    const FIELDS: [TelecomField; 4] = [
        (
            &["home_phone", "homePhone", "TELEPHONE_DAY"],
            "phone",
            Some("home"),
        ),
        (&["work_phone", "workPhone"], "phone", Some("work")),
        (&["mobile_phone", "mobilePhone"], "phone", Some("mobile")),
        (&["email", "EMAIL"], "email", None),
    ];

    let telecom: Vec<Value> = FIELDS
        .iter()
        .filter_map(|(aliases, system, usage)| {
            let value = message.get(aliases)?;
            let mut point = Map::new();
            point.insert("system".to_string(), json!(system));
            point.insert("value".to_string(), json!(value));
            put(&mut point, "use", usage.map(|u| json!(u)));
            Some(Value::Object(point))
        })
        .collect();

    (!telecom.is_empty()).then(|| json!(telecom))
}

/// §6.3's ethnic-category extension (a one-element `extension` array).
fn ethnic_category(message: &PromsMessage) -> Option<Value> {
    let code = message.get(&["ethnic_origin_code", "ethnicOriginCode"]);
    let display = message.get(&["ethnic_origin", "ethnicOrigin"]);

    if code.is_none() && display.is_none() {
        return None;
    }

    let mut coding = Map::new();
    coding.insert("system".to_string(), json!(fhir::ETHNICITY_SYSTEM));
    put(&mut coding, "code", code.map(|c| json!(c)));
    put(&mut coding, "display", display.map(|d| json!(d)));

    let mut concept = Map::new();
    concept.insert("coding".to_string(), json!([Value::Object(coding)]));
    put(&mut concept, "text", display.map(|d| json!(d)));

    Some(json!([{
        "url": fhir::ETHNIC_CATEGORY_EXTENSION,
        "valueCodeableConcept": Value::Object(concept),
    }]))
}

/// §6.3.2's `Patient.communication`: the spoken language as sent.
///
/// `PREFERRED_LANGUAGE` and `preferredLanguage` are legacy aliases kept by
/// upstream for older payloads. There is no `preferred` flag.
fn communication(message: &PromsMessage) -> Option<Value> {
    let code = message.get(&[
        "preferred_spoken_language_code",
        "preferredSpokenLanguageCode",
        "PREFERRED_LANGUAGE",
        "preferredLanguage",
    ]);
    let display = message.get(&["spoken_language", "spokenLanguage"]);

    if code.is_none() && display.is_none() {
        return None;
    }
    // WPAS code `9` means Unknown: say nothing rather than file it.
    if code.is_some_and(|c| c.trim() == fhir::SPOKEN_LANGUAGE_UNKNOWN_CODE) {
        return None;
    }

    let mut coding = Map::new();
    coding.insert("system".to_string(), json!(fhir::SPOKEN_LANGUAGE_SYSTEM));
    put(&mut coding, "code", code.map(|c| json!(c)));
    put(&mut coding, "display", display.map(|d| json!(d)));

    Some(json!([{ "language": { "coding": [Value::Object(coding)] } }]))
}

/// §6.4's `ServiceRequest`: the clinical resource of a Referral bundle.
///
/// `present` lists the slots actually filled, so a `requester` or `performer`
/// reference is emitted only when its target resource is in the bundle.
///
/// Revised by upstream `#435`: the WPAS IG profile and identifier system;
/// `code` is the **pathway** under the WPAS pathway system (the event type is
/// already `MessageHeader.eventCoding`); no `reasonCode`; `occurrenceDateTime`
/// is a plain date, because a midnight-UTC time shifts an hour in BST; the
/// `requester` is the referrer `Practitioner` and the `performer` the
/// consultant `Practitioner`.
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
        profile_meta(fhir::WPAS_IG_SERVICE_REQUEST_PROFILE),
    );
    request.insert("status".to_string(), json!(fhir::SERVICE_REQUEST_STATUS));
    request.insert("intent".to_string(), json!(fhir::SERVICE_REQUEST_INTENT));
    // #423: always `false`; the mapping fixes it rather than reading it.
    request.insert("doNotPerform".to_string(), json!(false));
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
                "system": fhir::WPAS_IG_SERVICE_REQUEST_IDENTIFIER_SYSTEM,
                "value": identifier,
            }]),
        );
    }

    if let Some(pathway) = message.get(&["eventPathway", "event_pathway"]) {
        request.insert(
            "code".to_string(),
            json!({ "coding": [{
                "system": fhir::WPAS_IG_PATHWAY_SYSTEM,
                "code": pathway,
            }] }),
        );
    }

    put(
        &mut request,
        "occurrenceDateTime",
        to_fhir_date(message.get_or_empty(&["eventDate", "event_date"])).map(|d| json!(d)),
    );

    if present.contains(&Slot::Referrer) {
        put(
            &mut request,
            "requester",
            reference(identities, Slot::Referrer, "Practitioner"),
        );
    }
    if present.contains(&Slot::Consultant) {
        put(
            &mut request,
            "performer",
            reference(identities, Slot::Consultant, "Practitioner").map(|r| json!([r])),
        );
    }

    Value::Object(request)
}

/// §6.5's `Procedure`: the clinical resource of the surgery and pre-op
/// bundles. `status` is the fixed `completed` of §9.5.
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

/// §6.6's `Appointment.participant[0].status` for the patient.
///
/// The one confirmed business rule in the mapping (upstream
/// `_patient_participant_status`, #346): `CONFIRM_APPT` `Y` is `accepted`,
/// `N` is `declined`, and anything else — including absent — is
/// `tentative`, never `accepted`, so an unconfirmed appointment is not
/// presented as confirmed.
fn patient_participant_status(message: &PromsMessage) -> &'static str {
    let confirm = message.get_or_empty(&["CONFIRM_APPT", "confirmAppt", "confirm_appt"]);

    match confirm.trim().to_ascii_uppercase().as_str() {
        "Y" => "accepted",
        "N" => "declined",
        _ => "tentative",
    }
}

/// §6.6's `Appointment`: scheduled, rescheduled or cancelled, selected by
/// `message_type.appointment_status`. `present` gates the practitioner
/// participant, as for [`service_request`].
#[must_use]
pub fn appointment(
    message: &PromsMessage,
    message_type: &MessageType,
    identities: &Identities,
    present: &[Slot],
) -> Value {
    let mut participants = vec![json!({
        "actor": reference(identities, Slot::Patient, "Patient"),
        "status": patient_participant_status(message),
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

    // Upstream #346: `activityNotekey` identifies the appointment.
    if let Some(key) = message.get(&["activityNotekey", "activity_note_key"]) {
        appointment.insert(
            "identifier".to_string(),
            json!([{ "system": fhir::APPOINTMENT_IDENTIFIER_SYSTEM, "value": key }]),
        );
    }

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

/// §6.7's `Encounter`: the class and status come from
/// [`fhir::ENCOUNTER_CLASSES`] by message type name, falling back to the
/// `AMB` default for a type the table does not name.
#[must_use]
pub fn encounter(
    message: &PromsMessage,
    message_type: &MessageType,
    identities: &Identities,
) -> Value {
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

    // Upstream #346: `activityNotekey` identifies the encounter.
    if let Some(key) = message.get(&["activityNotekey", "activity_note_key"]) {
        encounter.insert(
            "identifier".to_string(),
            json!([{ "system": fhir::ENCOUNTER_IDENTIFIER_SYSTEM, "value": key }]),
        );
    }

    Value::Object(encounter)
}

/// §6.8's `PractitionerRole`: the consultant's, in a Referral bundle only.
///
/// Its identifier is the `consultant_code`. The specialty is sent only when
/// the three characters at the start of `consultant_specialty` are in the
/// national practice-setting list ([`fhir::PRACTICE_SETTING_CODES`]); an
/// unlisted code is dropped rather than filed under a system it is not in.
/// Characters, not bytes, as Python slices.
#[must_use]
pub fn consultant_role(message: &PromsMessage, identities: &Identities, present: &[Slot]) -> Value {
    let mut role = Map::new();
    role.insert("resourceType".to_string(), json!("PractitionerRole"));
    role.insert(
        "id".to_string(),
        json!(identities.get(Slot::ConsultantRole).unwrap_or_default()),
    );
    role.insert(
        "meta".to_string(),
        profile_meta(fhir::WPAS_IG_PRACTITIONER_ROLE_PROFILE),
    );

    if let Some(code) = message.get(&["consultant_code", "consultantCode"]) {
        role.insert(
            "identifier".to_string(),
            json!([{
                "system": fhir::WPAS_IG_PRACTITIONER_ROLE_IDENTIFIER_SYSTEM,
                "value": code,
            }]),
        );
    }

    put(
        &mut role,
        "practitioner",
        reference(identities, Slot::Consultant, "Practitioner"),
    );
    if present.contains(&Slot::Organization) {
        put(
            &mut role,
            "organization",
            reference(identities, Slot::Organization, "Organization"),
        );
    }

    let specialty_code: String = message
        .get_or_empty(&["consultant_specialty", "consultantSpecialty"])
        .trim()
        .chars()
        .take(3)
        .collect();
    if fhir::PRACTICE_SETTING_CODES.contains(&specialty_code.as_str()) {
        let mut coding = Map::new();
        coding.insert("system".to_string(), json!(fhir::PRACTICE_SETTING_SYSTEM));
        coding.insert("code".to_string(), json!(specialty_code));
        put(
            &mut coding,
            "display",
            message
                .get(&["specialty_name", "specialtyName", "main_specialty_name"])
                .map(|d| json!(d)),
        );
        role.insert(
            "specialty".to_string(),
            json!([{ "coding": [Value::Object(coding)] }]),
        );
    }

    Value::Object(role)
}

/// A Referral's `Practitioner`: an optional identifier under the board's own
/// system, and a free-text name. `None` when there is neither (§6.9.1).
///
/// The identifier needs a **known board**: there is no valid system for an
/// unknown one, so it is omitted rather than filed under a guess. WPAS sends
/// no GMC or GMP numbers, so the referrer's code goes under the board's own
/// `wpas-referrer-code` (DHCW's decision, upstream `#435`).
fn referral_practitioner(
    message: &PromsMessage,
    identities: &Identities,
    slot: Slot,
    code: Option<&str>,
    name: Option<&str>,
    system_of: fn(&SourceSystem) -> &'static str,
) -> Option<Value> {
    let board = board_of(message);
    let identifier = code
        .zip(board)
        .map(|(code, board)| json!([{ "system": system_of(&board), "value": code }]));
    if identifier.is_none() && name.is_none() {
        return None;
    }

    let mut practitioner = Map::new();
    practitioner.insert("resourceType".to_string(), json!("Practitioner"));
    practitioner.insert(
        "id".to_string(),
        json!(identities.get(slot).unwrap_or_default()),
    );
    practitioner.insert(
        "meta".to_string(),
        profile_meta(fhir::WPAS_IG_PRACTITIONER_PROFILE),
    );
    put(&mut practitioner, "identifier", identifier);
    put(
        &mut practitioner,
        "name",
        name.map(|text| json!([{ "text": text }])),
    );
    Some(Value::Object(practitioner))
}

/// §6.9's referring GP, for a Referral bundle (`referrer_code`,
/// `referrer_name`).
#[must_use]
pub fn referrer(message: &PromsMessage, identities: &Identities) -> Option<Value> {
    referral_practitioner(
        message,
        identities,
        Slot::Referrer,
        message.get(&["referrer_code", "referrerCode"]),
        message.get(&["referrer_name", "referrerName"]),
        |board| board.referrer_code_system,
    )
}

/// §6.9's consultant, for a Referral bundle (`consultant_code`,
/// `clinicianName`).
#[must_use]
pub fn consultant(message: &PromsMessage, identities: &Identities) -> Option<Value> {
    referral_practitioner(
        message,
        identities,
        Slot::Consultant,
        message.get(&["consultant_code", "consultantCode"]),
        message.get(&["clinicianName", "clinician_name", "CONS_NAME"]),
        |board| board.consultant_code_system,
    )
}

/// §6.9's `Practitioner` for every bundle but a Referral.
///
/// `None` when there is neither an identifier nor a name (§6.9.1). The
/// identifier is `consultant_code` under the WPAS practitioner system; the
/// name is `clinicianName` as **free text** (`name.text`), not split into
/// family and given (§6.9.2).
#[must_use]
pub fn practitioner(message: &PromsMessage, identities: &Identities) -> Option<Value> {
    let consultant_code = message.get(&["consultant_code", "consultantCode"]);
    let name_source = message.get(&["clinicianName", "clinician_name", "CONS_NAME"]);

    if consultant_code.is_none() && name_source.is_none() {
        return None;
    }

    let mut practitioner = Map::new();
    practitioner.insert("resourceType".to_string(), json!("Practitioner"));
    practitioner.insert(
        "id".to_string(),
        json!(identities.get(Slot::Practitioner).unwrap_or_default()),
    );
    practitioner.insert("meta".to_string(), profile_meta(fhir::PRACTITIONER_PROFILE));
    put(
        &mut practitioner,
        "identifier",
        consultant_code
            .map(|code| json!([{ "system": fhir::WPAS_PRACTITIONER_SYSTEM, "value": code }])),
    );
    put(
        &mut practitioner,
        "name",
        name_source.map(|text| json!([{ "text": text }])),
    );

    Some(Value::Object(practitioner))
}

/// §6.10's `Organization`: the sending health board.
///
/// The identifier is the **board's ODS code** where the board is known
/// (`hbCode` carries placeholders such as `X98`), else `hb_code`; `None` when
/// neither gives one (§6.10.1). `profile` is the WPAS IG's for a Referral
/// (upstream `#435`). The name is [`health_board_name`].
#[must_use]
pub fn organization(
    message: &PromsMessage,
    identities: &Identities,
    is_referral: bool,
) -> Option<Value> {
    let ods_code = board_of(message)
        .map(|board| board.ods_code.to_string())
        .or_else(|| {
            message
                .get(&["hb_code", "hbCode", "HB_CODE"])
                .map(str::to_string)
        })?;

    let mut organization = Map::new();
    organization.insert("resourceType".to_string(), json!("Organization"));
    organization.insert(
        "id".to_string(),
        json!(identities.get(Slot::Organization).unwrap_or_default()),
    );
    organization.insert(
        "meta".to_string(),
        profile_meta(if is_referral {
            fhir::WPAS_IG_ORGANIZATION_PROFILE
        } else {
            fhir::ORGANIZATION_PROFILE
        }),
    );
    organization.insert("active".to_string(), json!(true));
    organization.insert(
        "identifier".to_string(),
        json!([{ "system": fhir::ODS_ORGANISATION_CODE_SYSTEM, "value": ods_code }]),
    );
    put(
        &mut organization,
        "name",
        health_board_name(message).map(|n| json!(n)),
    );

    Some(Value::Object(organization))
}

/// §6.11's `Location`.
///
/// **Always present**, unlike [`practitioner`] and [`organization`]: the
/// Python's `map_location` returns a `Location` unconditionally, so a
/// bundle with no location data still carries an entry (now holding an id,
/// a profile and `status`). See §9.13.
///
/// Upstream #423: `identifier` is `referrer_prac`; there is no `name`;
/// `address.text` is `referrer_location` and `referrer_postcode` joined by
/// a space, so a location string that already ends in the postcode repeats
/// it (§6.11). The mapping says so; it is reproduced, not tidied.
#[must_use]
pub fn location(message: &PromsMessage, identities: &Identities) -> Value {
    let mut location = Map::new();
    location.insert("resourceType".to_string(), json!("Location"));
    location.insert(
        "id".to_string(),
        json!(identities.get(Slot::Location).unwrap_or_default()),
    );
    location.insert("meta".to_string(), profile_meta(fhir::LOCATION_PROFILE));
    location.insert("status".to_string(), json!("active"));

    if let Some(prac) = message.get(&["referrer_prac", "referrerPrac"]) {
        location.insert(
            "identifier".to_string(),
            json!([{ "system": fhir::LOCATION_IDENTIFIER_SYSTEM, "value": prac }]),
        );
    }

    let place = message.get(&["referrer_location", "referrerLocation"]);
    let postcode = message.get(&["referrer_postcode", "referrerPostcode"]);
    let text: Vec<&str> = [place, postcode].into_iter().flatten().collect();
    if !text.is_empty() {
        let mut address = Map::new();
        address.insert("text".to_string(), json!(text.join(" ")));
        put(&mut address, "postalCode", postcode.map(|p| json!(p)));
        location.insert("address".to_string(), Value::Object(address));
    }

    Value::Object(location)
}

/// The `SYSTEM_ID`, falling back to `hbCode` (§6.2); empty when neither is
/// present, which selects the `unknown` source of §5.2.2.
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

/// The clinical resource for a bundle (§2.2): dispatches on
/// `message_type.clinical` to the one builder that bundle shape uses.
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
        Clinical::Encounter => encounter(message, message_type, identities),
    }
}
