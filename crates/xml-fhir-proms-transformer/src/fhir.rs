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

/// `Patient.meta.profile` (`spec/16-proms-transformer.md` §6.3).
///
/// Upstream #423 (`fhir_constants.py`) replaced `DataStandardsWales-Patient`
/// with the WPAS-specific profile when the Referral mapping was revised.
pub const PATIENT_PROFILE: &str = "https://fhir.nhs.wales/StructureDefinition/wpas-patient";

/// `Practitioner.meta.profile` (§6.9).
pub const PRACTITIONER_PROFILE: &str =
    "https://fhir.nhs.wales/StructureDefinition/DataStandardsWales-Practitioner";

/// `Organization.meta.profile` (the sending health board).
pub const ORGANIZATION_PROFILE: &str =
    "https://fhir.nhs.wales/StructureDefinition/DataStandardsWales-Organization";

/// `Location.meta.profile`.
pub const LOCATION_PROFILE: &str =
    "https://fhir.nhs.wales/StructureDefinition/DataStandardsWales-Location";

/// The Promptly WPAS Integration IG (upstream `#435`): the profiles and code
/// systems the **Referral** bundle claims. Other bundles keep the Wales and UK
/// Core profiles above.
pub const WPAS_IG_BASE: &str = "https://wpas-integration-ig.tools.labs.promptly.health";

/// `Bundle.meta.profile` of a Referral (§3.1).
pub const WPAS_IG_MESSAGE_BUNDLE_PROFILE: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/StructureDefinition/wpas-messagebundle";

/// `MessageHeader.meta.profile` of a Referral (§6.2).
pub const WPAS_IG_MESSAGE_HEADER_PROFILE: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/StructureDefinition/wpas-messageheader";

/// `Patient.meta.profile` of a Referral (§6.3); it omits the NHS-number
/// verification status (upstream `#440`, story 621936).
pub const WPAS_IG_PATIENT_PROFILE: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/StructureDefinition/wpas-patient";

/// `ServiceRequest.meta.profile` (§6.4).
pub const WPAS_IG_SERVICE_REQUEST_PROFILE: &str = "https://wpas-integration-ig.tools.labs.promptly.health/StructureDefinition/wpas-servicerequest";

/// `PractitionerRole.meta.profile` (§6.8).
pub const WPAS_IG_PRACTITIONER_ROLE_PROFILE: &str = "https://wpas-integration-ig.tools.labs.promptly.health/StructureDefinition/wpas-practitionerrole";

/// `Practitioner.meta.profile` of a Referral's two practitioners (§6.9).
pub const WPAS_IG_PRACTITIONER_PROFILE: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/StructureDefinition/wpas-practitioner";

/// `Organization.meta.profile` of a Referral (§6.10).
pub const WPAS_IG_ORGANIZATION_PROFILE: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/StructureDefinition/wpas-organization";

/// `MessageHeader.eventCoding.system` of a Referral (§6.2).
pub const WPAS_IG_EVENT_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/CodeSystem/wpas-event-codes";

/// `ServiceRequest.code.coding.system`: the pathway, a national map value such
/// as `GENERIC` (§6.4; confirmed by Promptly, 28 Oct 2026).
pub const WPAS_IG_PATHWAY_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/CodeSystem/wpas-proms-pathway";

/// `ServiceRequest.identifier.system` (§6.4).
pub const WPAS_IG_SERVICE_REQUEST_IDENTIFIER_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-servicerequest";

/// `PractitionerRole.identifier.system` (§6.8): the consultant's code.
pub const WPAS_IG_PRACTITIONER_ROLE_IDENTIFIER_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-practitionerrole";

/// `PractitionerRole.specialty.coding.system` (§6.8).
pub const PRACTICE_SETTING_SYSTEM: &str =
    "https://fhir.hl7.org.uk/CodeSystem/UKCore-PracticeSettingCode";

/// The national specialty list: `PractitionerRole.specialty` is sent only for
/// a code in it (§6.8).
pub const PRACTICE_SETTING_CODES: &[&str] = &[
    "100", "101", "107", "110", "120", "130", "140", "141", "142", "143", "145", "146", "147",
    "148", "149", "150", "160", "170", "171", "180", "190", "192", "200", "300", "301", "302",
    "303", "304", "305", "310", "311", "313", "314", "315", "317", "320", "321", "325", "326",
    "330", "340", "350", "352", "360", "361", "370", "371", "400", "401", "410", "420", "421",
    "430", "450", "451", "460", "500", "501", "502", "504", "560", "600", "601", "700", "710",
    "711", "712", "713", "715", "800", "810", "820", "821", "822", "823", "824", "830", "831",
    "833", "834", "900", "901", "902", "903", "904", "950", "960",
];

/// `Encounter.meta.profile` (§6.7); UK Core.
pub const ENCOUNTER_PROFILE: &str = "https://fhir.hl7.org.uk/StructureDefinition/UKCore-Encounter";

/// `Procedure.meta.profile` (§6.5); UK Core.
pub const PROCEDURE_PROFILE: &str = "https://fhir.hl7.org.uk/StructureDefinition/UKCore-Procedure";

/// `Appointment.meta.profile` (§6.6); UK Core.
pub const APPOINTMENT_PROFILE: &str =
    "https://fhir.hl7.org.uk/StructureDefinition/UKCore-Appointment";

/// The fixed `MessageHeader.destination.name` (§6.2); the endpoint beside it
/// is configurable (§10.3), the name is not.
pub const PROMPTLY_DESTINATION_NAME: &str = "FHIR Promptly Collect";

/// The default `MessageHeader.destination.endpoint` (§6.2, §10.3).
///
/// This is Promptly's **staging** `$process-message` URL, which is what
/// upstream #423 hard-codes as the fallback. The endpoint differs per
/// environment, so a deployment sets `PROMPTLY_DESTINATION_ENDPOINT`
/// ([`PROMPTLY_DESTINATION_ENDPOINT_ENV_VAR`]); leaving it unset sends
/// production messages to staging, which is why §10.3 says it MUST be set.
pub const PROMPTLY_ENDPOINT: &str =
    "https://wpas-fhir.stg.labs.promptly.health/fhir/r4/$process-message";

/// The environment variable that overrides [`PROMPTLY_ENDPOINT`] (§10.3).
pub const PROMPTLY_DESTINATION_ENDPOINT_ENV_VAR: &str = "PROMPTLY_DESTINATION_ENDPOINT";

/// Base of `MessageHeader.source.endpoint`; the `SYSTEM_ID` is appended
/// after a `/` (§5.2.2).
pub const WPAS_SOURCE_ENDPOINT_BASE: &str = "https://fhir.nhs.uk/source-system/wpas";

/// `Patient.identifier[NHS].system` (§6.3): the national NHS number
/// namespace.
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

/// `Organization.identifier.system` (§6.10): the ODS organisation code
/// system; the value is the board's ODS code.
pub const ODS_ORGANISATION_CODE_SYSTEM: &str = "https://fhir.nhs.uk/Id/ods-organization-code";

/// `Practitioner.identifier.system`.
///
/// Used for values that are plainly not GMC numbers — see §9.6.
pub const GMC_NUMBER_SYSTEM: &str = "https://fhir.hl7.org.uk/Id/gmc-number";

/// `Practitioner.identifier[0].system` — the WPAS `consultant_code` (§6.9).
///
/// Upstream #423 stopped filing the consultant code under the GMC system
/// (§9.6) and gave it its own namespace; the GMC system is kept for the
/// referrer's code only.
pub const WPAS_PRACTITIONER_SYSTEM: &str = "https://fhir.nhs.wales/Id/wpas-practitioner";

/// `Location.identifier.system` (§6.11).
pub const LOCATION_IDENTIFIER_SYSTEM: &str = "https://fhir.nhs.wales/Id/wrts-location-identifier";

/// `Patient.communication.language.coding.system` (§6.3.2).
///
/// Replaces the UK Core `HumanLanguage` system: the spoken-language code is
/// now emitted as WPAS sent it, so it is filed under a WPAS code system
/// rather than one whose codes it may not belong to (upstream #423).
pub const SPOKEN_LANGUAGE_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/CodeSystem/wpas-spoken-language";

/// The WPAS spoken-language code meaning *Unknown*: the communication entry
/// is omitted for it (§6.3.2; upstream `#435`).
pub const SPOKEN_LANGUAGE_UNKNOWN_CODE: &str = "9";

/// The `Patient` ethnic-category extension URL (§6.3); UK Core's, with the
/// WPAS coding system in [`ETHNICITY_SYSTEM`].
pub const ETHNIC_CATEGORY_EXTENSION: &str =
    "https://fhir.hl7.org.uk/StructureDefinition/Extension-UKCore-EthnicCategory";

/// The coding system of the ethnic-category extension (§6.3).
pub const ETHNICITY_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/CodeSystem/wpas-ethnicity";

/// `Encounter.class.system` (§6.7): HL7 v3 ActCode, from which
/// [`ENCOUNTER_CLASSES`] draws its codes.
pub const ENCOUNTER_CLASS_SYSTEM: &str = "http://terminology.hl7.org/CodeSystem/v3-ActCode";

/// `Encounter.identifier.system` (§6.7, upstream #346).
///
/// Still on the `tools.labs.promptly.health` host that §9.11 criticised for
/// the other two; upstream moved only the ServiceRequest and
/// PractitionerRole systems.
pub const ENCOUNTER_IDENTIFIER_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-encounter";

/// `Appointment.identifier.system` (§6.6, upstream #346). Same host caveat
/// as [`ENCOUNTER_IDENTIFIER_SYSTEM`].
pub const APPOINTMENT_IDENTIFIER_SYSTEM: &str =
    "https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-appointment";

/// `ServiceRequest.status` (§6.4): always `active`; it is not derived from
/// the payload.
pub const SERVICE_REQUEST_STATUS: &str = "active";

/// `ServiceRequest.intent` (§6.4). `order` since upstream #423 (was `plan`).
pub const SERVICE_REQUEST_INTENT: &str = "order";

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
/// The first two displays are **lowercase**, which is what the Python's
/// mapper literals emit and what HL7's own v3-ActCode spells. The last two
/// (outpatient, discharge) are the **capitalised** `fhir_constants.py`
/// copies, because upstream #346 wrote them that way (§6.7.1). The
/// discharge row reuses the `IMP` code: v3 ActCode has no discharge class,
/// so only `status` differs (a TEMP placeholder upstream).
pub const ENCOUNTER_CLASSES: [(&str, &str, &str, &str); 4] = [
    (
        "INPATIENT_ADMISSION",
        "IMP",
        "inpatient encounter",
        "in-progress",
    ),
    ("PREADMISSION", "PRENC", "pre-admission", "planned"),
    ("OUTPATIENT_VISIT", "AMB", "Ambulatory", "finished"),
    ("DISCHARGE", "IMP", "Inpatient encounter", "finished"),
];

/// The `(code, display, status)` for a message type §6.7 does not name:
/// ambulatory, in progress. Lowercase display, like the first two table rows.
pub const ENCOUNTER_CLASS_DEFAULT: (&str, &str, &str) = ("AMB", "ambulatory", "in-progress");
