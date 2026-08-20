# 16 — PROMs transformer (WPAS XML → FHIR R4B)

Turns a WPAS `PromsEventRequest` XML document into a FHIR R4B message
Bundle for Promptly. Implemented by `crates/xml-fhir-proms-transformer`.

Reference implementation: `Integration-Hub-Beta/xml_fhir_proms_transformer/`
and its `PROMS_FHIR_Mapping_Analysis.md`.

> **Status.** §1–§5 are normative now and fix the routing, the bundle
> shapes, the entry ordering and the identity model — the parts other
> systems see. The per-resource field mappings of §6 are completed at the
> start of Phase 8 from the eleven mappers in
> `xml_fhir_proms_transformer/mappers/`, in the same table form as
> [`09-transformers.md`](09-transformers.md) §3–§5. This component is
> independent of the HL7 v2 pipeline and nothing depends on it.

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

§2.1 The WPAS payload's root element is always `PromsEventRequest`. Routing
is on the `eventCode` **child element**, never on the root tag.

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
(`108`) has a documented `MessageHeader.source.name` and `endpoint`; the
others are **left unset rather than guessed**, and the endpoint is
overridable by environment variable so a deployment can supply real values
without a code change. This posture is preserved: an unknown source produces
a bundle with the field absent, not a fabricated one.

---

## §6 Resource mappings

*(Phase 8.)* One subsection per mapper, in the table form of
[`09-transformers.md`](09-transformers.md) §2, derived from:

```
message_header_mapper.py   patient_mapper.py       service_request_mapper.py
procedure_mapper.py        appointment_mapper.py   encounter_mapper.py
practitioner_role_mapper.py participant_mappers.py location_mapper.py
mapping_utils.py           reference_data.py
```

§6.1 `mappers/_deprecated/` (`care_plan_mapper.py`, `task_mapper.py`) is
**not** ported. Those resources are not in any bundle of §3.

---

## §7 Reference data

§7.1 Code lookups are resolved through a `ReferenceDataResolver`, with a
default implementation and an injectable one for tests.

§7.2 A code that cannot be resolved MUST leave the coding absent rather than
emitting a placeholder. A fabricated clinical code is worse than a missing
one.

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
