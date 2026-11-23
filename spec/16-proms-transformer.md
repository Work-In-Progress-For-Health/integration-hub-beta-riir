# 16 — PROMs transformer (WPAS XML → FHIR R4B)

Turns a WPAS `PromsEventRequest` XML document into a FHIR R4B message
Bundle for Promptly. Implemented by `crates/xml-fhir-proms-transformer`.

Reference implementation: `Integration-Hub-Beta/xml_fhir_proms_transformer/`
and its `PROMS_FHIR_Mapping_Analysis.md`.

> **Status.** Complete and normative. §6's field mappings were written at
> the start of Phase 8 from the eleven mappers in
> `xml_fhir_proms_transformer/mappers/`, as planned, and checked against
> bundles captured from the Python for all eleven fixtures.
>
> Six findings came out of that reading and are recorded in §9 rather than
> reproduced silently. Two of them — §9.1's shifting entry positions and
> §9.5's `Appointment.end` — are worth the Hub team's attention before
> cutover.

---

## §1 Shape

§1.1 It is a transformer in the sense of
[`09-transformers.md`](09-transformers.md) §1: same runtime, same
configuration, same receive loop. Only `transform` differs — the input is
XML, not ER7, and the output is JSON, not ER7.

§1.2 The output is a FHIR **R4B** `Bundle` of type `message`, serialised as
JSON.

---

## §2 Routing

§2.1 Routing is on the `eventCode` **child element**, and the root element
is **not** required to be `PromsEventRequest`. Ten of the eleven fixtures
use that root; `EXPLICIT_EVENT_CODE_MESSAGE` uses `WpasMessage`, and it
routes on its `eventCode` exactly like the others. An earlier draft of this
section said the root "is always `PromsEventRequest`", which the corpus
disproves.

§2.1.1 The root tag is a **fallback**, tried only when no event code is
found at all (§2.5.7). No root in the corpus is one of §2.2's six codes, so
the fallback never resolves there — it raises §2.3's failure instead. An
earlier draft of this section said routing is "never on the root tag"; the
code does consult it, and it is written down here so a reader of
`resolve_message_type` does not conclude the specification is describing
something else.

§2.2 The ten message types and their bundle shapes. Upstream #370 split
`SURGERY` and `PREOP` into suffixed placeholder codes (`PROMS Scenarios.xlsx`,
"PROMS Message-TEMP" column); the bare codes survive as a **legacy
fallback** routed to the original scenario (§10.2).

| `eventCode` | Bundle | Clinical resource |
| --- | --- | --- |
| `REFERRAL` | Referral | `ServiceRequest` |
| `SURGERY-PROC`, legacy `SURGERY` | Procedure | `Procedure` |
| `SURGERY-PERF` | Surgery performed | `Procedure` |
| `SURGERY-DN` | Discharge | `Encounter`, `finished` |
| `PREOP-AS`, legacy `PREOP` | Appointment | `Appointment`, scheduled |
| `PREOP-VIS` | Outpatient visit | `Encounter`, `finished` |
| `PREOP-AR` | Appointment reschedule | `Appointment`, `booked` |
| `INPATIENT` | Encounter | `Encounter` |
| `CANCELLED` | Appointment cancellation | `Appointment`, `status = cancelled` |
| `PREREAD` | Encounter, pre-admission | `Encounter`, pre-admission |

§2.3 An `eventCode` outside this set fails the transform, and the message is
abandoned per [`09-transformers.md`](09-transformers.md) §1.6.

§2.4 `CANCELLED` has the same shape as `PREOP` and `PREREAD` the same shape
as `INPATIENT`; they differ in status and coding, not structure.

§2.4.1 **The routing table, in full.** `eventCode` selects exactly one type;
`event_code` and `event_display` go to `MessageHeader.eventCoding`, and
`definition` to `MessageHeader.definition`.

| `eventCode` | Type | `eventCoding.code` | `eventCoding.display` | Clinical resource | `definition` suffix |
| --- | --- | --- | --- | --- | --- |
| `REFERRAL` | `REFERRAL` | `REFERRAL` | Patient Referral | `ServiceRequest` | `/referral` |
| `SURGERY-PROC`, `SURGERY` | `PROCEDURE_PERFORMED` | `SURGERY-PROC` | Procedure Performed | `Procedure` | `/procedure` |
| `SURGERY-PERF` | `SURGERY_PERFORMED` | `SURGERY-PERF` | Surgery Performed | `Procedure` | `/surgery` |
| `SURGERY-DN` | `DISCHARGE` | `SURGERY-DN` | Discharge Notification | `Encounter` | `/discharge` |
| `PREOP-AS`, `PREOP` | `APPOINTMENT_SCHEDULED` | `PREOP-AS` | Appointment Scheduled | `Appointment`, `booked` | `/appointment` |
| `PREOP-VIS` | `OUTPATIENT_VISIT` | `PREOP-VIS` | Outpatient Visit | `Encounter` | `/outpatient` |
| `PREOP-AR` | `APPOINTMENT_RESCHEDULE` | `PREOP-AR` | Appointment Reschedule | `Appointment`, `booked` | `/appointment-reschedule` |
| `INPATIENT` | `INPATIENT_ADMISSION` | `INPATIENT` | Inpatient Admission | `Encounter` | `/encounter` |
| `CANCELLED` | `APPOINTMENT_CANCELLED` | `CANCELLED` | Appointment Cancellation | `Appointment`, `cancelled` | `/appointment-cancellation` |
| `PREREAD` | `PREADMISSION` | `PREREAD` | Pre-admission Notification | `Encounter` | `/preadmission` |

§2.4.2 *Superseded by §10.2.* Three types — `SURGERY_PERFORMED`,
`OUTPATIENT_VISIT` and `DISCHARGE` — were once defined and unreachable
because they shared an `eventCode` with another type, and were not ported.
Upstream #370 gave each its own code, so all three are now routed and
ported, and §9.8's collision no longer exists for the suffixed codes. It
remains true of the **legacy** bare `SURGERY` and `PREOP`, which still mean
procedure-performed and appointment-scheduled.

§2.4.3 §2.3's failure message lists the routing table's keys, bare codes
included, sorted: `CANCELLED`, `INPATIENT`, `PREOP`, `PREOP-AR`, `PREOP-AS`,
`PREOP-VIS`, `PREREAD`, `REFERRAL`, `SURGERY`, `SURGERY-DN`, `SURGERY-PERF`,
`SURGERY-PROC`.

---

## §2.5 Parsing

§2.5.1 The payload is a bespoke XML schema, **not** HL7 v2.xml, so none of
the HL7 tooling of [`06-validation.md`](06-validation.md) applies. Fields
are simple leaf elements: `<nhsNumber>1234567890</nhsNumber>`.

§2.5.2 Every **leaf** element in document order is indexed under its
normalised name (§6.0). A leaf whose text is empty or whitespace is skipped
**entirely** — see §2.5.5a for what that costs — so "absent" and "empty"
are indistinguishable to a reader (§6.0a).

§2.5.3 A namespace prefix is stripped before normalising, so a namespaced
payload reads the same as a bare one.

§2.5.4 **The first occurrence of a repeated name wins**, and the rest are
dropped with a debug line. The specification uses no repeating leaf, so a
repeat is a payload problem; overwriting silently would hide it. A duplicate
skips the **whole leaf**, so the second element's attributes are dropped
with it.

§2.5.5 Attributes are indexed under `{tag}_{attribute}`, normalised. **The
first value for a key wins, whichever kind it came from** — an element does
not take precedence over an attribute; document order decides.

§2.5.5a **An attribute on a text-free element is unreachable.** §2.5.2's
skip happens before attributes are examined, so `<event code="REFERRAL"/>`
indexes nothing at all. This is the Python's behaviour and it is
reproduced; it is called out because `resolve_message_type` describes the
root-tag fallback as being "for future attribute-based format", and this
parser could not read such a format. See §9.7.

§2.5.6 An empty payload, or one that is not well-formed XML, fails the
transform — the same outcome as §2.3's unknown event code.

§2.5.7 Routing reads `eventCode`, then `event_code`/`EVENT_CODE`, then the
legacy `MESSAGE_TYPE`/`messageType`/`MSG_TYPE`/`msgType`, upper-cased and
first-non-empty. With none of them present the **root tag** is tried
(§2.1.1), and failing that the transform fails per §2.3.

---

## §3 Bundle entry order

Entry order is **positional and normative** — Promptly reads by position.

| Bundle | Entries, in order |
| --- | --- |
| Referral | `MessageHeader`, `Patient`, `ServiceRequest`, `Practitioner` (referrer), `PractitionerRole` (consultant), `Practitioner` (consultant), `Organization` (§10.6; no `Location`) |
| Procedure | `MessageHeader`, `Patient`, `Procedure`, `Practitioner`, `Organization`, `Location` |
| Appointment | `MessageHeader`, `Patient`, `Appointment`, `Practitioner`, `Organization`, `Location` |
| Encounter | `MessageHeader`, `Patient`, `Encounter`, `Practitioner`, `Organization`, `Location` |
| Appointment cancellation, reschedule | as Appointment |
| Encounter pre-admission, outpatient visit, discharge | as Encounter |
| Surgery performed | as Procedure |

§3.1 `MessageHeader` is always first. FHIR requires it of a `message`
bundle.

§3.2 **The entries in §3's table are those present when every source field
is populated. An entry whose source data is absent is omitted**, so the
list is a maximum rather than a fixed shape. Measured against the Python:

| Fixture | Entries |
| --- | --- |
| `REFERRAL_MESSAGE` | `MessageHeader`, `Patient`, `ServiceRequest`, `Practitioner`, `PractitionerRole`, `Practitioner`, `Organization` |
| `MINIMAL_REFERRAL_MESSAGE` | `MessageHeader`, `Patient`, `ServiceRequest` |

§3.3 That makes "positional" in §3 mean **relative** order, not absolute
index. In a full referral index 3 is the referrer `Practitioner`; in a
minimal one there is no index 3 at all. A consumer reading by absolute index would take one for the
other. See §9.1 — whether Promptly does that is a question for the Hub team,
and it is not answerable from this repository.

§3.4 The Rust reproduces the omission behaviour exactly, because §8.1
compares whole bundles against the Python's.

---

## §4 Identity and cross-references

§4.1 Every resource is given a fresh UUID.

§4.2 An entry's `fullUrl` is `urn:uuid:{uuid}`.

§4.3 Cross-references between entries use the same `urn:uuid:{uuid}` form,
so a bundle is self-contained and carries no server-assigned identifiers.

§4.4 UUID generation MUST be injectable, so tests can assert on a whole
bundle deterministically. The Python threads a `UuidFactory` for exactly
this reason and the Rust does the same.

---

## §5 Source systems

§5.1 A source health board is looked up by the WPAS `SYSTEM_ID` field.

§5.2 **Seven** `SYSTEM_ID` values are WPAS health boards (upstream #423,
`WPAS PROMS Mapping - Amy_Questions.xlsx`). Each has a name:

| `SYSTEM_ID` | `source.name` |
| --- | --- |
| `108` | Swansea Bay University Health Board |
| `109` | Betsi Cadwaladr University Health Board |
| `126` | Cwm Taf Morgannwg University Health Board |
| `139` | Aneurin Bevan University Health Board |
| `149` | Hywel Dda University Health Board |
| `170` | Powys Teaching Health Board |
| `310` | Velindre University NHS Trust |

`103` (Princess of Wales, PIMS) and `140` (Cardiff and Vale) do not use
WPAS, and `131` was a spreadsheet error for Velindre. None of the three is a
source: `140`, which was previously configured, is **removed**.

§5.2.0 Each board also carries (upstream #435, #437, #438; §10.6) its **ODS
code**, used for `Organization.identifier`: `108` 7A3, `109` 7A1, `126` 7A5,
`139` 7A6, `149` 7A2, `170` 7A7, `310` RQF; and the identifier systems of the
WPAS `consultant_code` and `referrer_code`, `https://fhir.{domain}.nhs.wales/Id/wpas-consultant-code`
and `…/wpas-referrer-code` with `{domain}` `sbuhb`, `bcuhb`, `ctmuhb`, `abuhb`,
`hduhb`, `pthb`, `vunhst`. **Betsi Cadwaladr (`109`) is the exception:** both
codes go under `https://fhir.bcuhb.nhs.wales/Id/central-pas-identifier`.

§5.2.1 `source.name` is the board's name for a known board and **omitted**
for an unknown one.

§5.2.2 `source.endpoint` is `https://fhir.nhs.uk/source-system/wpas/{id}` for
a known board, unless the `WPAS_SOURCE_ENDPOINT_{id}` environment variable
overrides it. For an unknown board, or no `SYSTEM_ID` at all, it is the same
base plus the id as sent, or `unknown`. FHIR requires `endpoint` when
`source` is present, which is why it is never omitted.

This **supersedes** the earlier fabricated
`https://wpas-source.invalid/system-id/{id}` placeholder (the former §5.2.2
and §9.2): the endpoint is now derived from the id on a real-looking base
rather than a reserved-invalid one. An unknown id still yields a value no
registry has issued, so §9.2 stands in a reduced form (§10.4).

---

## §6 Resource mappings

Derived from the eleven mappers in `xml_fhir_proms_transformer/mappers/`.

**Read §10.6 with this section.** It records the Referral rework of upstream
`#435` and its follow-ups, which supersedes the Referral rows below.

§6.0 **Field lookup is by normalised key, then by alias order.** Two things
happen, and conflating them leads to the wrong conclusion about what the
alias lists mean:

1. Every element name is **normalised** — lowercased, with everything
   outside `[a-z0-9]` removed (§2.5). So `nhsNumber`, `NHS_NUMBER` and
   `nhs-number` are one key, not three.
2. A read then tries its aliases **in order** and takes the first
   **non-empty** value.

So an alias list matters only where the aliases are genuinely different
words. `message.get("nhsNumber", "NHS_NUMBER")` is one lookup written
twice; `message.get("crn", "UNIT_NUMBER", "unitNumber")` is two — `crn`
first, then `unitnumber`. Where the order is load-bearing the tables below
are normative in that order.

§6.0a The distinction between **absent** and **present but empty** is lost
by the lookup: `get` returns the default for both, because §2.5.2 does not
index an empty element at all. One mapper needs the difference — §6.3.3's
`deceased` — and it uses `has`, which has the same blind spot. This is
reproduced, and it is why §6.3.3's "present and empty" case is reached only
when the element carries whitespace rather than nothing.

§6.0.1 An absent or unparseable value leaves its element **unset**. Nothing
in §6 substitutes a default for a missing clinical value; §5.2.2's endpoint
is the single exception in this document and it is flagged there.

§6.0.2 A resource whose every source field is absent is **omitted from the
bundle** rather than emitted empty (§3.2).

### §6.1 Dates and times

§6.1.1 `to_fhir_date` accepts an ISO datetime as well as a date, and falls
back to a list of common NHS representations. Unparseable leaves the element
unset with a warning.

§6.1.2 `to_fhir_datetime` returns a **timezone-aware** value: WPAS carries
no offset, so **UTC is assumed**. A date-only value becomes midnight UTC.
Whether WPAS means local time is unconfirmed — see §9.3.

§6.1.3 `split_person_name` splits one WPAS name field into prefix, given
names and family. Two formats occur: `"ARDERN-JONES L"` (referrer) and
`"Jones, Hannah"` (consultant, comma-separated surname first).

### §6.2 `MessageHeader`

| Target | Source | Rule |
| --- | --- | --- |
| `id` | — | UUID (§4.1) |
| `meta.profile` | — | `…/wpas-messageheader`. Flagged incorrect by a reviewer; see §9.4 |
| `eventCoding.code` | `eventCode`, `event_code`, `EVENT_CODE`, then the legacy `MESSAGE_TYPE`, `messageType`, `MSG_TYPE`, `msgType` | As sent, so a legacy bare `SURGERY` stays `SURGERY`; falls back to the routed type's own code (§10.2) |
| `eventCoding.display` | — | The message type's display |
| `source` | `system_id`, `SYSTEM_ID`, `systemId`, then `hbCode`, `hb_code`, `HB_CODE` | §5.2.1–§5.2.2 |
| `destination` | — | Fixed Promptly Collect name; the endpoint is the `PROMPTLY_DESTINATION_ENDPOINT` environment variable when set, otherwise the Promptly staging `$process-message` URL (§10.3) |
| `definition` | — | The message type's definition, when it has one |
| `sender` | — | Reference to the `Organization` entry; `display` is the health board name of §6.10 (omitted when unresolved) |
| `focus` | — | Ordered: `Patient` always, then `ServiceRequest`, `PractitionerRole`, `Procedure`, `Encounter`, `Appointment` as present |

§6.2.1 `focus[0]` is always the patient, with `display` built from
`patientFirstname` and `patientSurname`.

### §6.3 `Patient`

| Target | Source | Rule |
| --- | --- | --- |
| `meta.profile` | — | `…/StructureDefinition/wpas-patient` (was `DataStandardsWales-Patient`) |
| `active` | — | `true` |
| `identifier[NHS]` | `nhsNumber`, `NHS_NUMBER` | System is the NHS number URI |
| `identifier[NHS].extension` | `NHS_CERTIFICATION`, `nhsCertification` | Verification-status extension; display from the resolver |
| `identifier[PAS]` | `crn`, `UNIT_NUMBER`, `unitNumber` | Omitted unless the source system has a documented PAS identifier system |
| `name.family` | `patientSurname`, `SURNAME`, `surname` | `use = official` |
| `name.given` | `patientFirstname`/`FORENAME`/`forename`, `patientMiddlename`/`patientMiddleName` | **One** value: first and middle joined with a space |
| `name.prefix` | — | **Not mapped** (was `patientTitle`) |
| `gender` | `gender`, `SEX`, `sex` | Through the resolver; unresolved leaves it unset (§7.2) |
| `birthDate` | `dob`, `BIRTHDATE`, `birthDate` | §6.1.1 |
| `address.line` | `buildingName`/`ADDRESS_1` and `streetRoadName` | Joined with `", "` |
| `address.city` | `postTown`, `ADDRESS_2` | |
| `address.district` | `postalCounty`, `postal_county` | Upstream #346; a county alone makes an address |
| `address.postalCode` | `postCode`, `POSTCODE`, `postalCode` | |
| `telecom` | `home_phone`/`homePhone`/`TELEPHONE_DAY`; `work_phone`/`workPhone`; `mobile_phone`/`mobilePhone`; `email`/`EMAIL` | One `ContactPoint` per populated field, in that order: `phone`+`home`, `phone`+`work`, `phone`+`mobile`, `email` (no `use`) |
| `extension[ethnicCategory]` | `ethnic_origin_code`/`ethnicOriginCode`, `ethnic_origin`/`ethnicOrigin` | `valueCodeableConcept.coding` under the WPAS ethnicity system, `text` the display |
| `communication.language` | `preferred_spoken_language_code`/`preferredSpokenLanguageCode`/`PREFERRED_LANGUAGE`/`preferredLanguage`, and `spoken_language`/`spokenLanguage` | Code and display **as sent**, under the WPAS spoken-language system; no `preferred` flag; no resolver |
| `deceased*` | `DOD`/`dod`, then legacy `DEATHDATE`/`deathDate` | Always considered now (§6.3.3) |

§6.3.1 `name` is omitted entirely unless a family **or** a first given name
is present.

§6.3.2 `communication` is no longer resolved through the reference-data
service: the code and display are taken as sent. This supersedes the
resolver-then-display behaviour and **retires §9.9** — a patient's spoken
language now reaches Promptly. The earlier `contactPreference` extension
(upstream #346) is removed again by #423.

§6.3.3 `deceased` has three outcomes: no `DOD` element (or an empty one,
which §2.5.2 makes the same thing) leaves the element **unset** — the former
`deceasedBoolean = false` for "present and empty" is gone; a parseable value
is `deceasedDateTime`; a non-empty unparseable value is
`deceasedBoolean = true` — deceased with an unknown date, rather than
silently alive. The former `include_deceased` switch is gone: every bundle
now carries it.

### §6.4 `ServiceRequest` (Referral)

| Target | Source | Rule |
| --- | --- | --- |
| `status` | — | `active` |
| `intent` | — | `order` (was `plan`) |
| `doNotPerform` | — | `false` |
| `subject` | — | The `Patient` entry |
| `identifier` | `pathway`, `unique_code`, `uniqueCode`, `UNIQUE_ID` | System `https://fhir.nhs.wales/Id/wpas-servicerequest`; see §9.5 |
| `code.coding` | the routing field (`eventCode` and its aliases, §2.5.7), upper-cased; `REFERRAL` when only the root tag routed | System `…/CodeSystem/proms-event`, `display = "Referral"` |
| `reasonCode.coding` | `eventPathway`, `event_pathway` | System `…/CodeSystem/wpas-event-pathway`, **no display** yet (pending a WPAS ServicePoint lookup) |
| `occurrenceDateTime` | `eventDate`, `event_date` | §6.1.2 |
| `requester` | — | The `PractitionerRole` entry |

### §6.5 `Procedure` (Surgery)

| Target | Source | Rule |
| --- | --- | --- |
| `status` | — | `completed`. The spreadsheet says `Yes`, which is not a FHIR code — §9.5 |
| `subject` | — | The `Patient` entry |
| `code.coding.code` | `eventPathway`, `event_pathway` | `display = "Procedure performed"` |
| `performedDateTime` | `appointmentDate` + `appointmentTime` | Concatenated, then §6.1.2 |

### §6.6 `Appointment` (`PREOP-AS`, `PREOP-AR`, `CANCELLED`)

| Target | Source | Rule |
| --- | --- | --- |
| `identifier` | `activityNotekey`, `activity_note_key` | System `…/id/wpas-appointment` (upstream #346) |
| `status` | — | From the message type; `booked` by default, `cancelled` for `CANCELLED` |
| `participant[0]` | `CONFIRM_APPT`, `confirmAppt`, `confirm_appt` | The `Patient`; `Y` is `accepted`, `N` is `declined`, anything else (including absent) is `tentative` |
| `participant[1]` | — | The `Practitioner`, when present |
| `serviceType` | `main_specialty_name`, `mainSpecialtyName`, `specialty_name` | `display` only, no code |
| `start` | `appointmentDate` + `appointmentTime` | §6.1.2 |
| `end` | — | **Set equal to `start`** — WPAS supplies no duration; §9.5 |

### §6.7 `Encounter` (`INPATIENT`, `PREREAD`, `PREOP-VIS`, `SURGERY-DN`)

| Message type | `class` | `status` |
| --- | --- | --- |
| `INPATIENT_ADMISSION` | `IMP`, "inpatient encounter" | `in-progress` |
| `PREADMISSION` | `PRENC`, "pre-admission" | `planned` |
| `OUTPATIENT_VISIT` | `AMB`, "Ambulatory" | `finished` |
| `DISCHARGE` | `IMP`, "Inpatient encounter" | `finished` |
| anything else | `AMB`, "ambulatory" | `in-progress` |

`subject` references the `Patient` entry. `identifier` is `activityNotekey`
(or `activity_note_key`) under `…/id/wpas-encounter`, when present. The
`Encounter` carries no other mapped fields.

§6.7.1 The two new displays are **capitalised** where the first two are not,
because they come from `fhir_constants.py`'s constants rather than the
mapper's literals. Reproduced. The v3 ActCode `EncounterClass` set has no
discharge class, so a discharge reuses `IMP` and differs only in `status`
(a TEMP placeholder upstream).

### §6.8 `PractitionerRole`

Built for the Referral bundle only.

| Target | Source | Rule |
| --- | --- | --- |
| `identifier` | `referrer_code`, `referrerCode` | System is the **GMC** number URI (referral). A non-referral caller would use `consultant_code` under `https://fhir.nhs.wales/Id/wpas-practitioner-role`; no bundle does today |
| `practitioner` | — | The `Practitioner` entry, when present |
| `organization` | — | The `Organization` entry, when present |
| `specialty.coding.code` | `consultant_specialty`, `consultantSpecialty` | **First three characters** only |
| `specialty.coding.display` | `specialty_name`, `specialtyName`, `main_specialty_name` | |
| `location` | — | The `Location` entry, when present |

### §6.9 `Practitioner`

| Target | Source | Rule |
| --- | --- | --- |
| `identifier[0]` | `consultant_code`, `consultantCode` | System `https://fhir.nhs.wales/Id/wpas-practitioner`; every bundle |
| `identifier[1]` | `referrer_code`, `referrerCode` | Referral only; GMC number URI (§9.6) |
| `name.text` | Referral: `referrer_name`, `referrerName`. Others: `clinicianName`, `clinician_name`, `CONS_NAME` | Free text; **not** parsed into family and given |

§6.9.1 Omitted entirely when there is neither an identifier nor a name
(§6.0.2).

§6.9.2 `split_person_name` (§6.1.3) is no longer used by any mapper.

### §6.10 `Organization`

Promptly asked for the **health board**, not the practice.

| Target | Source | Rule |
| --- | --- | --- |
| `active` | — | `true` |
| `identifier` | `hb_code`, `hbCode`, `HB_CODE` | ODS organisation code system |
| `name` | — | The health board name: from the `system_id` lookup (§5.2) when it is a known board, otherwise `dha_code_name(hb_code)` (§7.1.2); omitted when that is just the code echoed back |

§6.10.1 Omitted when there is no `hb_code`. `referrer_org` and `dhaCode` are
no longer read. The same name is `MessageHeader.sender.display` (§6.2).

### §6.11 `Location` — always present

| Target | Source | Rule |
| --- | --- | --- |
| `status` | — | `active` |
| `identifier` | `referrer_prac`, `referrerPrac` | `wrts-location-identifier` system |
| `name` | — | **None** — the mapping has no `Location.name` |
| `address.text` | `referrer_location`/`referrerLocation` and `referrer_postcode`/`referrerPostcode` | The two joined with a space (so a location that already ends with the postcode repeats it) |
| `address.postalCode` | `referrer_postcode`, `referrerPostcode` | |

§6.11.0 **A `Location` entry is emitted even when there is nothing to put
in it**, except that it now carries `status = active` always. Unlike §6.9's
`Practitioner` and §6.10's `Organization`, `map_location` has no guard: a
bundle whose payload carries neither `referrer_prac` nor location data
still ends with a `Location` holding an `id`, a `meta.profile` and
`status`. See §9.13.

§6.11.1 The former first-segment `Location.name` and whole-string
`Location.identifier` rules (§9.5) are retired by #423.

### §6.12 Not ported

§6.12.1 `mappers/_deprecated/` (`care_plan_mapper.py`, `task_mapper.py`) is
**not** ported. Those resources are not in any bundle of §3.

---

## §7 Reference data

§7.1 Two kinds of lookup exist and they behave **differently** on an
unrecognised code. Conflating them gets one of them wrong.

### §7.1.1 The resolver seam — absent on failure

`resolve_gender` and `resolve_language` sit behind a
`ReferenceDataResolver`, with a static default and an injectable one for
tests. They exist because the Fiorano workflow called an external
`CoreReferenceDataLookup_Service` for `SEX` and `PREFERRED_LANGUAGE`, and
the Integration Hub has no equivalent.

| Lookup | Behaviour |
| --- | --- |
| `resolve_gender` | WPAS `SEX` → `male`/`female`/`other`/`unknown`; `None` otherwise |
| `resolve_language` | **always `None`** — see §9.9 |

§7.2 A code the resolver cannot map MUST leave the element **absent** rather
than emitting a placeholder. A fabricated clinical code is worse than a
missing one.

### §7.1.2 The wiki lookups — the code echoed back

`nhs_certification_display` and `dha_code_name` are ports of JavaScript
functions from the INSE wiki, and each **returns its input unchanged** when
the code is not recognised. That is the Fiorano workflow's behaviour and it
is preserved so this service produces the same output.

| Lookup | Known code | Unknown code |
| --- | --- | --- |
| `nhs_certification_display` | the display text | **the code itself** |
| `dha_code_name` | the health board name | **the code itself** |

§7.2.1 So §7.2's rule does **not** hold for these two: an unrecognised
certification code `99` is emitted as
`Coding.display = "99"`, which asserts that `"99"` is the human-readable
description of that status. Recorded as §9.10 rather than silently
corrected, because changing it would change what Promptly stores.

§7.3 The gender table accepts both letter and numeric WPAS codes:
`M`/`1` → `male`, `F`/`2` → `female`, `O`/`9` → `other`, and
`U`/`0`/`X` → `unknown`. Matching is upper-cased and trimmed.

---

## §8 Tests

§8.1 Golden: every fixture in
`xml_fhir_proms_transformer/tests/wpas_messages.py`, transformed with a
deterministic UUID factory (§4.4) and compared against the Python's bundle
JSON, canonically.

§8.2 Unit: §2.2's routing table, including §2.3's unknown code.

§8.3 Unit: §3's entry order for each bundle, asserted positionally.

§8.4 Unit: §4.2 and §4.3 — every reference resolves to an entry in the same
bundle, and no reference dangles.

§8.5 Unit: §5.2's unknown source leaving fields absent, and §7.2's
unresolved code leaving the coding absent.

§8.6 Every produced bundle validates against the FHIR R4B `Bundle` profile.

---

## §9 Findings raised with the Hub team

§9.1 **Bundle entries are omitted when their source data is absent, while
§3 describes the order as positional.** A full referral has seven entries
and a minimal one has five, with `Practitioner` and `Organization` dropping
out of the middle — so absolute indices shift. If Promptly reads entries by
position rather than by `resourceType`, a minimal referral hands it a
`Location` where it expects a `Practitioner`.

Which it does cannot be determined from this repository: Promptly is an
external system. The behaviour is reproduced exactly (§3.4) so nothing
changes on cutover, but if Promptly is positional then this is a latent
defect in the Python for every message with incomplete practitioner data,
and worth knowing about before it is one in Rust too.

§9.2 **An undocumented source system gets a fabricated endpoint.**
`https://wpas-source.invalid/system-id/{id}` is emitted for seven of the
eight configured health boards — every one except Swansea Bay. It is inert
by construction (RFC 2606 reserves `.invalid`), and FHIR requires
`endpoint` when `MessageHeader.source` is present, so the alternative is
omitting `source` entirely. Recorded because the value is synthetic, it
appears in a clinical message, and a reader of the bundle has no way to tell
it apart from a real endpoint that happens to be unreachable.

§9.3 **WPAS date/times carry no timezone and UTC is assumed.**
`to_fhir_datetime` stamps every naive value as UTC, because `fhir.resources`
rejects a naive datetime. If WPAS means Europe/London, every appointment
time in a bundle is up to an hour wrong for seven months of the year, and
wrong in the direction that makes an appointment look earlier than it is.
The Python's own docstring says "confirm with the spec owner"; that
confirmation does not appear to have happened. Reproduced as-is.

§9.4 **The `MessageHeader` profile URL is flagged incorrect in the source.**
`fhir_constants.py` carries `# SPEC GAP: reviewer flagged the above as
incorrect — awaiting confirmed NHS Wales URL` against
`https://fhir.promptly.health/wpas/StructureDefinition/wpas-messageheader`.
Every other profile in that file is a `fhir.nhs.wales` URL. Reproduced,
because changing it would change what Promptly validates against.

§9.5 **Nine values in the bundles have no specified source.** Each is a
`SPEC GAP` comment in the Python, and each is a decision somebody made in
the absence of a mapping:

| Element | What is emitted | Why it is a guess |
| --- | --- | --- |
| `ServiceRequest.identifier` | `pathway` | The spreadsheet names no field |
| `PractitionerRole.identifier` | `activityNotekey` | The spreadsheet names no field |
| `Procedure.status` | `completed` | The spreadsheet says `Yes`, not a FHIR code |
| `Appointment.participant.status` | `accepted` | Required 1..1; the spreadsheet is silent |
| `Appointment.end` | equal to `start` | WPAS supplies no duration or end time |
| `Location.name` | first comma-delimited segment of `referrer_location` | The spreadsheet marks the source `??` |
| `Location.identifier` | the whole `referrer_location` string | No dedicated code or id field |
| `Patient.communication` | resolver, then raw display text | "pending Core Reference Data service integration" |
| `Encounter` class for an unrecognised type | `AMB`, ambulatory | No stated default |

`Appointment.end` is the one worth looking at first: an appointment whose
end equals its start has zero duration, which a downstream calendar may
render as an instant or reject outright.

§9.6 **`Practitioner.identifier` uses the GMC system for values that are not
GMC numbers.** `consultant_code` values such as `HANJO` are local codes. The
bundle asserts they are GMC registration numbers, which is a false statement
about a clinician's identity in a clinical message. The Python's comment
calls it "the best available option pending spec clarification". Reproduced,
and raised here because it is an assertion rather than an omission — §7.2's
principle is that a fabricated code is worse than a missing one, and this is
the same class of thing one level up.

§9.7 **An attribute-carrying payload would not parse.** `proms_parser.py`
skips a leaf with no text before it looks at that leaf's attributes, so
`<event code="REFERRAL"/>` yields no fields at all — not the event code, and
not anything else on that element. Attributes are only reachable on an
element that also has text.

This is worth knowing because `resolve_message_type`'s docstring gives the
root-tag fallback as being "for future attribute-based format". If such a
format is coming, the parser needs a change before it arrives, and the
symptom otherwise is a message that routes as unroutable with no indication
that its data was silently dropped. Reproduced exactly (§2.5.5a) so nothing
changes on cutover.

§9.8 **`eventCode` cannot distinguish five WPAS scenarios, and two of them
would produce a different clinical resource.**

`message_types.py` defines nine types for six codes. The three that never
route are not redundant duplicates — two of them differ in the resource
they would emit:

| Unreachable type | Shares `eventCode` with | Would emit | Actually emitted |
| --- | --- | --- | --- |
| `SURGERY_PERFORMED` | `PROCEDURE_PERFORMED` (`SURGERY`) | `Procedure` | `Procedure` — same shape |
| `DISCHARGE` | `PROCEDURE_PERFORMED` (`SURGERY`) | **`Encounter`** | **`Procedure`** |
| `OUTPATIENT_VISIT` | `APPOINTMENT_SCHEDULED` (`PREOP`) | **`Encounter`** | **`Appointment`** |

The Python's own comment says the spreadsheet "defines multiple scenarios
for SURGERY and PREOP but the eventCode alone cannot distinguish them — a
single mapping is used per code until the specification owner confirms a
disambiguation field."

§9.8.1 The consequence for `DISCHARGE` is not a mis-shaped bundle, it is a
**false clinical statement**. A discharge arriving as `eventCode=SURGERY`
becomes `Procedure { status: "completed", code.display: "Procedure
performed" }` — a positive assertion that a procedure was carried out on
that patient, on that date. §7.2 already holds that a fabricated clinical
code is worse than a missing one; this is the same principle at the level of
the resource type.

§9.8.2 What cannot be answered from this repository: whether WPAS ever
actually sends a discharge or an outpatient visit under those codes. If it
never does, this is latent and harmless. If it does, it is wrong today and
has been for as long as the Python has been running. Either way the fix is
a disambiguation field in the payload, which is the Hub team's to specify.

§9.8.3 Reproduced exactly (§2.4.1), because changing which resource a code
produces would change what Promptly stores.

§9.9 **`Patient.communication` is never emitted.**
`StaticReferenceDataResolver.resolve_language` returns `None` for every
input, so §6.3's `communication` element is omitted from every bundle the
Python has ever produced. The mapper's fallback — use the raw
`spoken_language` display text when the code cannot be resolved — is
therefore live code that never runs with the default resolver.

The comment says the authoritative code set "lives in the Core Reference
Data service, which the Integration Hub does not yet consume". So a
patient's recorded spoken language does not reach Promptly at all. For a
service operating under the Welsh Language Standards that is worth knowing
about explicitly — a Welsh-speaking patient's preference is captured in
WPAS and dropped in transit. Reproduced; the seam is in place so a
service-backed resolver needs no mapper change.

§9.10 **Two lookups emit the unrecognised code as its own display text.**
Per §7.1.2, `nhs_certification_display("99")` returns `"99"`, which becomes
`Coding.display = "99"` — a claim that `"99"` is the description of that
NHS-number verification status. `dha_code_name` does the same for an
unknown organisation code, becoming `Organization.name = "7Z9"`.

This is the wiki's documented behaviour and is preserved, but it is the
opposite of §7.2's principle and it produces a resource that reads as
though the value were meaningful. Omitting the display instead would leave
the `code` intact and lose nothing.

§9.11 **Two identifier systems name a laboratory host.**
`ServiceRequest.identifier.system` and
`PractitionerRole.identifier.system` are

```
https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-servicerequest
https://wpas-integration-ig.tools.labs.promptly.health/id/wpas-practitionerrole
```

`tools.labs.promptly.health` reads as a development or integration-guide
environment, not a production namespace. Every other identifier system in
the bundle is an `fhir.nhs.uk`, `fhir.nhs.wales` or `fhir.hl7.org.uk` URL.

An identifier system is the namespace a receiver files the value under, so
if these change later, every previously sent `ServiceRequest` is namespaced
differently from every subsequent one and the two do not match. Reproduced
exactly, and raised because it looks like a value that was never finalised
rather than one that was chosen.

§9.12 **The NHS-number verification code system and its extension differ by
a suffix.** The extension is
`…/StructureDefinition/Extension-UKCore-NHSNumberVerificationStatus` and
the code system is
`…/CodeSystem/UKCore-NHSNumberVerificationStatus**Wales**`. Writing them as
a matching pair is the obvious mistake and produces a coding in a code
system that does not exist. Not a defect in the Python — it has them right
— but recorded because this port got it wrong on a first pass and only the
constant-by-constant check against the source caught it.

§9.13 **An empty `Location` is emitted for every bundle.**
`map_location` returns a `Location` unconditionally, where
`map_practitioner` and `map_organization` beside it return `None` when
their sources are absent. So a payload with no location data produces an
entry containing nothing but a generated id and a profile URL.

It is not harmful — a resource with no content asserts nothing — but it is
an entry a consumer has to skip, it shifts nothing because it is last, and
it reads as an oversight rather than a decision given the two guarded
mappers beside it. Reproduced exactly (§6.11.0), because removing it would
change the entry count of every bundle.

Worth noting how this was found: reading the three mappers side by side
suggests they behave alike, and only the byte-for-byte comparison against
the Python's own output showed otherwise.

---

## §10 Upstream synchronisation (#346, #370, #423)

§10.0 The Rust port had been captured from the Python **before** upstream
#346 (`927e69d`, "fhir transformer cont mapping changes") and #370
(`8a7f6f3`, "additional event scenarios"), although both pre-date the
`9e797ac` baseline the rest of the workspace was synced to. Verified by
regenerating the corpus from `927e69d^`: it reproduced the committed
`python-golden.json` exactly. #346, #370 and #423 (`c074825`, INTHUB-594635
"revised spec") were therefore ported together, and the corpus was
regenerated from the upstream HEAD with the §4.4 factory.

§10.1 #346: `Appointment.identifier` and `Encounter.identifier` from
`activityNotekey`; the patient `Appointment.participant.status` rule of
§6.6; `address.district` of §6.3; `OUTPATIENT_VISIT` and `DISCHARGE`
encounter classes of §6.7; `MessageHeader.eventCoding.code` preserving the
raw routing field. Its `contactPreference` extension and `PractitionerRole`
identifier from `specialty_name` were short-lived and are replaced by #423.

§10.2 #370: the suffixed codes of §2.2. Because the bare `SURGERY` and
`PREOP` still route, `eventCoding.code` reports the code **as sent**
(§6.2), while `eventCoding.display` and `definition` come from the routed
type. This is why a bare `SURGERY` payload yields `eventCoding.code =
SURGERY` with display "Procedure Performed".

§10.3 #423: §5, §6.2 to §6.4 and §6.8 to §6.11. The destination endpoint is
environment-dependent: `PROMPTLY_DESTINATION_ENDPOINT` overrides the staging
default `https://wpas-fhir.stg.labs.promptly.health/fhir/r4/$process-message`.
The default is a **staging** URL in a production message when the variable is
unset; deployments MUST set it. Identifier systems for `ServiceRequest` and
`PractitionerRole` moved from the `tools.labs.promptly.health` host to
`https://fhir.nhs.wales/Id/…`, which **retires §9.11**.

§10.4 Findings revised by this sync: §9.2 (the `.invalid` placeholder) is
replaced by an id-derived `fhir.nhs.uk` URL for unknown boards (§5.2.2);
§9.5's `PractitionerRole.identifier`, `Location.name`, `Location.identifier`
and `Patient.communication` rows are retired (§6.8, §6.11, §6.3.2); §9.8 now
applies only to the legacy bare codes (§2.4.2); §9.9 and §9.11 are retired.
The remaining rows stand.

§10.5 The upstream tester's PROMs sample referral gained `home_phone`,
`work_phone`, `mobile_phone`, `email`, `ethnic_origin_code`,
`ethnic_origin` and `referrer_prac`, and `hbCode` became `7A2`
(`integration-hub-tester`, `spec/integration-hub-tester-component-preview.md`).

§10.6 **#423 (second half), #435, #437 to #440 and #447: the Referral
rework** (upstream `c074825`, `2ab0b66`, `ef9ca9d`, `662b60b`, `58a7809`,
`e15fc89`, `bb13f36`). The Referral bundle moved to the Promptly **WPAS
Integration IG** (`https://wpas-integration-ig.tools.labs.promptly.health`).
Every other bundle is unchanged except where stated. **This section
supersedes any contrary row in §3, §5 and §6; the older rows describe the
mapping up to #346/#370/#423's first half.** Verified, as §8.1 requires, by
regenerating the corpus from the Python at HEAD (`fed0803`, 13 Nov 2026)
and comparing all 18 bundles whole: they are equal.

| Clause | Revision |
| --- | --- |
| §3, §4 | A Referral is `MessageHeader`, `Patient`, `ServiceRequest`, **referrer `Practitioner`**, **consultant `PractitionerRole`**, **consultant `Practitioner`**, `Organization`. **No `Location`.** UUIDs are allocated in that order (seven), so an absent referrer or consultant still leaves the gap. The consultant `PractitionerRole` and `Practitioner` stand or fall together. `Bundle.meta.profile` is `…/StructureDefinition/wpas-messagebundle`. |
| §6.2 | Referral header: profile `wpas-messageheader` (IG), `eventCoding.system` `…/CodeSystem/wpas-event-codes`, `code` the **lowercase** canonical code (`referral`), **no `definition`**. `focus` is the patient, the `ServiceRequest`, the referrer `Practitioner`, then the consultant `PractitionerRole`. |
| §6.3 | Referral `Patient` profile is the IG's `wpas-patient` and carries **no NHS-number verification status** (#440); other bundles keep both. `Patient.name` has **no `use`** (#439) and the **title is a `prefix` again**. `gender` `I` is `other` (#439). Spoken language is under the IG's `wpas-spoken-language` system and WPAS code `9` (Unknown) **omits `communication`** entirely; ethnicity is under the IG's `wpas-ethnicity` system. |
| §6.4 | IG profile and identifier system. `code` is the **pathway** (`eventPathway`, e.g. `GENERIC`) under `…/CodeSystem/wpas-proms-pathway`, no display; **no `reasonCode`**. `occurrenceDateTime` is a **plain date** (a midnight-UTC time shifts an hour in BST). `requester` is the referrer `Practitioner`; `performer` is the consultant `Practitioner`. |
| §6.8 | Only the **consultant's** role. Identifier `consultant_code` under `…/id/wpas-practitionerrole`; refers to the consultant `Practitioner` and the `Organization`. `specialty` is sent only when the first three characters of `consultant_specialty` are on the national practice-setting list (`UKCore-PracticeSettingCode`, 87 codes), as that system; otherwise **omitted**. No `location`. |
| §6.9 | Referral practitioners are `wpas-practitioner` (IG): the **referrer** (`referrer_code`, `referrer_name`) and the **consultant** (`consultant_code`, `clinicianName`), each with a free-text name and an identifier under the sending board's **own** system (§5.2.0). An **unknown board has no valid system, so the identifier is omitted** rather than filed under a guess; the name is kept. Neither identifier nor name: no entry. Other bundles keep one `Practitioner` (`consultant_code`, WPAS practitioner system). |
| §6.10 | `Organization.identifier` is the **board's ODS code** (`hbCode` carries placeholders such as `X98`), falling back to `hb_code` for an unknown board. A known board is enough: `hb_code` is no longer required. Referral profile is the IG's `wpas-organization`. `sender.display` and the name use the same board lookup. |
| §6.11 | Unchanged for the bundles that have one. |

Deviations from the Python: none in the mapping (the golden proves it). The
unroutable-fixture error text still differs in wording, as before (§8.1).
**Not ported:** `integration-hub-tester`'s four `TEMP` PROMs samples, which
need the `SURGERY-PERF`, `SURGERY-DN`, `PREOP-VIS` and `PREOP-AR` routing
upstream added in `8a7f6f3`; the tester's `proms_samples` is otherwise as
before. §9.5's `ServiceRequest` and `PractitionerRole` rows and §9.6 (the
referrer's code under the GMC system) are **retired** by this section.

**Checked arithmetic** (`rust-checked-arithmetic`): the crate denies
`arithmetic_side_effects`, `as_conversions`, `indexing_slicing`, `string_slice`,
`integer_division` and `unreachable`. The bundle's `meta` is inserted through
the object, not by indexing; the sequential-UUID test double keeps two
`#[expect]`s (a counter only its own calls advance, RCA-EXC-2).
