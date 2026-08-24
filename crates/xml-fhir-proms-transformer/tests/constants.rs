//! Every FHIR constant, against the Python's.
//!
//! These are systems, profiles and extension URLs that reach Promptly. A
//! wrong one is not a compile error and not a test failure anywhere else:
//! it is an identifier Promptly files under the wrong namespace, or a
//! profile it rejects the message for.
//!
//! Four of these were **guessed wrong** on a first pass — two identifier
//! systems and a code system whose name differs from its own extension's
//! by a `Wales` suffix. That is why they are checked against the source
//! rather than eyeballed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use xml_fhir_proms_transformer::fhir;

/// Every constant `fhir_constants.py` defines, by name.
fn python_constants() -> BTreeMap<String, String> {
    let raw = include_str!("corpus/python-constants.json");
    serde_json::from_str(raw).expect("the constant corpus is valid JSON")
}

#[test]
fn every_ported_constant_matches_the_python() {
    let python = python_constants();

    let ours: [(&str, &str); 22] = [
        ("MESSAGE_HEADER_PROFILE", fhir::MESSAGE_HEADER_PROFILE),
        ("PATIENT_PROFILE", fhir::PATIENT_PROFILE),
        ("PRACTITIONER_PROFILE", fhir::PRACTITIONER_PROFILE),
        ("PRACTITIONER_ROLE_PROFILE", fhir::PRACTITIONER_ROLE_PROFILE),
        ("ORGANIZATION_PROFILE", fhir::ORGANIZATION_PROFILE),
        ("LOCATION_PROFILE", fhir::LOCATION_PROFILE),
        ("SERVICE_REQUEST_PROFILE", fhir::SERVICE_REQUEST_PROFILE),
        ("ENCOUNTER_PROFILE", fhir::ENCOUNTER_PROFILE),
        ("PROCEDURE_PROFILE", fhir::PROCEDURE_PROFILE),
        ("APPOINTMENT_PROFILE", fhir::APPOINTMENT_PROFILE),
        (
            "PROMPTLY_COLLECT_DESTINATION_NAME",
            fhir::PROMPTLY_DESTINATION_NAME,
        ),
        ("PROMPTLY_COLLECT_ENDPOINT", fhir::PROMPTLY_ENDPOINT),
        ("NHS_NUMBER_SYSTEM", fhir::NHS_NUMBER_SYSTEM),
        (
            "NHS_NUMBER_VERIFICATION_EXTENSION",
            fhir::NHS_NUMBER_VERIFICATION_EXTENSION,
        ),
        (
            "NHS_NUMBER_VERIFICATION_SYSTEM",
            fhir::NHS_NUMBER_VERIFICATION_SYSTEM,
        ),
        (
            "ODS_ORGANISATION_CODE_SYSTEM",
            fhir::ODS_ORGANISATION_CODE_SYSTEM,
        ),
        ("GMC_NUMBER_SYSTEM", fhir::GMC_NUMBER_SYSTEM),
        (
            "LOCATION_IDENTIFIER_SYSTEM",
            fhir::LOCATION_IDENTIFIER_SYSTEM,
        ),
        ("HUMAN_LANGUAGE_SYSTEM", fhir::HUMAN_LANGUAGE_SYSTEM),
        ("ENCOUNTER_CLASS_SYSTEM", fhir::ENCOUNTER_CLASS_SYSTEM),
        (
            "SERVICE_REQUEST_IDENTIFIER_SYSTEM",
            fhir::SERVICE_REQUEST_IDENTIFIER_SYSTEM,
        ),
        (
            "PRACTITIONER_ROLE_IDENTIFIER_SYSTEM",
            fhir::PRACTITIONER_ROLE_IDENTIFIER_SYSTEM,
        ),
    ];

    for (name, value) in ours {
        let theirs = python
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not in fhir_constants.py"));

        assert_eq!(value, theirs, "{name}");
    }
}

#[test]
fn the_fixed_status_codes_match() {
    let python = python_constants();

    assert_eq!(
        fhir::SERVICE_REQUEST_STATUS,
        python["SERVICE_REQUEST_STATUS"]
    );
    assert_eq!(
        fhir::SERVICE_REQUEST_INTENT,
        python["SERVICE_REQUEST_INTENT"]
    );
    assert_eq!(fhir::PROCEDURE_STATUS, python["PROCEDURE_STATUS_DEFAULT"]);
    assert_eq!(
        fhir::APPOINTMENT_PARTICIPANT_STATUS,
        python["APPOINTMENT_PARTICIPANT_STATUS_DEFAULT"]
    );
}

#[test]
fn the_verification_system_and_its_extension_differ_by_more_than_a_path() {
    // The trap this test exists for: the extension is
    // `…Extension-UKCore-NHSNumberVerificationStatus` and the code system
    // is `…CodeSystem/UKCore-NHSNumberVerificationStatus**Wales**`. Writing
    // them as a matching pair is the obvious mistake and produces a coding
    // in a code system that does not exist.
    assert!(fhir::NHS_NUMBER_VERIFICATION_SYSTEM.ends_with("Wales"));
    assert!(!fhir::NHS_NUMBER_VERIFICATION_EXTENSION.ends_with("Wales"));
}

#[test]
fn two_identifier_systems_name_a_laboratory_host() {
    // spec/16 §9.11 — asserted rather than described, so the day somebody
    // changes it the change is deliberate. These are the namespaces
    // Promptly files a `ServiceRequest` and a `PractitionerRole` under.
    for system in [
        fhir::SERVICE_REQUEST_IDENTIFIER_SYSTEM,
        fhir::PRACTITIONER_ROLE_IDENTIFIER_SYSTEM,
    ] {
        assert!(
            system.contains("tools.labs.promptly.health"),
            "{system} no longer names the labs host; if that was deliberate, \
             update spec/16 §9.11 and this test together"
        );
    }
}

#[test]
fn every_profile_is_a_profile_url() {
    // A `meta.profile` that is not a StructureDefinition URL is a message
    // Promptly rejects.
    for profile in [
        fhir::MESSAGE_HEADER_PROFILE,
        fhir::PATIENT_PROFILE,
        fhir::PRACTITIONER_PROFILE,
        fhir::PRACTITIONER_ROLE_PROFILE,
        fhir::ORGANIZATION_PROFILE,
        fhir::LOCATION_PROFILE,
        fhir::SERVICE_REQUEST_PROFILE,
        fhir::ENCOUNTER_PROFILE,
        fhir::PROCEDURE_PROFILE,
        fhir::APPOINTMENT_PROFILE,
    ] {
        assert!(profile.starts_with("https://"), "{profile}");
        assert!(profile.contains("/StructureDefinition/"), "{profile}");
    }
}

#[test]
fn the_encounter_classes_use_hl7s_lowercase_displays() {
    // §6.7 — measured from the goldens. `fhir_constants.py` carries
    // capitalised copies the mapper does not use, and HL7's own
    // v3-ActCode spells them lowercase.
    let by_name: BTreeMap<&str, (&str, &str, &str)> = fhir::ENCOUNTER_CLASSES
        .iter()
        .map(|(name, code, display, status)| (*name, (*code, *display, *status)))
        .collect();

    assert_eq!(
        by_name["INPATIENT_ADMISSION"],
        ("IMP", "inpatient encounter", "in-progress")
    );
    assert_eq!(
        by_name["PREADMISSION"],
        ("PRENC", "pre-admission", "planned")
    );
    assert_eq!(
        fhir::ENCOUNTER_CLASS_DEFAULT,
        ("AMB", "ambulatory", "in-progress")
    );
}
