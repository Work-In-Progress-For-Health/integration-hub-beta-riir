//! Synthetic WPAS `PromsEventRequest` samples, one per supported `eventCode`.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §5.1: every
//! name, number and address is invented. NHS numbers are from the `999`
//! test range. The `REFERRAL` sample follows upstream #423 (phones, email,
//! ethnicity, `referrer_prac`, `hbCode` `7A2`), the fields the revised
//! mapping of `spec/16-proms-transformer.md` §6 reads; the contact details
//! are invented and use the reserved `.invalid` domain.

/// The `REFERRAL` sample: carries the contact, ethnicity and referrer fields
/// the revised mapping reads, so it exercises the most of the Bundle.
pub(super) const REFERRAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<PromsEventRequest>
  <system_id>149</system_id>
  <hbCode>7A2</hbCode>
  <eventCode>REFERRAL</eventCode>
  <eventDate>2024-06-12</eventDate>
  <eventPathway>ORTHO</eventPathway>
  <pathway>7A00000001</pathway>
  <activityNotekey>ANK-7A00000001</activityNotekey>
  <nhsNumber>9990000018</nhsNumber>
  <crn>TS0000001</crn>
  <patientTitle>Mr</patientTitle>
  <patientFirstname>Test</patientFirstname>
  <patientMiddlename>Middle</patientMiddlename>
  <patientSurname>Person</patientSurname>
  <gender>M</gender>
  <dob>1980-01-01</dob>
  <buildingName>Test House</buildingName>
  <streetRoadName>Test Street</streetRoadName>
  <postTown>Testtown</postTown>
  <postalCounty>Testshire</postalCounty>
  <postCode>CF10 1EP</postCode>
  <home_phone>07000000001</home_phone>
  <work_phone>07000000002</work_phone>
  <mobile_phone>07000000003</mobile_phone>
  <email>test.person@example.invalid</email>
  <ethnic_origin_code>A</ethnic_origin_code>
  <ethnic_origin>White - British</ethnic_origin>
  <preferred_spoken_language_code>CY</preferred_spoken_language_code>
  <spoken_language>Welsh</spoken_language>
  <referrer_prac>W95001</referrer_prac>
  <referrer_location>UNIVERSITY HOSPITAL TEST, TEST PARK, CARDIFF, CF14 4XW</referrer_location>
  <referrer_postcode>CF14 4XW</referrer_postcode>
  <dhaCode>7A1</dhaCode>
  <consultant_code>TESTA</consultant_code>
  <clinicianName>Tester, Alex</clinicianName>
  <referrer_code>G7654321</referrer_code>
  <referrer_name>TESTER A</referrer_name>
  <referrer_org></referrer_org>
  <consultant_specialty>110</consultant_specialty>
  <main_specialty_name>Trauma and Orthopaedics</main_specialty_name>
  <appointmentDate></appointmentDate>
  <appointmentTime></appointmentTime>
</PromsEventRequest>"#;

/// The `SURGERY` sample: the bare legacy code, which routes as `SURGERY-PROC`
/// to a `Procedure` bundle (`spec/16-proms-transformer.md` §2.4.2).
pub(super) const SURGERY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<PromsEventRequest>
  <system_id>149</system_id>
  <hbCode>X98</hbCode>
  <eventCode>SURGERY</eventCode>
  <eventDate>2024-07-20</eventDate>
  <eventPathway>ORTHO</eventPathway>
  <pathway>7A00000002</pathway>
  <nhsNumber>9990000026</nhsNumber>
  <crn>TS0000002</crn>
  <patientFirstname>Test</patientFirstname>
  <patientSurname>Patient</patientSurname>
  <gender>F</gender>
  <dob>1975-06-30</dob>
  <postCode>CF10 1EP</postCode>
  <referrer_location>UNIVERSITY HOSPITAL TEST, TEST PARK, CARDIFF, CF14 4XW</referrer_location>
  <referrer_postcode>CF14 4XW</referrer_postcode>
  <dhaCode>7A4</dhaCode>
  <consultant_code>TESTB</consultant_code>
  <clinicianName>Tester, Blair</clinicianName>
  <appointmentDate>2024-07-20</appointmentDate>
  <appointmentTime>09:30</appointmentTime>
</PromsEventRequest>"#;

/// The `PREOP` sample: the bare legacy code, which routes as `PREOP-AS` to a
/// `booked` `Appointment` bundle (`spec/16-proms-transformer.md` §2.4.2).
pub(super) const PREOP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<PromsEventRequest>
  <system_id>149</system_id>
  <hbCode>X98</hbCode>
  <eventCode>PREOP</eventCode>
  <eventDate>2024-08-05</eventDate>
  <pathway>7A00000003</pathway>
  <activityNotekey>ANK-7A00000003</activityNotekey>
  <nhsNumber>9990000034</nhsNumber>
  <crn>TS0000003</crn>
  <patientFirstname>Test</patientFirstname>
  <patientSurname>Subject</patientSurname>
  <gender>M</gender>
  <dob>1960-01-20</dob>
  <postCode>SA2 8QA</postCode>
  <referrer_location>TEST HOSPITAL, SWANSEA, SA6 6NL</referrer_location>
  <referrer_postcode>SA6 6NL</referrer_postcode>
  <dhaCode>7A2</dhaCode>
  <consultant_code>TESTC</consultant_code>
  <clinicianName>Tester, Casey</clinicianName>
  <main_specialty_name>Trauma and Orthopaedics</main_specialty_name>
  <appointmentDate>2024-08-15</appointmentDate>
  <appointmentTime>14:00</appointmentTime>
  <CONFIRM_APPT>Y</CONFIRM_APPT>
</PromsEventRequest>"#;

/// The `INPATIENT` sample: routes to an `Encounter` bundle.
pub(super) const INPATIENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<PromsEventRequest>
  <system_id>149</system_id>
  <hbCode>X98</hbCode>
  <eventCode>INPATIENT</eventCode>
  <eventDate>2024-09-10</eventDate>
  <pathway>7A00000004</pathway>
  <activityNotekey>ANK-7A00000004</activityNotekey>
  <nhsNumber>9990000042</nhsNumber>
  <crn>TS0000004</crn>
  <patientFirstname>Test</patientFirstname>
  <patientSurname>Admitted</patientSurname>
  <gender>F</gender>
  <dob>1940-09-02</dob>
  <postCode>SA2 8QA</postCode>
  <referrer_location>TEST HOSPITAL, SWANSEA, SA6 6NL</referrer_location>
  <referrer_postcode>SA6 6NL</referrer_postcode>
  <dhaCode>7A2</dhaCode>
  <consultant_code>TESTD</consultant_code>
  <clinicianName>Tester, Dana</clinicianName>
  <appointmentDate>2024-10-15</appointmentDate>
  <appointmentTime>11:00</appointmentTime>
  <CONFIRM_APPT>N</CONFIRM_APPT>
</PromsEventRequest>"#;

/// The `CANCELLED` sample: routes to a `cancelled` `Appointment` bundle.
pub(super) const CANCELLED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<PromsEventRequest>
  <system_id>149</system_id>
  <hbCode>X98</hbCode>
  <eventCode>CANCELLED</eventCode>
  <eventDate>2024-10-01</eventDate>
  <pathway>7A00000005</pathway>
  <activityNotekey>ANK-7A00000005</activityNotekey>
  <nhsNumber>9990000050</nhsNumber>
  <crn>TS0000005</crn>
  <patientFirstname>Test</patientFirstname>
  <patientSurname>Cancelled</patientSurname>
  <gender>F</gender>
  <dob>1950-03-03</dob>
  <postCode>SA2 8QA</postCode>
  <referrer_location>TEST HOSPITAL, SWANSEA, SA6 6NL</referrer_location>
  <referrer_postcode>SA6 6NL</referrer_postcode>
  <dhaCode>7A2</dhaCode>
  <consultant_code>TESTD</consultant_code>
  <clinicianName>Tester, Dana</clinicianName>
  <appointmentDate>2024-10-15</appointmentDate>
  <appointmentTime>11:00</appointmentTime>
  <CONFIRM_APPT>N</CONFIRM_APPT>
</PromsEventRequest>"#;

/// The `PREREAD` sample: routes to a pre-admission `Encounter` bundle.
pub(super) const PREREAD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<PromsEventRequest>
  <system_id>149</system_id>
  <hbCode>X98</hbCode>
  <eventCode>PREREAD</eventCode>
  <eventDate>2024-11-05</eventDate>
  <pathway>7A00000006</pathway>
  <nhsNumber>9990000069</nhsNumber>
  <crn>TS0000006</crn>
  <patientFirstname>Test</patientFirstname>
  <patientSurname>Preread</patientSurname>
  <gender>M</gender>
  <dob>1978-07-22</dob>
  <postCode>CF10 1EP</postCode>
  <referrer_location>UNIVERSITY HOSPITAL TEST, TEST PARK, CARDIFF, CF14 4XW</referrer_location>
  <referrer_postcode>CF14 4XW</referrer_postcode>
  <dhaCode>7A4</dhaCode>
  <consultant_code>TESTD</consultant_code>
  <clinicianName>Tester, Dana</clinicianName>
</PromsEventRequest>"#;
