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

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::assert_is_empty,
    clippy::unused_async_trait_impl,
    clippy::missing_docs_in_private_items,
    clippy::needless_pass_by_value,
    clippy::needless_collect
)]

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

    let ours: [(&str, &str); 39] = [
        ("MESSAGE_HEADER_PROFILE", fhir::MESSAGE_HEADER_PROFILE),
        ("PATIENT_PROFILE", fhir::PATIENT_PROFILE),
        ("PRACTITIONER_PROFILE", fhir::PRACTITIONER_PROFILE),
        ("ORGANIZATION_PROFILE", fhir::ORGANIZATION_PROFILE),
        ("LOCATION_PROFILE", fhir::LOCATION_PROFILE),
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
        ("SPOKEN_LANGUAGE_SYSTEM", fhir::SPOKEN_LANGUAGE_SYSTEM),
        ("ETHNIC_CATEGORY_EXTENSION", fhir::ETHNIC_CATEGORY_EXTENSION),
        ("ETHNICITY_SYSTEM", fhir::ETHNICITY_SYSTEM),
        ("WPAS_PRACTITIONER_SYSTEM", fhir::WPAS_PRACTITIONER_SYSTEM),
        ("WPAS_SOURCE_ENDPOINT_BASE", fhir::WPAS_SOURCE_ENDPOINT_BASE),
        (
            "PROMPTLY_DESTINATION_ENDPOINT_ENV_VAR",
            fhir::PROMPTLY_DESTINATION_ENDPOINT_ENV_VAR,
        ),
        (
            "ENCOUNTER_IDENTIFIER_SYSTEM",
            fhir::ENCOUNTER_IDENTIFIER_SYSTEM,
        ),
        (
            "APPOINTMENT_IDENTIFIER_SYSTEM",
            fhir::APPOINTMENT_IDENTIFIER_SYSTEM,
        ),
        ("ENCOUNTER_CLASS_SYSTEM", fhir::ENCOUNTER_CLASS_SYSTEM),
        ("WPAS_IG_BASE", fhir::WPAS_IG_BASE),
        (
            "WPAS_IG_MESSAGE_BUNDLE_PROFILE",
            fhir::WPAS_IG_MESSAGE_BUNDLE_PROFILE,
        ),
        (
            "WPAS_IG_MESSAGE_HEADER_PROFILE",
            fhir::WPAS_IG_MESSAGE_HEADER_PROFILE,
        ),
        ("WPAS_IG_PATIENT_PROFILE", fhir::WPAS_IG_PATIENT_PROFILE),
        (
            "WPAS_IG_SERVICE_REQUEST_PROFILE",
            fhir::WPAS_IG_SERVICE_REQUEST_PROFILE,
        ),
        (
            "WPAS_IG_PRACTITIONER_ROLE_PROFILE",
            fhir::WPAS_IG_PRACTITIONER_ROLE_PROFILE,
        ),
        (
            "WPAS_IG_PRACTITIONER_PROFILE",
            fhir::WPAS_IG_PRACTITIONER_PROFILE,
        ),
        (
            "WPAS_IG_ORGANIZATION_PROFILE",
            fhir::WPAS_IG_ORGANIZATION_PROFILE,
        ),
        ("WPAS_IG_EVENT_SYSTEM", fhir::WPAS_IG_EVENT_SYSTEM),
        ("WPAS_IG_PATHWAY_SYSTEM", fhir::WPAS_IG_PATHWAY_SYSTEM),
        (
            "WPAS_IG_SERVICE_REQUEST_IDENTIFIER_SYSTEM",
            fhir::WPAS_IG_SERVICE_REQUEST_IDENTIFIER_SYSTEM,
        ),
        (
            "WPAS_IG_PRACTITIONER_ROLE_IDENTIFIER_SYSTEM",
            fhir::WPAS_IG_PRACTITIONER_ROLE_IDENTIFIER_SYSTEM,
        ),
        ("PRACTICE_SETTING_SYSTEM", fhir::PRACTICE_SETTING_SYSTEM),
        (
            "SPOKEN_LANGUAGE_UNKNOWN_CODE",
            fhir::SPOKEN_LANGUAGE_UNKNOWN_CODE,
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
fn the_referral_ig_constants_are_all_under_the_published_ig_base() {
    // spec/16 §10.5 — upstream #435 moved a Referral's profiles and its code
    // and identifier systems under the WPAS Integration IG. The Encounter and
    // Appointment identifier systems were already there (§9.11).
    for url in [
        fhir::WPAS_IG_MESSAGE_BUNDLE_PROFILE,
        fhir::WPAS_IG_MESSAGE_HEADER_PROFILE,
        fhir::WPAS_IG_PATIENT_PROFILE,
        fhir::WPAS_IG_SERVICE_REQUEST_PROFILE,
        fhir::WPAS_IG_PRACTITIONER_ROLE_PROFILE,
        fhir::WPAS_IG_PRACTITIONER_PROFILE,
        fhir::WPAS_IG_ORGANIZATION_PROFILE,
        fhir::WPAS_IG_EVENT_SYSTEM,
        fhir::WPAS_IG_PATHWAY_SYSTEM,
        fhir::WPAS_IG_SERVICE_REQUEST_IDENTIFIER_SYSTEM,
        fhir::WPAS_IG_PRACTITIONER_ROLE_IDENTIFIER_SYSTEM,
        fhir::SPOKEN_LANGUAGE_SYSTEM,
        fhir::ETHNICITY_SYSTEM,
        fhir::ENCOUNTER_IDENTIFIER_SYSTEM,
        fhir::APPOINTMENT_IDENTIFIER_SYSTEM,
    ] {
        assert!(url.starts_with(fhir::WPAS_IG_BASE), "{url}");
    }
}

#[test]
fn the_practice_setting_list_is_the_pythons() {
    // 87 national specialty codes; PractitionerRole.specialty is sent only
    // for one of them (spec/16 §6.8).
    assert_eq!(fhir::PRACTICE_SETTING_CODES.len(), 87);
    assert!(fhir::PRACTICE_SETTING_CODES.iter().all(|c| c.len() == 3));
    let mut sorted = fhir::PRACTICE_SETTING_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 87, "no duplicates");
    assert!(
        !fhir::PRACTICE_SETTING_CODES.contains(&"650"),
        "650 (physiotherapy) is not on it"
    );
}

#[test]
fn the_destination_default_is_the_staging_endpoint() {
    // §10.3 — a production deployment that forgets
    // PROMPTLY_DESTINATION_ENDPOINT sends to staging; this makes that fact
    // visible in the test output rather than only in a comment.
    assert!(fhir::PROMPTLY_ENDPOINT.contains(".stg."));
}

#[test]
fn every_profile_is_a_profile_url() {
    // A `meta.profile` that is not a StructureDefinition URL is a message
    // Promptly rejects.
    for profile in [
        fhir::MESSAGE_HEADER_PROFILE,
        fhir::PATIENT_PROFILE,
        fhir::PRACTITIONER_PROFILE,
        fhir::ORGANIZATION_PROFILE,
        fhir::LOCATION_PROFILE,
        fhir::WPAS_IG_MESSAGE_BUNDLE_PROFILE,
        fhir::WPAS_IG_MESSAGE_HEADER_PROFILE,
        fhir::WPAS_IG_PATIENT_PROFILE,
        fhir::WPAS_IG_SERVICE_REQUEST_PROFILE,
        fhir::WPAS_IG_PRACTITIONER_ROLE_PROFILE,
        fhir::WPAS_IG_PRACTITIONER_PROFILE,
        fhir::WPAS_IG_ORGANIZATION_PROFILE,
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
    // §6.7 and §6.7.1 — measured from the goldens. The first two
    // displays are the mapper's lowercase literals (HL7's own spelling).
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
    // The two #346 rows carry fhir_constants.py's capitalised displays.
    assert_eq!(
        by_name["OUTPATIENT_VISIT"],
        ("AMB", "Ambulatory", "finished")
    );
    assert_eq!(
        by_name["DISCHARGE"],
        ("IMP", "Inpatient encounter", "finished")
    );
    assert_eq!(
        fhir::ENCOUNTER_CLASS_DEFAULT,
        ("AMB", "ambulatory", "in-progress")
    );
}
