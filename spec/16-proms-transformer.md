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

§2.2 The six message types and their bundle shapes:

| `eventCode` | Bundle | Clinical resource |
| --- | --- | --- |
| `REFERRAL` | Referral | `ServiceRequest` |
| `SURGERY` | Procedure | `Procedure` |
| `PREOP` | Appointment | `Appointment`, scheduled |
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
| `SURGERY` | `PROCEDURE_PERFORMED` | `SURGERY` | Procedure Performed | `Procedure` | `/procedure` |
| `PREOP` | `APPOINTMENT_SCHEDULED` | `PREOP` | Appointment Scheduled | `Appointment`, `booked` | `/appointment` |
| `INPATIENT` | `INPATIENT_ADMISSION` | `INPATIENT` | Inpatient Admission | `Encounter` | `/encounter` |
| `CANCELLED` | `APPOINTMENT_CANCELLED` | `CANCELLED` | Appointment Cancellation | `Appointment`, `cancelled` | `/appointment-cancellation` |
| `PREREAD` | `PREADMISSION` | `PREREAD` | Pre-admission Notification | `Encounter` | `/preadmission` |

§2.4.2 Three further types are **defined and unreachable** —
`SURGERY_PERFORMED`, `OUTPATIENT_VISIT` and `DISCHARGE`. Each shares an
`eventCode` with a type that is selected instead. They are **not** ported:
a type nothing can route to is not behaviour, and carrying it across would
suggest a choice exists where none does. What they reveal is recorded as
§9.8, which is a question for the Hub team rather than a porting decision.

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
| Referral | `MessageHeader`, `Patient`, `ServiceRequest`, `PractitionerRole`, `Practitioner`, `Organization`, `Location` |
| Procedure | `MessageHeader`, `Patient`, `Procedure`, `Practitioner`, `Organization`, `Location` |
| Appointment | `MessageHeader`, `Patient`, `Appointment`, `Practitioner`, `Organization`, `Location` |
| Encounter | `MessageHeader`, `Patient`, `Encounter`, `Practitioner`, `Organization`, `Location` |
| Appointment cancellation | as Appointment |
| Encounter pre-admission | as Encounter |

§3.1 `MessageHeader` is always first. FHIR requires it of a `message`
bundle.

§3.2 **The entries in §3's table are those present when every source field
is populated. An entry whose source data is absent is omitted**, so the
list is a maximum rather than a fixed shape. Measured against the Python:

| Fixture | Entries |
| --- | --- |
| `REFERRAL_MESSAGE` | `MessageHeader`, `Patient`, `ServiceRequest`, `PractitionerRole`, `Practitioner`, `Organization`, `Location` |
| `MINIMAL_REFERRAL_MESSAGE` | `MessageHeader`, `Patient`, `ServiceRequest`, `PractitionerRole`, `Location` |

§3.3 That makes "positional" in §3 mean **relative** order, not absolute
index. In a full referral index 4 is `Practitioner`; in a minimal one it is
`Location`. A consumer reading by absolute index would take one for the
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

§5.2 Eight `SYSTEM_ID` values route to the PROMs queue. Only Swansea Bay
(`108`) has a documented `MessageHeader.source.name` and `endpoint`.

§5.2.1 **`source.name` is omitted for an undocumented source.** Measured:
`REFERRAL_MESSAGE` (`SYSTEM_ID` 149) produces a `source` carrying only an
`endpoint`.

§5.2.2 **`source.endpoint` is *not* omitted — it is fabricated.** The Python
emits `https://wpas-source.invalid/system-id/{id}`, and
`https://wpas-source.invalid/system-id/unknown` when there is no `SYSTEM_ID`
at all. An earlier draft of this section said the field was "left unset
rather than guessed" and that an unknown source "produces a bundle with the
field absent, not a fabricated one". That is not what happens.

The choice is defensible — `.invalid` is reserved by RFC 2606 and can never
resolve, so the value is inert rather than misleading — and `endpoint` is
required by FHIR when `source` is present, which is presumably why it exists.
It is reproduced. But it is a fabricated value in a clinical message and the
specification claimed the opposite, so it is recorded rather than quietly
matched. See §9.2.

---

## §6 Resource mappings

Derived from the eleven mappers in `xml_fhir_proms_transformer/mappers/`.

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
| `eventCoding.code` | `eventCode`, `event_code`, `EVENT_CODE` | Falls back to the routed type's own code |
| `eventCoding.display` | — | The message type's display |
| `source` | `system_id`, `SYSTEM_ID`, `systemId`, then `hbCode`, `hb_code`, `HB_CODE` | §5.2.1–§5.2.2 |
| `destination` | — | Fixed Promptly Collect name and endpoint |
| `definition` | — | The message type's definition, when it has one |
| `sender` | `referrer_org`, `referrerOrg` | Reference to the `Organization` entry; `display` from the field |
| `focus` | — | Ordered: `Patient` always, then `ServiceRequest`, `PractitionerRole`, `Procedure`, `Encounter`, `Appointment` as present |

§6.2.1 `focus[0]` is always the patient, with `display` built from
`patientFirstname` and `patientSurname`.

### §6.3 `Patient`

| Target | Source | Rule |
| --- | --- | --- |
| `identifier[NHS]` | `nhsNumber`, `NHS_NUMBER` | System is the NHS number URI |
| `identifier[NHS].extension` | `NHS_CERTIFICATION`, `nhsCertification` | Verification-status extension; display from the resolver |
| `identifier[PAS]` | `crn`, `UNIT_NUMBER`, `unitNumber` | Omitted unless the source system has a documented PAS identifier system |
| `name.family` | `patientSurname`, `SURNAME`, `surname` | `use = official` |
| `name.given` | `patientFirstname`/`FORENAME`/`forename`, then `patientMiddlename`/`patientMiddleName` | In that order |
| `name.prefix` | `patientTitle`, `TITLE`, `title` | |
| `gender` | `gender`, `SEX`, `sex` | Through the resolver; unresolved leaves it unset (§7.2) |
| `birthDate` | `dob`, `BIRTHDATE`, `birthDate` | §6.1.1 |
| `address.line` | `buildingName`/`ADDRESS_1` and `streetRoadName` | Joined with `", "` |
| `address.city` | `postTown`, `ADDRESS_2` | |
| `address.postalCode` | `postCode`, `POSTCODE`, `postalCode` | |
| `communication` | `preferred_spoken_language_code`/`PREFERRED_LANGUAGE`, and `spoken_language` | `preferred = true` |
| `deceased*` | `DEATHDATE`, `deathDate` | Only when the caller asks for it |

§6.3.1 `name` is omitted entirely unless a family **or** a first given name
is present.

§6.3.2 `communication` uses the resolved code when there is one and the raw
display text otherwise, so a language is not silently lost when the code
cannot be mapped.

§6.3.3 `deceased` has four outcomes, and they differ: the field **absent**
leaves the element unset; **present and empty** is `deceasedBoolean = false`;
a parseable value is `deceasedDateTime`; a non-empty unparseable value is
`deceasedBoolean = true` — deceased with an unknown date, rather than
silently alive.

### §6.4 `ServiceRequest` (Referral)

| Target | Source | Rule |
| --- | --- | --- |
| `status` | — | `active` |
| `intent` | — | Fixed |
| `subject` | — | The `Patient` entry |
| `identifier` | `pathway`, `unique_code`, `uniqueCode`, `UNIQUE_ID` | See §9.5 — no source field was specified |
| `code.coding.code` | `eventPathway`, `event_pathway` | `display = "Referral"` |
| `occurrenceDateTime` | `eventDate`, `event_date` | §6.1.2 |
| `requester` | — | The `PractitionerRole` entry |

### §6.5 `Procedure` (Surgery)

| Target | Source | Rule |
| --- | --- | --- |
| `status` | — | `completed`. The spreadsheet says `Yes`, which is not a FHIR code — §9.5 |
| `subject` | — | The `Patient` entry |
| `code.coding.code` | `eventPathway`, `event_pathway` | `display = "Procedure performed"` |
| `performedDateTime` | `appointmentDate` + `appointmentTime` | Concatenated, then §6.1.2 |

### §6.6 `Appointment` (`PREOP`, `CANCELLED`)

| Target | Source | Rule |
| --- | --- | --- |
| `status` | — | From the message type; `booked` by default, `cancelled` for `CANCELLED` |
| `participant[0]` | — | The `Patient`, `status = accepted` — §9.5 |
| `participant[1]` | — | The `Practitioner`, when present |
| `serviceType` | `main_specialty_name`, `mainSpecialtyName`, `specialty_name` | `display` only, no code |
| `start` | `appointmentDate` + `appointmentTime` | §6.1.2 |
| `end` | — | **Set equal to `start`** — WPAS supplies no duration; §9.5 |

### §6.7 `Encounter` (`INPATIENT`, `PREREAD`)

| Message type | `class` | `status` |
| --- | --- | --- |
| `INPATIENT_ADMISSION` | `IMP`, "inpatient encounter" | `in-progress` |
| `PREADMISSION` | `PRENC`, "pre-admission" | `planned` |
| anything else | `AMB`, "ambulatory" | `in-progress` |

`subject` references the `Patient` entry. The `Encounter` carries no other
mapped fields.

### §6.8 `PractitionerRole`

| Target | Source | Rule |
| --- | --- | --- |
| `identifier` | `activityNotekey`, `activity_notekey` | See §9.5 — no source field was specified |
| `practitioner` | — | The `Practitioner` entry, when present |
| `organization` | — | The `Organization` entry, when present |
| `specialty.coding.code` | `consultant_specialty`, `consultantSpecialty` | |
| `specialty.coding.display` | `specialty_name`, `specialtyName`, `main_specialty_name` | |
| `location` | — | The `Location` entry, when present |

### §6.9 `Practitioner`

The source fields **depend on the bundle** (§2.2):

| Bundle | `identifier` | Name |
| --- | --- | --- |
| Referral | `referrer_code`, `referrerCode` | `referrer_name`, `referrerName` |
| every other | `consultant_code`, `consultantCode` | `clinicianName`, `clinician_name`, `CONS_NAME` |

§6.9.1 Omitted entirely when neither an identifier nor a name is available
(§6.0.2).

§6.9.2 `identifier.system` is the GMC number URI. A `consultant_code` such
as `HANJO` is plainly not a GMC number; the system is used as the best
available option. See §9.6.

### §6.10 `Organization`

| Target | Source | Rule |
| --- | --- | --- |
| `identifier` | `dhaCode`, `DHA_CODE`, `dha_code` | ODS organisation code system |
| `name` | `referrer_org`, `referrerOrg` | Falls back to the DHA code lookup when empty |

§6.10.1 Omitted when neither `referrer_org` nor a DHA code is present. The
fallback exists because `referrer_org` is present-but-empty in real SIT
payloads.

### §6.11 `Location` — always present

| Target | Source | Rule |
| --- | --- | --- |
| `identifier` | `referrer_location`, `referrerLocation` | The whole free-text string, used as-is |
| `name` | `referrer_location` | **The first comma-delimited segment** — §9.5 |
| `address.postalCode` | `referrer_postcode`, `referrerPostcode` | |

§6.11.0 **A `Location` entry is emitted even when there is nothing to put
in it.** Unlike §6.9's `Practitioner` and §6.10's `Organization`,
`map_location` has no guard: a bundle whose payload carries neither
`referrer_location` nor `referrer_postcode` still ends with a `Location`
holding only an `id` and a `meta.profile`. Measured —
`EXPLICIT_EVENT_CODE_MESSAGE` is exactly that case. See §9.13.

§6.11.1 `referrer_location` is a full free-text address, e.g.
`"YSBYTY GWYNEDD, PENRHOSGARNEDD, BANGOR, GWYNEDD, LL57 2PW"`. The
specification records no dedicated name field, so the first segment is
taken.

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
