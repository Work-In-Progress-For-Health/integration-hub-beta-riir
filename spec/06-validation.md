# 06 — Validation and conversion

HL7 message validation, the per-flow schemas, and conversion between ER7 and
v2.xml. Implemented by `crates/ihb-validation`.

Reference implementation:
`Integration-Hub-Beta/shared_libs/hl7_validation/`.

This is the crate where the rewrite's central technical decision lives:
Python validated the v2.xml rendering of a message against an XSD with
`xmlschema`. Rust has no comparable XML Schema validator, so the XSDs are
compiled **offline** into `hl7-2` JSON dictionaries and validation becomes
`hl7-2`'s own schema-shape validation. §3 specifies that pipeline and §7
specifies the evidence required that it is equivalent.

---

## §1 What the crate provides

| Operation | Python counterpart |
| --- | --- |
| Parse ER7 into a navigable message | `parse_er7_message` |
| Convert ER7 → v2.xml, schema-unaware | `convert_er7_to_xml` |
| Convert ER7 → v2.xml against a flow schema | `convert_er7_to_xml_with_flow_schema` |
| Convert v2.xml → ER7 | `xml_to_er7` |
| Validate ER7 against a flow schema | `validate_er7_with_flow_schema` |
| Validate and convert, returning both | `validate_and_convert_er7_with_flow_schema` |
| Validate v2.xml against a flow schema | `validate_xml` |
| Validate against a standard HL7 release | `validate_er7_with_standard` |

§1.1 Each operation MUST also have a variant that takes an already-parsed
message, so a caller that has parsed once does not parse again. The Python
added these for the same reason and the ingress path depends on them.

---

## §2 Structure resolution

How the message's structure identifier is determined
(`validate.py :: _resolve_structure_info`).

§2.1 If `MSH-9.3` (message structure) is present and non-blank, it **is**
the structure identifier and no override is used.

§2.2 Otherwise, if `MSH-9.2` (trigger event) is present:

- with `MSH-9.1` (message code) present, the structure identifier is the
  trigger mapping's value for `(code, trigger)`, defaulting to
  `"{code}_{trigger}"`;
- with `MSH-9.1` absent, the structure identifier is the trigger alone.

In both cases the resolved identifier is recorded as an **override**, and
the XML conversion uses it as the root element name.

§2.3 If neither is present, the operation fails with
`"Unable to determine structure or trigger from ER7 message"`.

§2.4 The default trigger mapping is normative
(`validate.py :: _DEFAULT_TRIGGER_MAPPING`):

| Code | Trigger | Structure |
| --- | --- | --- |
| `ADT` | `A28` | `ADT_A05` |
| `ADT` | `A31` | `ADT_A05` |
| `ADT` | `A40` | `ADT_A39` |

§2.5 A caller MAY supply its own mapping, which replaces the default
entirely rather than extending it.

---

## §3 Flow schemas

### §3.1 The schema groups

`hl7_validation/resources/<group>/`. Each group is one flow's dialect,
carrying a `<release>_types.xsd`, `<release>_fields.xsd`,
`<release>_segments.xsd`, and one XSD per message structure.

| Group | HL7 release | Structures |
| --- | --- | --- |
| `phw` | 2.5 | `ADT_A05`, `ADT_A39` |
| `paris` | 2.5.1 | `ADT_A05`, `ADT_A39` |
| `chemo` | 2.4 | `ADT_A05`, `ADT_A39` |
| `pims` | 2.3.1 | `ADT_A01`, `ADT_A39`, `ADT_A40` |
| `mosaiq` | 2.5 | `ADT_A05`, `ADT_A39` |
| `wds` | 2.5 | `ADT_A05`, `ADT_A39` |

§3.1.1 There is no `mpi` group. `HL7_VALIDATION_FLOW=mpi` selects custom
field validation and routing properties only, and suppresses flow-schema
validation ([`01-architecture.md`](01-architecture.md) §2.2.1).

§3.1.2 Resolving a structure that the group has no schema for is an error
naming the group, the structure, and the available structures — matching the
Python's message so an operator's search still finds the answer:

```
No XSD mapping for flow 'phw' and trigger/structure 'ADT_A03'. Available: ADT_A05, ADT_A39
```

### §3.2 The dictionary pipeline

§3.2.1 The XSD files in
`Integration-Hub-Beta/shared_libs/hl7_validation/hl7_validation/resources/`
are copied into `crates/ihb-validation/schemas/<group>/` **unmodified**, and
are the input of record.

§3.2.2 `cargo xtask build-dictionaries` runs
`hl7-2-from-xsd-into-json-dictionary` over each group and writes
`crates/ihb-validation/dictionaries/<group>.json`.

§3.2.3 The generated dictionaries are **committed**, so a build needs no XSD
toolchain and a container image needs no generation step.

§3.2.4 `cargo xtask verify-dictionaries` regenerates into a temporary
directory and fails if the result differs from what is committed. This runs
in CI, so an XSD change that is not regenerated fails the build.

§3.2.5 Dictionaries are embedded in the binary with `include_str!` and
parsed once into an `Arc<Dictionary>` per group, cached for the process
lifetime. Nothing reads a schema from disk at run time.

### §3.3 Validation

§3.3.1 A message is validated by parsing it with the group's dictionary in
**schema-shape** mode, so the dictionary decides required fields,
repeatability, and declared components, and then running `hl7-2`'s
validation.

§3.3.2 A diagnostic of severity `Error` makes the message invalid. Severity
`Warning` MUST NOT.

§3.3.2a **`ValueFormat` diagnostics MUST be excluded.** `hl7-2` checks that
a value matches its data type's format — that a `DTM` field parses as a
timestamp, that an `NM` field is numeric. The flow XSDs do not: every
primitive in them is declared `<xsd:restriction base="xsd:string"/>`, so
`xmlschema` never checked a value's shape and the Python's verdict never
depended on one. Including these diagnostics would reject messages the
Python accepts, in production, today. See §7.6 for the measurement.

§3.3.2b Every other `Kind` — `Header`, `StructureUnknown`,
`SegmentMissing`, `FieldUnknown`, `ComponentUnknown` — is honoured at
severity `Error`.

§3.3.2c **`SegmentUnknown` and `StructureMismatch` count as errors even
though `hl7-2` reports them as warnings.** Its reasoning is right for a
bundled release: an unrecognised segment is usually a local extension, not a
malformed message. It is wrong for a flow dictionary, which was compiled
from an XSD that **enumerates exactly what that sender may send** — so a
segment the XSD does not list is a schema violation, not an extension.

Measured: appending `ZZZ|1|nonsense` to any sample message produces
`StructureMismatch` at warning severity, detailed as "the message also
carries local Z-segments, which no structure describes". The Python rejects
that message. Accepting it would mean forwarding to MPI something production
currently bounces.

§3.3.3 The error text MUST carry the structural path of the offending node
and the reason, and **MUST NOT carry any field value**. Python has an entire
function for this (`validate.py :: _format_schema_validation_error`) because
`xmlschema`'s own error string embeds the whole instance document, PII and
all. The Rust type system enforces it: `ValidationError` has a `path: String`
and a `reason: String` and no field that could hold a value.

§3.3.4 Rendered form, matching the Python's shape:

```
{reason} (path: {path})
```

---

## §4 Conversion, ER7 → v2.xml

Delegated to `hl7-2-from-er7-into-xml`. The following are additional
requirements this workspace places on the result.

§4.1 The namespace is `urn:hl7-org:v2xml`. The root element is the structure
identifier from §2.

§4.2 A field's components are emitted as typed child elements named for the
field's HL7 data type (`CX.1`, `XPN.1`, `FN.1`, …), resolving through base
types where a type inherits.

§4.3 Repetitions separated by `~` are emitted as repeated field elements —
but **only where the schema allows repetition** (`maxOccurs` greater than 1
or unbounded). Where it does not, the raw value including the `~` is emitted
as a single element.

§4.4 `MSH.2` never repeats, whatever the schema says: it holds the encoding
characters, of which `~` is one.

§4.5 A field that is required by the schema and absent from the message MUST
be emitted as an empty element, repeated to its minimum occurrence, so the
document satisfies the schema's sequence.

§4.6 **Required-segment insertion.** Two segments are synthesised when the
structure requires them and the message omits them
(`convert.py :: insert_missing_required_segments`):

- **`EVN`** is inserted after `MSH`/`SFT`, with `EVN-2` set to the value of
  `MSH-7`.
- **`PV1`** is inserted at its position in the structure's sequence, with
  `PV1-2` set to the literal `U`.

§4.6.1 Insertion happens at the point the first segment that sorts *after*
the missing one is emitted, so ordering is preserved.

§4.6.4 **What this means, stated plainly.** A message that arrives without
an event segment is **accepted**, with its event time invented from the
message header. That is an editorial act on clinical data, performed
silently, on every flow whose structure requires `EVN`. It is what
production does today, so the Rust does the same rather than start rejecting
a class of message mid-cutover — but whether it *should* is a question for
the Hub team, and it is recorded here so it is asked rather than inherited.

§4.6.5 **The repair does not work for every structure, and that is
reproduced.** The Python appends the synthesised segment to the document
root while walking the message, and by the time it reaches the first segment
that is not `MSH`/`SFT`/`EVN` it has already appended that segment's
enclosing group. So where the structure puts the segment after `EVN` inside
a **group**, the synthesised `EVN` lands after the group and the message is
rejected anyway:

```
Unexpected child with tag ADT_A39.PIDPD1MRGPV1 at position 2.
Tag EVN expected.
```

| Flow, structure | What follows `EVN` | Repair works? |
| --- | --- | --- |
| `phw` `ADT_A05` | `PID`, a top-level segment | yes |
| `chemo` `ADT_A05` | `PID`, a top-level segment | yes |
| `pims` `ADT_A39` | the `PIDPD1MRGPV1` group | **no** |
| `phw` `ADT_A39` | the `PATIENT` group | **no** |

The Rust reproduces this: it supplies the segment only when the structure's
next item is a top-level segment. That is deliberately the conservative
choice. Repairing correctly would start **accepting** PIMS messages that
production rejects today and forwarding them to MPI with a fabricated event
time — a change to clinical data flow, in the riskier direction, made as a
side effect of a rewrite. It belongs with §4.6.4 as one question for the Hub
team.

§4.7 Segments are grouped into the structure's groups where the structure
defines them, and emitted flat where it does not.

§4.8 Conversion without a schema (`convert_er7_to_xml`) still resolves the
structure per §2 and still names components by type, but performs no
required-field padding, no repetition gating, and no segment insertion.

---

## §5 Conversion, v2.xml → ER7

Delegated to `hl7-2-from-xml-into-er7`. Additional requirements:

§5.1 Group elements are flattened; grouped and flat input MUST produce the
same ER7.

§5.2 A field element's text is its value; an element with children joins
their texts with `^`, recursively.

§5.3 Repeated field elements join with `~`.

§5.4 Field positions come from the number after the element name's last `.`.
An element whose name has no `.` is skipped.

§5.5 `MSH` is written from field 2 onwards, since `MSH-1` is the separator
itself; every other segment is written from field 1.

§5.6 A segment with no numbered field elements renders as `"{TAG}|"`.

§5.7 Segments join with `\r`, with no trailing terminator.

### §5.8 Two normalisations before conversion

`hl7-2-from-xml-into-er7` needs its input adjusted in two ways before it can
read a real LIMS payload. Both are **workarounds pending an upstream fix**,
both are implemented in `ihb-validation::convert::xml_to_er7`, and both are
narrow: they touch element names and empty elements, never text content.

§5.8.1 ~~**Namespace prefixes are stripped.**~~ **Fixed upstream in
`hl7-2-from-xml-into-er7` 0.6. The workaround is removed.**

0.5 read an element's name verbatim, so a document whose elements were
prefixed — `<ns0:MSH>` — yielded `MissingMsh` and was rejected outright.
`local/sample_messages/lims-to-mpi.sample.xml` uses `ns0:`, so **every LIMS
message would have been rejected**; this module rewrote every tag to cope.
As measured against 0.5:

| Input | 0.5 | 0.6 |
| --- | --- | --- |
| `xmlns="urn:hl7-org:v2xml"` (default namespace) | reads | reads |
| `<ns0:MSH>` | **`MissingMsh`** | reads |
| `<hl7:MSH>` | **`MissingMsh`** | reads |
| no namespace | reads | reads |

0.6 matches on local name, which is what §5.8.1 asked for. `strip_namespace_prefixes`
and its tag rewriter are deleted.

§5.8.1a A prefixed document converting correctly is now a **requirement**,
not an observation, and `convert::tests::a_namespace_prefixed_document_needs_no_help`
tests it directly. The dependency is on a repository under active
development, pinned to an exact published version, so an upstream
regression arrives only on a deliberate version bump — but it would take
the whole LIMS flow down silently if it did. Deleting the workaround without leaving a test behind would have
traded a known problem for an unmonitored one.

§5.8.2 **Empty elements are dropped.** The converter renders an empty
element as HL7's explicit null, `""` — the same as an element whose text
genuinely is `""`. The two mean opposite things:

| Input | Renders as | Correct? |
| --- | --- | --- |
| `<HD.2/>` | `""` | **no** — "nothing here" became "delete this" |
| `<HD.2></HD.2>` | `""` | **no** |
| `<HD.2>""</HD.2>` | `""` | yes |
| element absent | *(nothing)* | yes |

A schema-shaped document writes every declared position, so nearly all of
them are empty; left alone, a forwarded LIMS message would carry deletion
markers through most of its fields.

§5.8.3 Dropping empty elements is **safe**, and this is the load-bearing
detail: the converter takes each component's position from its **element
name**, not from its order among its siblings. `HD.3` lands in component 3
whether or not `HD.2` is present. §5.9 is the same fact seen from the other
side.

§5.8.4 The drop runs to a fixed point. A container whose children were all
empty becomes empty only *because* they were removed, and nesting in these
documents goes three deep.

### §5.9 Finding — the Python's conversion moves components

**Status: a defect in the Python, live in the LIMS SOAP flow. Raise with the
Hub team.**

The Python's `xml_to_er7` builds a component list by joining an element's
**non-empty** children with `^` (`convert.py ::
_extract_text_from_element`). Position therefore comes from order among the
surviving children, so dropping an empty one shifts everything after it.

Measured on `local/sample_messages/lims-to-mpi.sample.xml`, whose source ER7
is known exactly because `_gen_sample.py` generated it:

```
source        PID|1||B0000010612^^^328^PI||…|||Welsh Parliament^Cardiff Bay^CARDIFF^^CF99 1SN
              PD1|||^^UNK|UNK

Python        PID|1||B0000010612^328^PI||…|||Welsh Parliament^Cardiff Bay^CARDIFF^CF99 1SN
              PD1|||UNK|UNK

Rust          PID|1||B0000010612^^^328^PI||…|||Welsh Parliament^Cardiff Bay^CARDIFF^^CF99 1SN
              PD1|||^^UNK|UNK        ← byte-identical to the source
```

What that means in clinical terms:

| Value | Belongs in | Python puts it in |
| --- | --- | --- |
| `328`, the assigning authority | `PID-3.4.1` | `PID-3.2` |
| `PI`, the identifier type | `PID-3.5` | `PID-3.3` |
| `CF99 1SN`, the postcode | `PID-11.5` | `PID-11.4` (the county) |
| `UNK`, the practice | `PD1-3.3` | `PD1-3.1` |

So MPI receives, for every LIMS message, a patient identifier whose
assigning authority is in the wrong component and an address whose postcode
is in the county field.

**This is not a rewrite decision.** It is a live data-quality question about
the LIMS → MPI flow as it runs today, and it goes to the Hub team with the
evidence above. The Rust does the right thing; the tests in
`crates/ihb-validation/tests/soap_payload.rs` pin every row of that table so
it cannot regress.

§5.9.1 Because of this, §7.3's "canonically equal to the Python's output"
requirement **does not apply to the XML → ER7 direction**. The Rust is
deliberately not bug-compatible here: reproducing this would mean knowingly
sending MPI a misfiled identifier. Recorded as a deviation in
[`plan.md` §9](../plan.md#9-deliberate-deviations-from-the-python).

---

## §6 Standard-release validation

`standard_validate.py`.

§6.1 The supported releases are exactly `2.4`, `2.5`, `2.5.1`, `2.6`. Any
other value fails with

```
Unsupported HL7 version '{version}'. Supported versions: 2.4, 2.5, 2.5.1, 2.6
```

§6.2 When validating an already-parsed message, a mismatch between the
message's own version and the requested version fails with

```
Message version {actual} does not match requested version {requested}
```

§6.3 A validation failure renders as

```
Standard HL7 v{version} validation failed: {reason}
```

§6.4 A parse failure renders as `Unable to parse ER7 message: {reason}`. The
prefix is the literal `Unable to parse ER7 message` and is matched by tests
in the Python; it is preserved.

---

## §7 Equivalence evidence

This section is what makes §3's substitution acceptable, and it is a
blocking requirement on Phase 2.

§7.1 A **differential corpus** is assembled from every message fixture in
the Python test suite: `hl7_validation/tests/test_{phw,paris,chemo,pims,
mosaiq,wds}.py`, `hl7_chemo_transformer/tests/chemo_messages.py`,
`hl7_pims_transformer/tests/pims_messages.py`, and
`Integration-Hub-Beta/local/sample_messages/`.

§7.2 For every message in the corpus, the Rust MUST produce the **same
verdict** (valid / invalid) as the Python, for its flow. A disagreement is a
blocking defect and MUST be triaged before any service depends on this
crate.

§7.3 For every message in the corpus that the Python converts to v2.xml, the
Rust MUST produce **XML-canonically equal** output: same elements, same
order, same text. Attribute-level and whitespace differences are not
material; element order is.

§7.4 The corpus round-trips: ER7 → v2.xml → ER7 MUST return the original
message, modulo the required-segment insertion of §4.6 and the trailing
empty fields the Python also drops. The Python has this test
(`test_convert_no_data_loss.py`) and it is ported.

§7.5 The corpus fixtures are committed under
`crates/ihb-validation/tests/corpus/` with the Python's output alongside as
the golden file, so the comparison does not require a Python runtime in CI.

### §7.5a Differential result — Phase 2

**30 of 30 cases agree.**

Six sample messages from `Integration-Hub-Beta/local/sample_messages/`, each
in five variants — as sent, and with `MSH`, `EVN` or `PID` removed, and with
an unknown `Z`-segment appended. Verdicts and XML captured from the Python
once, committed as `crates/ihb-validation/tests/corpus/python-verdicts.json`,
and compared by `crates/ihb-validation/tests/differential.rs`.

Reaching agreement took three adjustments, each triaged rather than papered
over:

| Disagreement | Cases | Resolution |
| --- | --- | --- |
| Rust rejected messages with no `EVN`; Python accepted them | 5 | §4.6's required-segment repair was specified but unimplemented. Implemented. |
| Rust accepted a `pims` message with no `EVN`; Python rejected it | 1 | §4.6.5 — the Python's repair misplaces the segment for grouped structures. Reproduced deliberately. |
| Rust accepted messages carrying a `Z`-segment; Python rejected them | 6 | §3.3.2c — `StructureMismatch` counts as an error against a flow schema. |

The corpus also asserts, for every case: the same resolved structure (§2),
v2.xml in the right namespace with the right root (§4.1), and that **no
rendered error contains any value taken from the message's own `PID` or
`PD1` segments** ([`index.md`](index.md) §0.8.4).

### §7.6 Phase 0 probe result

Recorded here because §3.3.2a's exclusion rests on it.

**Method.** The six schema groups were converted with
`hl7-2-from-xsd-into-json-dictionary`. Twelve cases — each of the six
messages in `Integration-Hub-Beta/local/sample_messages/` as-is, and each
with its `PID` segment removed — were put through
`validate_er7_with_flow_schema` in the Python and through `hl7-2` in
schema-shape mode in Rust.

**Result.** 12 of 12 verdicts agree. Every as-is message is valid in both;
every `PID`-less message is invalid in both, the Rust reporting
`SegmentMissing` and the Python reporting the same absence as an incomplete
`ADT_A05` content model.

**The one adjustment.** Before the §3.3.2a exclusion, Rust additionally
rejected the PHW sample on two `ValueFormat` diagnostics:

```
MSH[1]-7[1].1: "2025-05-05 23:23:32" is not a valid DTM value
PD1[1]-3[1].3: "W00000" is not a valid NM value
```

Both are real observations about the data and both are outside what the
XSDs express. The first is the very value the PHW transformer exists to
normalise ([`09-transformers.md`](09-transformers.md) §3.4) — rejecting it
at ingress would break the PHW flow entirely. Excluding `ValueFormat` is
therefore not a concession; it is the correct reading of what the Python
validated.

**Also confirmed.** The generated dictionaries are byte-identical, in their
`types`, `segments` and `structures`, to the `dictionary.json` files already
committed in the Python's `resources/` directories. The two toolchains
agree.

**Not yet measured.** This probe covers structural verdicts only. The XML
equivalence of §7.3 and the round-trip of §7.4 are P2.12–P2.13.

---

## §8 Errors

§8.1 One error type, `ValidationError`, with variants:

| Variant | When |
| --- | --- |
| `Parse` | The ER7 could not be parsed |
| `Structure` | §2.3 — no structure and no trigger |
| `NoSchema` | §3.1.2 — the group has no schema for this structure |
| `Schema` | §3.3 — a schema diagnostic, carrying `path` and `reason` |
| `UnsupportedVersion` | §6.1 |
| `VersionMismatch` | §6.2 |
| `Xml` | The v2.xml could not be read |

§8.2 No variant may hold a message payload or a field value. There MUST be a
test that pushes a payload containing a known marker through every failure
path and asserts the marker never appears in the rendered error.

§8.3 A `ValidationResult` type carries the outcome and the generated XML
together, so a caller that needs both does not convert twice
(`validation_result.py :: ValidationResult`):

| Field | Notes |
| --- | --- |
| `xml_string` | The v2.xml rendering, present even when invalid |
| `structure_id` | §2 |
| `message_type` | `MSH-9.1` |
| `trigger_event` | `MSH-9.2` |
| `message_control_id` | `MSH-10` |
| `is_valid` | |
| `error_message` | Present when invalid |

### §8.4 The stored XML differs from the Python's, and how

Measured by `hl7-server/tests/golden.rs`, which runs each flow's sample
message through the whole server and compares the resulting `XmlPayload`
against the XML the Python produced for the same message (§7's corpus).

§8.4.0 **Serialisation differs and always will.** Neither form is more
correct and none of it changes meaning:

| | Python (`xmlschema`) | Rust (`hl7-2-from-er7-into-xml`) |
| --- | --- | --- |
| Declaration | none | `<?xml version="1.0" encoding="UTF-8"?>` |
| Namespace | `ns0:` prefix on every element | default `xmlns=` |
| Whitespace | none | indented, one element per line |

A consumer that string-matches the stored column on `<ns0:PID.3>` would stop
finding it. Nothing in this workspace does, but the dashboard and any ad-hoc
SQL are outside it, so this is flagged rather than assumed harmless.

§8.4.1 **`MSH-9.3` is populated where the Python left it empty.** Four of
the six samples agree element for element. The chemocare sample's `MSH-9` is
`ADT^A28`, with no structure component: the Rust writes the resolved
structure (`ADT_A05`) into `MSH-9.3` before converting, because the
converter names the root element from `MSH-9` and would otherwise emit
`<ADT_A28>`, a root no schema knows. The Python named the root separately
and left the component empty.

The audit record therefore shows a component the source system did not
send. The **forwarded** message is unaffected — this server sends the raw
ER7 — but a reader of `XmlPayload` should know it is a rendering, not a
transcript.

§8.4.2 **HL7's explicit null is lost in the rendering.** The pims sample's
`PID-5` is `test^test^""^^MISS`. In HL7 `""` is an *explicit null*, meaning
"delete this value", which is materially different from an absent value.
The Python carried the two characters into the XML; the Rust's converter
renders an empty element.

This is a loss of meaning, and it is worth the Hub team's attention: any
future consumer that drives an update from `XmlPayload` rather than from
`RawPayload` would turn a deletion into a no-change. The forwarded ER7
keeps the `""`, and `golden.rs` asserts that byte-for-byte, so MPI is not
affected today. The proper fix is upstream in
`hl7-2-from-er7-into-xml`.

§8.4.3 The golden test asserts the difference set is **exactly** §8.4.1 and
§8.4.2 — no more, and no fewer. A new divergence fails it, and so does
fixing one of these without removing it from the list. A list of known
exceptions that is never rechecked is a list of defects nobody is looking
at.

---

## §9 Real XSD validation, through two engines

Implemented by `crates/ihb-validation/src/xsd.rs`.

§3.2 exists because there was no mature XML Schema validator in Rust when
this rewrite began. There now are two — `xsd-schema` and `uppsala` — and
this section specifies using **both**, comparing them, and reporting when
they disagree.

### §9.1 Why two

One young crate deciding whether a clinical message is well-formed is a
single point of failure with no way to notice it has failed. Two independent
implementations give a cheap, continuous check: agreement is evidence, and
**disagreement is information that MUST be surfaced rather than resolved
silently**.

§9.1.1 The verdict is the primary engine's. The cross-check's verdict is
recorded alongside it, and `Report::engines_agreed` reports whether they
matched.

§9.1.2 A disagreement MUST NOT be averaged, voted on, or quietly resolved in
favour of whichever is more convenient. It is reported.

### §9.2 The engines

| Role | Crate | Status |
| --- | --- | --- |
| Primary | `xsd-schema` | Agrees with the Python on 24 of 24 (§9.3) |
| Cross-check | `uppsala` | Currently defective on these schemas (§9.4) |

### §9.3 Differential result — `xsd-schema`

**24 of 24 validatable corpus cases agree with the Python's `xmlschema`**,
including the `xs:include` chains and the padded documents. The six
remaining cases are those where the Python raised before producing XML, so
there is nothing to compare.

This is a stronger result than §3.2's dictionary route can claim, because it
is the *same kind* of validation the Python performs rather than an
equivalent one.

### §9.4 Finding — `uppsala` misresolves an unprefixed type reference

**Status: a defect in `uppsala` 0.9.0, the latest published version.
Reported upstream as <https://github.com/kushaldas/uppsala/issues/39>. The cross-check
fails until it is fixed.**

An earlier draft of this section claimed it had been reported before it
had, while `tasks.md` P2.17 tracked the reporting as outstanding — the two
disagreed for some time. It is now true, and the issue number is here so
the claim can be checked rather than taken on trust. The working copy of
the report is
[`docs/upstream/uppsala-id-type-resolution.md`](../docs/upstream/uppsala-id-type-resolution.md).

An unprefixed `base="ID"` in a schema whose default namespace is its target
namespace must resolve to **that schema's** `ID`, not to the built-in
`xs:ID`. `uppsala` resolves it to the built-in.

The HL7 v2.xml schemas define exactly such a type:

```xml
<xsd:schema xmlns="urn:hl7-org:v2xml" targetNamespace="urn:hl7-org:v2xml">
    <xsd:simpleType name="ID"><xsd:restriction base="xsd:string"/></xsd:simpleType>
    <xsd:complexType name="HD.3.CONTENT">
        <xsd:simpleContent><xsd:extension base="ID"/></xsd:simpleContent>
    </xsd:complexType>
```

So every `HD.3` — and every other field of HL7 type `ID` — is checked
against XML's `NCName` rules, which an unconstrained string need not satisfy.
Empty values, values with spaces, and values starting with a digit are all
rejected. `252` is a sending application.

Minimal reproduction, fifteen lines, no HL7 involved: a schema with a local
`ID` type; `<root></root>`, `<root>252</root>` and `<root>a b</root>` are all
valid and all three are rejected.

Effect on the corpus: 11 of 24 cases wrongly invalid, **including every
`as-is` sample message**.

§9.4.1 `ihb-validation` pins this with a test that asserts the disagreement.
When the defect is fixed the test fails, and the correct response is to
promote the cross-check — not to weaken the assertion.

### §9.5 Cost

Measured, release build, PHW `ADT_A05`:

| | |
| --- | --- |
| `SchemaValidator::new` alone | 10.2 ms |
| Construct per document, then validate | 10.2 ms |
| **Reuse one validator** | **3.6 µs** |

§9.5.1 The compiled validator MUST be cached, not merely the schema set.
Constructing one per document is roughly 2,800× slower and would cap an
ingress at about five messages a second.

§9.5.2 The cache MUST be **thread-local**. `xsd_schema::SchemaSet` holds
`RefCell`s: it is `Send` but not `Sync`, so it cannot be shared across
threads at all. A global behind a mutex would serialise every validation in
the process. Thirteen schemas and a bounded worker pool make per-thread
copies bounded and cheap.

§9.5.3 Steady-state throughput, from `cargo bench -p ihb-validation`:

| Route | Per message |
| --- | --- |
| Dictionary, end to end (§3.2) | 68 µs |
| `xsd-schema` alone | 95 µs |
| `uppsala` alone | 113 µs |
| Both engines, compared | 207 µs |

All four are fast enough for an ingress path. Cost is not what decides
between them.

### §9.6 What must be true before this replaces §3.2

This module is a cross-check today, not the ingest path. Before it could
take over:

1. §9.4 fixed, so the cross-check is meaningful rather than noise.
2. The XSDs available as **files** in the container image. Both engines
   resolve `xs:include` by path, unlike the dictionaries of §3.2.5 which are
   embedded in the binary.
3. The differential of §7 re-run against the XSD route with **all** cases
   agreeing, including the `error` cases §9.3 currently skips.
4. A decision, with the Hub team, on the three behaviours §3.2's route
   reproduces deliberately: §3.3.2a, §3.3.2c and §4.6.5. Real XSD validation
   would change all three, and each is a change in which messages reach MPI.
5. ~~**§9.7 resolved.** Promotion would put untrusted XML in front of a
   parser with two open denial-of-service advisories.~~ **Done**
   (2026-08-22) — `xsd-schema` 0.1.4 brings `quick-xml` 0.41.0 and both
   advisories are closed. The other four preconditions still stand.

### §9.7 Finding — `xsd-schema` pinned a vulnerable `quick-xml`

**Status: closed (2026-08-22).** `xsd-schema` 0.1.4 depends on `quick-xml`
0.41.0, which carries the fixes.

`xsd-schema` 0.1.3 depended on `quick-xml` 0.31.0, which carries
RUSTSEC-2026-0194 (quadratic run time checking a start tag for duplicate
attribute names) and RUSTSEC-2026-0195 (unbounded namespace-declaration
allocation in `NsReader`). Both are denial of service on **untrusted XML**,
and both needed `quick-xml >= 0.41`. At the time 0.1.3 was `xsd-schema`'s
latest release, so there was no upgrade path from here.

§9.7.1 **Nothing was exposed while it was open.** `quick-xml` enters the
graph through `xsd-schema` alone; the SOAP ingress
([`08-hl7-soap-server.md`](08-hl7-soap-server.md)) and the ER7/XML
conversions of §5 use their own parsers. The only documents reaching it were
those `er7_to_xml_for_flow` generates, and the generator emits a fixed
element structure carrying one namespace declaration and no other
attributes — so neither the duplicate-attribute nor the `xmlns`-flood shape
could be reached through it, whatever the ER7 said.

§9.7.2 **Resolution.** The workspace moved to `xsd-schema` 0.1.4, which
depends on `quick-xml` 0.41.0. `cargo tree -i quick-xml --workspace` shows
one version reached by one path, and `cargo deny check advisories` passes
with **no ignore for either advisory** — both entries were removed from
`deny.toml` rather than left in place, so a regression fails the gate
instead of being silently covered by an ignore nobody re-reads.

§9.7.3 **This clears one of §9.6's five preconditions, not §9.6 itself.**
Item 5 is resolved; items 1 to 4 are untouched. In particular §9.4's
`uppsala` type-resolution defect is still open, and item 4 — a decision on
three behaviours whose change alters which messages reach MPI — is still
the Hub team's to make. `xsd::validate` remains a **cross-check**, not the
ingest path.
