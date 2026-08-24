//! The FHIR constants the bundles carry.
//!
//! Implements the systems, profiles and fixed codes named throughout
//! `spec/16-proms-transformer.md` §6.
//!
//! # These reach Promptly
//!
//! Every value here appears in a bundle Promptly validates and stores. A
//! wrong profile URL is a rejected message; a wrong code system is a
//! silently unmatched identifier.

/// `MessageHeader.meta.profile`.
///
/// A reviewer flagged this as incorrect and it is reproduced anyway — see
/// `spec/16-proms-transformer.md` §9.4. Every other profile below is an
/// `nhs.wales` or `hl7.org.uk` URL; this one is Promptly's.
pub const MESSAGE_HEADER_PROFILE: &str =
    "https://fhir.promptly.health/wpas/StructureDefinition/wpas-messageheader";

/// `Patient.meta.profile`.
pub const PATIENT_PROFILE: &str =
    "https://fhir.nhs.wales/StructureDefinition/DataStandardsWales-Patient";

/// `Practitioner.meta.profile`.
pub const PRACTITIONER_PROFILE: &str =
    "https://fhir.nhs.wales/StructureDefinition/DataStandardsWales-Practitioner";

/// `PractitionerRole.meta.profile`.
pub const PRACTITIONER_ROLE_PROFILE: &str =
    "https://fhir.nhs.wales/StructureDefinition/DataStandardsWales-PractitionerRole";

/// `Organization.meta.profile`.
pub const ORGANIZATION_PROFILE: &str =
    "https://fhir.nhs.wales/StructureDefinition/DataStandardsWales-Organization";

/// `Location.meta.profile`.
pub const LOCATION_PROFILE: &str =
    "https://fhir.nhs.wales/StructureDefinition/DataStandardsWales-Location";

/// `ServiceRequest.meta.profile`.
pub const SERVICE_REQUEST_PROFILE: &str =
    "https://fhir.hl7.org.uk/StructureDefinition/UKCore-ServiceRequest";

/// `Encounter.meta.profile`.
pub const ENCOUNTER_PROFILE: &str = "https://fhir.hl7.org.uk/StructureDefinition/UKCore-Encounter";

/// `Procedure.meta.profile`.
pub const PROCEDURE_PROFILE: &str = "https://fhir.hl7.org.uk/StructureDefinition/UKCore-Procedure";

/// `Appointment.meta.profile`.
pub const APPOINTMENT_PROFILE: &str =
    "https://fhir.hl7.org.uk/StructureDefinition/UKCore-Appointment";

/// The fixed `MessageHeader.destination` (§6.2).
pub const PROMPTLY_DESTINATION_NAME: &str = "FHIR Promptly Collect";

/// The fixed `MessageHeader.destination.endpoint` (§6.2).
pub const PROMPTLY_ENDPOINT: &str = "https://collect.promptlyhealth.com/fhir";

/// `Patient.identifier[NHS].system`.
pub const NHS_NUMBER_SYSTEM: &str = "https://fhir.nhs.uk/Id/nhs-number";

/// The NHS-number verification extension (§6.3).
pub const NHS_NUMBER_VERIFICATION_EXTENSION: &str =
    "https://fhir.hl7.org.uk/StructureDefinition/Extension-UKCore-NHSNumberVerificationStatus";

/// The verification status code system.
///
/// Note the `Wales` suffix — the extension URL above has none, and the two
/// are easy to write as a matching pair by mistake.
pub const NHS_NUMBER_VERIFICATION_SYSTEM: &str =
    "https://fhir.hl7.org.uk/CodeSystem/UKCore-NHSNumberVerificationStatusWales";

/// `Organization.identifier.system`.
pub const ODS_ORGANISATION_CODE_SYSTEM: &str = "https://fhir.nhs.uk/Id/ods-organization-code";

/// `Practitioner.identifier.system`.
///
/// Used for values that are plainly not GMC numbers — see §9.6.
pub const GMC_NUMBER_SYSTEM: &str = "https://fhir.hl7.org.uk/Id/gmc-number";

/// `Location.identifier.system`.
pub const LOCATION_IDENTIFIER_SYSTEM: &str = "https://fhir.nhs.wales/Id/wrts-location-identifier";

/// `Patient.communication.language.coding.system`.
pub const HUMAN_LANGUAGE_SYSTEM: &str = "https://fhir.hl7.org.uk/CodeSystem/UKCore-HumanLanguage";

/// `Encounter.class.system`.
pub const ENCOUNTER_CLASS_SYSTEM: &str = "http://terminology.hl7.org/CodeSystem/v3-ActCode";

/// `ServiceRequest.identifier.system`.
///
/// Note the host: `tools.labs.promptly.health`. See §9.11.
pub const SERVICE_REQUEST_IDENTIFIER_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-servicerequest";

/// `PractitionerRole.identifier.system`.
///
/// Note the host: `tools.labs.promptly.health`. See §9.11.
pub const PRACTITIONER_ROLE_IDENTIFIER_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-practitionerrole";

/// `ServiceRequest.status` (§6.4).
pub const SERVICE_REQUEST_STATUS: &str = "active";

/// `ServiceRequest.intent` (§6.4).
pub const SERVICE_REQUEST_INTENT: &str = "plan";

/// `Procedure.status` (§6.5).
///
/// The mapping spreadsheet says `Yes`, which is not a FHIR `EventStatus`.
/// See §9.5.
pub const PROCEDURE_STATUS: &str = "completed";

/// `Appointment.participant.status` (§6.6).
///
/// Required 1..1 and unspecified by the spreadsheet. See §9.5.
pub const APPOINTMENT_PARTICIPANT_STATUS: &str = "accepted";

/// One `Encounter.class` and the status that goes with it (§6.7).
///
/// The displays are **lowercase**, which is what the Python emits and what
/// HL7's own v3-ActCode spells — `fhir_constants.py` carries capitalised
/// copies that the mapper does not use.
pub const ENCOUNTER_CLASSES: [(&str, &str, &str, &str); 2] = [
    (
        "INPATIENT_ADMISSION",
        "IMP",
        "inpatient encounter",
        "in-progress",
    ),
    ("PREADMISSION", "PRENC", "pre-admission", "planned"),
];

/// The class for a message type §6.7 does not name.
pub const ENCOUNTER_CLASS_DEFAULT: (&str, &str, &str) = ("AMB", "ambulatory", "in-progress");
