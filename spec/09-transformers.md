# 09 — Transformers

The transformer runtime, and the PHW, ChemoCare and PIMS mappings.
Implemented by `crates/ihb-transformer` and the three transformer crates.

Reference implementation:
`Integration-Hub-Beta/shared_libs/transformer_base_lib/`,
`Integration-Hub-Beta/hl7_{phw,chemo,pims}_transformer/`.

This is the largest body of business rules in the estate. §3–§5 are
mapping tables derived line by line from the Python mappers; they are the
normative statement of the rules, and the golden tests of
[`17-testing-and-quality.md`](17-testing-and-quality.md) §4 are the proof
the Rust agrees.

---

## §1 The runtime

`ihb-transformer` provides a `Transformer` trait and a `run` function.

```rust
pub trait Transformer {
    /// The name used in audit text and span names, e.g. "PHW".
    fn name(&self) -> &str;
    /// Transform one message. Returns the message to forward.
    fn transform(&mut self, message: &Message) -> Result<Message, TransformError>;
    /// Audit text for MESSAGE_RECEIVED. Default per §1.4.
    fn received_audit_text(&self) -> String { … }
    /// Audit text for MESSAGE_PROCESSED. Default per §1.5.
    fn processed_audit_text(&self, message: &Message) -> String { … }
}
```

§1.1 `run` wires: configuration
([`02-configuration.md`](02-configuration.md) §5), an egress queue sender, an
ingress queue receiver, a health-check listener, telemetry, and a receive
loop that runs until shutdown.

§1.2 The receive loop requests `MAX_BATCH_SIZE` messages per call and
processes them per-message
([`03-message-bus.md`](03-message-bus.md) §5.2).

§1.3 Per message (`message_processor.py :: process_message`):

```
1  decode the body as UTF-8
2  read the incoming application_properties
3  if present, log CorrelationId / WorkflowID / SourceSystem /
   MessageReceivedAt at info; if absent, log
   "No application_properties found on message" at warn
4  MESSAGE_RECEIVED event with the received audit text
5  parse the ER7
6  transform
7  send the transformed ER7 to the egress queue, carrying the
   INCOMING application_properties unchanged
8  MESSAGE_PROCESSED event with the processed audit text
9  return handled
```

§1.3.1 Step 7 is the mechanism by which the correlation id survives the hop.
The properties are forwarded **as received**, not rebuilt.

§1.3.2 The message id is not set on the outgoing message; only ingress sets
one.

§1.3.3 **A transformer may change the outgoing properties.** Step 7 sends
what `Transformer::egress_properties(incoming, transformed)` returns; the
default returns `incoming` unchanged, so §1.3.1 holds for every transformer
that does not override it (upstream `BaseTransformer.get_egress_properties`,
`04b30e7`). It receives the **transformed** message, so a transformer that
changes something a topic subscription filters on can correct the property
that carries it. It cannot fail: an override that cannot read what it needs
leaves the incoming value in place. Only PIMS overrides it (§5.11).

§1.4 Default received audit text:
`Message received for {name} transformation`

§1.5 Default processed audit text:
`{name} transformation applied for SENDING_APP: {MSH-3.1}`, with `UNKNOWN`
when `MSH-3.1` cannot be read.

§1.6 Failure handling:

| Failure | Log | `MESSAGE_FAILED` validation result | Result |
| --- | --- | --- | --- |
| Transform error | `Failed to transform {name} message: {e}` at error | `{name} transformation failed` | not handled |
| Anything else | `Unexpected error during message processing: {e}` at error | `Unexpected processing error` | not handled |

§1.7 "Not handled" abandons the message and every message behind it in the
batch, per [`03-message-bus.md`](03-message-bus.md) §5.2.2.

---

## §2 Mapping conventions

The Python mappers build a new `hl7apy` message and copy into it. The Rust
does the same with `er7`/`hl7-2`, and these conventions define what the
Python's helpers mean.

§2.1 **`set_nested_field(src, dst, path)`** copies the value at `path` from
the source segment to the same path in the destination **only when the
source value is non-empty**. A missing or empty source leaves the
destination untouched. In the tables below this is written **`copy`**.

§2.2 **`copy_segment_fields_in_range(src, dst, prefix, a, b)`** copies
fields `a..=b` inclusive, repetition by repetition, preserving every
repetition including empty ones. Written **`copy range a..=b`**.

§2.2.1 **"Repetition by repetition" is a requirement, not a description of
an implementation.** A copy MUST address each repetition individually
(`PID-13[1]`, `PID-13[2]`, …). It MUST NOT read a field as one string and
write that string back.

The reason is `hl7-2`'s escaping, which is correct and therefore dangerous
here. `Message::set` escapes what it is given, so a `~` in a value survives
as `\R\` — right, because a tilde inside a component is data. But it means
writing `"555-1111~555-2222"` back into `PID-13` produces **one** repetition
containing a literal tilde rather than **two** repetitions, and the message
reaches MPI with one nonsense phone number instead of two real ones.

Measured, not assumed:

```
source PID-13 repetitions      ["555-1111", "555-2222"]
set("PID-13", "555-1111~555-2222")   renders  555-1111\R\555-2222   ✗
set("PID-13[1]", …) then [2]         renders  555-1111~555-2222     ✓
```

`ihb-fields::copy` does this, and carries the test that pins it. Any mapper
that writes a repeating field by another route MUST have its own test
asserting on the **rendered** message, since the in-memory tree is not where
the loss shows up.

§2.2.2 ~~**Open question — an all-empty repeating field.**~~ **Settled by
the golden tests of §6.6.** The answer is that a field is copied
**structurally**, and a repetition count is preserved whether or not the
repetitions carry anything.

The question was what happens to a field that is present but whose every
repetition is empty — `PID-3` of `~` — given that the range-copy documents
itself as preserving "all repetitions … (including empty)" while the
single-field copy skips an empty source.

Three findings came out of running the real messages through both
implementations, and each is now a primitive in `ihb-fields`:

§2.2.2a **A whole field is copied by cloning its parsed structure, never by
writing its value back.** §2.2.1 states this for the repetition separator.
It is the same defect for `^` and `&`, and repetition-by-repetition copying
does **not** fix those, because the component separators live *inside* each
repetition. Reading `8888888^^^252^PI` and writing it through `set` yields
`8888888\S\\S\\S\252\S\PI` — one component, every separator escaped —
and MPI receives an identifier that is one long nonsense string. Measured
against the ChemoCare and PHW corpora, where it affected every message.
`ihb_fields::copy` clones `er7::Field` and writes no text at all.

§2.2.2b **An empty trailing repetition is kept.** The Python's mappers add a
destination repetition per source repetition unconditionally, so a field
ending in `~` keeps it. A copy that declines to write empty values never
creates the repetition, and the rendered field is one `~` short. Three of
the nine ChemoCare fixtures differ on exactly this. `ihb_fields::ensure_repetitions`
is the remedy, and a repetition count is information: `PID-13` with four
repetitions, the fourth blank, says something different from one with three.

§2.2.2c **Trailing empty components within a repetition are dropped.** The
Python's `hl7apy` does this on the way out, so `^^W00000^` renders as
`^^W00000` and `^^` renders as empty. HL7 states that trailing component
delimiters are not significant, so nothing is lost, but the golden
comparison is byte for byte. `ihb_fields::trim_trailing_separators` applies
it, and only a transformer calls it — an ingress server forwards its input
untouched (`spec/07-hl7-server.md` §9.8) and must not normalise anything.

§2.2.2d **HL7's explicit null survives a copy.** `hl7-2` decodes `""` to the
empty string, correctly — decoded, it *is* empty — so a copy that asks only
for the value drops it, and "clear the value you hold" becomes "I am saying
nothing about this". PIMS sends `""` in `PID-5.3` on every message.
`ihb_fields::copy` checks the parsed structure and calls `set_null`. This is
the same loss recorded at [`06-validation.md`](06-validation.md) §8.4.2 for
the XML rendering, found independently here.

§2.3 **`get_hl7_field_value(seg, path)`** reads a value, returning `""` for
anything missing. Written **`read`**.

§2.4 A destination path written as a literal in the tables is **set
unconditionally**.

§2.5 A new message is created at HL7 version **2.5** unless a row says
otherwise. `MSH-12.1` is set to `2.5` explicitly where the Python does so.

§2.6 The Rust MUST NOT reproduce `hl7apy`'s incidental behaviours: value
rewriting for `NM` and `SI` types (which the Python patches out in
`field_utils_lib/hl7apy_fidelity.py`), or auto-population of `MSH-7` on a
new message. `er7` does not rewrite values, and nothing is auto-populated;
where the Python relies on auto-population the table says so explicitly.

---

## §3 PHW → MPI

`hl7_phw_transformer/`. The lightest transformer: it copies the message and
normalises two timestamps.

Segment order in the output: `MSH`, `EVN`, `PID`, then every other segment
in source order.

### §3.1 `MSH`

| Destination | Source | Rule |
| --- | --- | --- |
| `MSH-3` … `MSH-6` | same | copy range 3..=6 |
| `MSH-7.1` | `MSH-7.1` | **transformed** per §3.4 |
| `MSH-8` … `MSH-21` | same | copy range 8..=21 |
| `MSH-12[0]` | `MSH-12[0]` | copy, first repetition, failures ignored |

§3.1.1 `MSH-7` is excluded from the copy ranges and handled explicitly, so
the new message's auto-populated timestamp is not overwritten when the
source omits one. In Rust nothing is auto-populated, so an absent `MSH-7`
leaves `MSH-7` absent.

§3.1.2 When `MSH-7.1` is present, the transformation of §3.4 is applied and
the pair `(original, transformed)` is recorded for the audit text of §3.5.

### §3.2 `EVN`

| Destination | Source | Rule |
| --- | --- | --- |
| `EVN` | `EVN` | copy, whole segment |

### §3.3 `PID`

Present only when the source has a `PID` segment.

| Destination | Source | Rule |
| --- | --- | --- |
| `PID-1` … `PID-28` | same | copy range 1..=28 |
| `PID-29.1` | `PID-29.1` | **transformed** per §3.6 |
| `PID-30` … `PID-39` | same | copy range 30..=39 |

### §3.4 Date/time normalisation

`datetime_transformer.py`.

1. If the value already parses as `%Y%m%d%H%M%S`, return it unchanged.
2. Otherwise parse it as `%Y-%m-%d %H:%M:%S` and re-format as
   `%Y%m%d%H%M%S`.
3. If neither parses, the transform **fails** — this is the `ValueError`
   the Python raises, and it surfaces as §1.6's transform error.

### §3.5 Audit text

`phw_transformer.py :: get_processed_audit_text`. When either
transformation ran, the processed audit text is
`HL7 transformations applied: {details}` where `{details}` joins with
`"; "`:

- `DateTime transformed from {a} to {b}`
- `Date of death transformed from {a} to {b}`

When neither ran, the default of §1.5 is used.

§3.5.1 The Python stores these on the transformer instance between
`transform_message` and `get_processed_audit_text`. The Rust MUST return
them from `transform` instead, so the runtime is not carrying per-message
state on a shared object.

### §3.6 Date of death

`date_of_death_transformer.py`. Applied to `PID-29.1` when non-blank.

| Input | Output |
| --- | --- |
| Blank or whitespace | `""` |
| `RESURREC`, case-insensitive, after trimming | `""` (two literal double-quote characters — HL7's "explicit null") |
| Anything else | The input, trimmed |

---

## §4 ChemoCare → MPI

`hl7_chemo_transformer/`. Rebuilds the message from scratch, mapping named
fields only.

Order: `MSH`, `EVN`, `PID`, `PD1`, `NK1`, then the non-specific segments of
§4.6.

### §4.1 `MSH`

| Destination | Source | Rule |
| --- | --- | --- |
| `MSH-1` | `MSH-1` | set |
| `MSH-2` | `MSH-2` | set |
| `MSH-3.1` | `MSH-3.1` | copy |
| `MSH-4.1` | `MSH-4.1` | copy |
| `MSH-5.1` | `MSH-5.1` | copy |
| `MSH-6.1` | `MSH-6.1` | copy |
| `MSH-7.1` | `MSH-7.1` | copy |
| `MSH-8` | `MSH-8` | copy |
| `MSH-9.1` | `MSH-9.1` | copy |
| `MSH-9.2` | `MSH-9.2` | copy |
| `MSH-9.3` | — | literal `ADT_A05` |
| `MSH-10` | `MSH-10` | set |
| `MSH-11` | `MSH-11` | set |
| `MSH-12.1` | — | literal `2.5` |
| `MSH-13` … `MSH-21` | same | copy range 13..=21 |

### §4.2 `EVN`

| Destination | Source | Rule |
| --- | --- | --- |
| `EVN` | `EVN` | copy, whole segment |

### §4.3 `PID`

Skipped entirely when the source has no `PID`.

| Destination | Source | Rule |
| --- | --- | --- |
| `PID-1` | `PID-1` | copy |
| `PID-3[1].4.1` | — | literal `NHS` |
| `PID-3[1].5` | — | literal `NH` |
| `PID-3[1].1` | §4.3.1 `pid2` | set when `pid2` non-empty |
| `PID-3[2].4.1` | §4.3.1 `msh3` | set |
| `PID-3[2].5` | — | literal `PI` |
| `PID-3[2].1` | §4.3.2 | set when both `pid2` and a health-board code resolve |
| `PID-5.1.1` | `PID-5.1.1` | copy |
| `PID-5.2` … `PID-5.8` | same | copy, each |
| `PID-5.9.1` | `PID-5.9.1` | copy |
| `PID-5.10`, `PID-5.11` | same | copy |
| `PID-6.1.1` | `PID-6.1.1` | copy |
| `PID-7`, `PID-8` | same | copy |
| `PID-9.1.1` | `PID-9.1.1` | copy |
| `PID-10.1` | `PID-10.1` | copy |
| `PID-11.1.1` | `PID-11.1.1` | copy |
| `PID-11.2` … `PID-11.5`, `PID-11.7`, `PID-11.8` | same | copy, each |
| `PID-13[n].1`, `.2`, `.4` | `PID-13[n]` same | copy, **every repetition** |
| `PID-14.1`, `PID-14.2` | same | copy |
| `PID-17.1` | `PID-17.1` | copy |
| `PID-22.1` | `PID-22.1` | copy |
| `PID-29.1` | `PID-29.1` | copy |
| `PID-32` | `PID-32` | copy |

§4.3.1 Two derived values:

- `pid2` = `PID-2.1`, falling back to the whole of `PID-2` when `PID-2.1` is
  empty.
- `msh3` = `MSH-3.1`, falling back to the whole of `MSH-3` when `MSH-3.1` is
  empty.

§4.3.2 **Health-board prefixing.** `PID-3[2].1` is set to
`{health_board}{pid2}` where `health_board` comes from this table
(`pid_mapper.py :: HEALTH_BOARD_MAPPING`):

| `msh3` | Prefix |
| --- | --- |
| `224` | `VCC` |
| `212` | `BCUCC` |
| `192` | `SWWCC` |
| `245` | `SEWCC` |

An `msh3` outside the table leaves `PID-3[2].1` **unset**. This is
deliberate: an unrecognised health board must not produce a fabricated
identifier.

§4.3.3 Both `PID-3` repetitions are created unconditionally, with their
authority and type codes, even when no identifier value is available.

### §4.4 `PD1`

Skipped when the source has no `PD1`.

| Destination | Source | Rule |
| --- | --- | --- |
| `PD1-3.1`, `.3`, `.4`, `.5`, `.7`, `.9` | same | copy, each |
| `PD1-3.6.1` | `PD1-3.6.1` | copy |
| `PD1-3.8.1` | `PD1-3.8.1` | copy |
| `PD1-4.1`, `.3`, `.4`, `.6` | same | copy, each |
| `PD1-4.2.1` | `PD1-4.2.1` | copy |

### §4.5 `NK1`

Skipped when the source has no `NK1`.

| Destination | Source | Rule |
| --- | --- | --- |
| `NK1-2.2`, `NK1-2.7` | same | copy |
| `NK1-2.1.1` | `NK1-2.1.1` | copy |
| `NK1-3.1` | `NK1-3.1` | copy |
| `NK1-4.2`, `NK1-4.7` | same | copy |
| `NK1-4.1.1` | `NK1-4.1.1` | copy |
| `NK1-5.1` | `NK1-5.1` | copy |

### §4.6 Non-specific segments

Copied verbatim when present, in this order:

```
PV1  PV2  OBX  AL1  DG1  PR1  GT1  IN1  IN2  IN3
```

§4.6.1 A failure copying one of these MUST be logged at warn as
`Error copying segment {name}: {error}` and MUST NOT fail the transform —
the required mapping has already completed.

---

## §5 PIMS → MPI

`hl7_pims_transformer/`. The most opinionated mapping: it rewrites routing
identifiers, normalises trigger events, and strips timezones.

§5.0 **Reparse flat.** The source message is re-parsed with group detection
**off** before mapping. Some PIMS structures (notably `ADT_A39`, used for
the A40 merge trigger) nest `PID`/`PD1`/`MRG`/`PV1` inside a repeating
`PATIENT` group, and every mapper below addresses segments directly.

Order: `MSH`, `EVN`, `PID`, `PD1`, `PV1`, `MRG`, then §5.7.

### §5.1 `MSH`

| Destination | Source | Rule |
| --- | --- | --- |
| `MSH-1` | `MSH-1` | set |
| `MSH-2` | `MSH-2` | set |
| `MSH-3.1` | — | literal `103` |
| `MSH-4.1` | — | literal `103` |
| `MSH-5.1` | — | literal `200` |
| `MSH-6.1` | — | literal `200` |
| `MSH-7.1` | `MSH-7.1` | **timezone-stripped** per §5.6 |
| `MSH-8` | `MSH-8` | copy |
| `MSH-9.1` | — | literal `ADT` |
| `MSH-9.2`, `MSH-9.3` | `MSH-9.2` | **normalised** per §5.1.1 |
| `MSH-10` | `MSH-10` | set |
| `MSH-11` | `MSH-11` | set |
| `MSH-12.1` | — | literal `2.5` |
| `MSH-13` | `MSH-13` | copy |
| `MSH-17` | — | literal `GBR` |
| `MSH-19.1` | — | literal `EN` |

§5.1.1 **Trigger normalisation.** The source `MSH-9.2`, trimmed and
upper-cased, maps as:

| Source trigger | `MSH-9.2` | `MSH-9.3` |
| --- | --- | --- |
| `A04`, `A28` | `A28` | `ADT_A05` |
| `A08`, `A31` | `A31` | `ADT_A05` |
| anything else, including absent | `A40` | `ADT_A39` |

The catch-all is deliberate: PIMS sends `A40` for merges and nothing else
reaches this transformer.

### §5.2 `EVN`

| Destination | Source | Rule |
| --- | --- | --- |
| `EVN-1` | `EVN-1` | copy |
| `EVN-2.1` | `EVN-2.1` | timezone-stripped per §5.6, when non-empty |
| `EVN-6.1` | `EVN-6.1` | timezone-stripped per §5.6, when non-empty |

### §5.3 `PID`

Skipped when the source has no `PID`.

| Destination | Source | Rule |
| --- | --- | --- |
| `PID-3[1]` | `PID-3[0]` | §5.3.1 |
| `PID-3[2]` | `PID-3[1]` | §5.3.2 |
| `PID-5.1.1` | `PID-5.1.1` | copy |
| `PID-5.2` … `PID-5.5` | same | copy, each |
| `PID-7.1` | `PID-7.1` | timezone-stripped per §5.6, when non-empty |
| `PID-8` | `PID-8` | copy |
| `PID-11.1.1` | `PID-11.1` | set — the whole field, because `SAD` does not exist in HL7 2.3.1 |
| `PID-11.2` … `PID-11.5` | same | copy, each |
| `PID-13[n].1` | `PID-13[n].1` | copy, **every repetition** |
| `PID-14.1` | `PID-14.1` | copy |
| `PID-29.1` | `PID-29.1` | §5.3.3 |

§5.3.1 The **first** source `PID-3` repetition is mapped only when its
`CX.1` is non-empty **and** its `CX.5` is `NI` (case-insensitive). It
produces a repetition with `CX.1` = the source `CX.1`, `CX.4.1` = `NHS`,
`CX.5` = `NH`.

§5.3.2 The **second** source `PID-3` repetition is mapped only when its
`CX.1` is non-empty **and** its `CX.5` is `PI`, compared case-insensitively
after trimming — the same rule as §5.3.1, and the same in the Python, which
applies `.strip().upper()` to both. `CX.1` is trimmed before it is written.
It produces a repetition with `CX.1` = the source `CX.1`, `CX.4.1` = `103`,
`CX.5` = `PI`.

§5.3.2a **The mapped repetitions are packed from the first position.** They
are written in order — first mapped identifier into `PID-3[1]`, second into
`PID-3[2]` — not into the position they came from. A message whose first
source repetition fails its guard therefore has its **second** identifier
land in `PID-3[1]`.

That is the Python's behaviour (`add_field` appends) and it is load-bearing:
MPI reads `PID-3[1]`, and leaving an empty repetition there would put the
patient's only identifier somewhere MPI does not look. The `a08` fixture in
`pims_messages.py` is exactly this case — its first repetition has an empty
`CX.1` — which is why it is in the corpus.

§5.3.3 Death date: when `PID-29.1` has **more than 6 characters**, it is
timezone-stripped per §5.6; otherwise it is set to `""` — HL7's explicit
null, two literal double-quote characters. Note the comparison is on length,
not on validity, and it is applied unconditionally, so a message with no
`PID-29` still gets an explicit null.

### §5.4 `PD1`

Skipped when the source has no `PD1`. Applied **only** for `A04` and `A08`
source triggers (§5.8).

| Destination | Source | Rule |
| --- | --- | --- |
| `PD1-4.1` | `PD1-4.1` | copy |
| `PD1-3.3` | `PD1-4[1].1` | set, only when the source `PD1-4` has more than one repetition |

### §5.5 `PV1`

Applied only for `A04` and `A08` source triggers.

| Destination | Source | Rule |
| --- | --- | --- |
| `PV1-2` | — | literal `N` |

### §5.6 Timezone stripping

`utils/remove_timezone_from_datetime.py`.

1. Blank or whitespace input returns `""`.
2. Take everything before the first `+`, then trim.
3. The result MUST be all digits and MUST be exactly 8 or 14 characters.
   Anything else **fails** the transform with

   ```
   Invalid datetime format after timezone removal: '{value}'. Expected format: YYYYMMDD (8 digits) or YYYYMMDDHHMMSS (14 digits)
   ```

§5.6.1 Only `+` offsets are handled. A `-` offset is not stripped and will
fail step 3. This matches the Python and the PIMS feed, which only ever
sends `+`.

### §5.7 `MRG`

Skipped when the source has no `MRG`. Applied **only** when the source
`MSH-9.2` is exactly `A40`.

| Destination | Source | Rule |
| --- | --- | --- |
| `MRG-1.1` | `MRG-1.1` | copy |
| `MRG-1.4.1` | — | literal `103` |
| `MRG-1.5` | — | literal `PI` |

### §5.8 Non-specific segments

Copied verbatim when present, in this order — note `PV1` is **absent** from
this list, unlike ChemoCare, because §5.5 owns it:

```
PV2  OBX  AL1  DG1  PR1  GT1  IN1  IN2  IN3
```

Failures are logged at warn and ignored, as §4.6.1.

### §5.9 Trigger predicate

`is_a04_or_a08_trigger_event` reads the source `MSH-9.2` and returns true
for exactly `A04` and `A08`. An unreadable `MSH-9.2` returns false. This is
a **case-sensitive** comparison on the raw value, unlike §5.1.1 which
upper-cases first; the difference is preserved.

### §5.10 Reference-data enrichment

Reference implementation: `hl7_pims_transformer/clients/reference_data_client.py`
and `mappers/pid_mapper.py` (`c1d2a74`, `04b30e7`).

Four `PID` fields carry source codes the eMPI does not accept, so each is
replaced by the code the `pims_to_mpi_codes` table of the lookup service
([`20-lookup-service.md`](20-lookup-service.md)) holds for it. The table's key
is the pair `code_type`, `pims_code`.

| Target | Source | `code_type` | Target code must match |
| --- | --- | --- | --- |
| `PID-8` | `PID-8` | `gender` | `^[A-Za-z0-9]{1,3}$` |
| `PID-16.1` | `PID-16.1` | `marital-status` | `^[A-Za-z0-9]{1,4}$` |
| `PID-22.1` | `PID-22.1` | `ethnic-group` | `^[A-Za-z0-9]{1,4}$` |
| `PID-32` | `PID-3[n].2` of the repetition whose `CX.5` is `NI` (trimmed, case-insensitive; the first such) | `nhs-status` | `^[0-9]{1,2}$` |

§5.10.1 A field is enriched only when its source, trimmed, is neither empty
nor the HL7 null (`""`). Otherwise the target is left unset, except that
`PID-8` is copied from the source by §5.3 first and stays as the copy.

§5.10.2 **A blank target code means "leave the field unset".** The lookup
returns `""` for it and the pattern is not applied to `""`. For `PID-16.1`,
`PID-22.1` and `PID-32` nothing is written. For `PID-8` the empty value **is
written**, because §5.3 has already copied the untranslated PIMS code and
skipping the write would send it to the eMPI. (`c1d2a74`'s fix in
`04b30e7`: gender `I` maps to `U`, but a deliberately blank gender row must
clear the copy.)

§5.10.3 **A lookup that fails fails the transform**, so §1.7 holds the
message and everything behind it on the session. The error is a
`TransformError::Field` at the `PID` field, whose text names the **dataset**
and the failure and carries **neither the source code nor the target code**
(deviation: upstream interpolates both; the source of `PID-16`/`PID-22` is
free text from a clinical message, `index.md` §0.8). These are the failures:

1. the base URL is unset (`REFERENCE_DATA_API_BASE_URL`);
2. the request fails, or every retry does;
3. `404`: no mapping;
4. any other non-`200` status;
5. a body that is not JSON, or has no string `value`;
6. a non-blank target code that does not match the pattern above.

§5.10.4 **The request** is
`GET {base}/api/v1/lookup/pims_to_mpi_codes?key.code_type=<code_type>&key.pims_code=<code>`
with both values percent-encoded (everything except `A-Za-z0-9-_.~`), sending
`Accept: application/json` and, when `REFERENCE_DATA_API_KEY` is set,
`X-API-Key`. The response `value` is trimmed. The base URL has trailing `/`
removed. `http` and `https` are both accepted; `https` verifies against
`ihb_tls`'s roots (`18-deployment.md` §4).

§5.10.5 **Known gap, shared with the Python.** `X-API-Key` is not a credential
the lookup service accepts (`20-lookup-service.md` §12: bearer or Easy Auth
only), so with lookup auth on this client is refused. Reproduced, not
introduced; see
[`docs/upstream/pims-lookup-auth-mismatch.md`](../docs/upstream/pims-lookup-auth-mismatch.md).

§5.10.5 **Tuning**, all optional: `REFERENCE_DATA_API_TIMEOUT_SECONDS`
(default `5`, a positive number), `REFERENCE_DATA_API_MAX_RETRIES` (default
`3`, a whole number >= 0) and `REFERENCE_DATA_API_RETRY_BACKOFF_SECONDS`
(default `0.5`, a number >= 0). A connection or read failure, or a status of
`429`, `502`, `503` or `504`, is retried up to `MAX_RETRIES` times, sleeping
`backoff * 2^(n-1)` before the n-th retry for n >= 2 and not at all before the
first (`urllib3` 2.x `Retry.get_backoff_time`). A value that is set and
unparseable is a startup configuration error, reported with the others
(`02-configuration.md` §1.4).

§5.10.6 **The call blocks the worker.** `Transformer::transform` is
synchronous and a session is processed one message at a time, so the client is
synchronous too (a `std` socket, with a deadline on connect, read and write).
On a multi-threaded runtime it runs inside `block_in_place`, so the session
lock renewal and the health check are not starved for the length of a retry
sequence.

§5.10.7 **Deviation.** Upstream's `urllib3` pool reuses connections; this
client opens one per lookup, because at most four lookups happen per message
and a message per session is the ceiling. Redirects are not followed.

### §5.11 Egress properties

§5.11.1 PIMS overrides §1.3.3: the outgoing properties are the incoming ones
with `MessageType` set to the **transformed** `MSH-9.2` (`A28`, `A31` or
`A40`). The incoming `MessageType` is the source trigger (`A04`/`A08`), and
the transformed topic's subscriptions filter on the normalised one
(§5.1.1). Every other incoming property is carried through, and the incoming
map is not modified. With no incoming properties the result is
`{MessageType: ...}` alone.

### §5.12 Topics

§5.12.1 The PIMS flow is on topics end to end in Compose (`04b30e7`): the
server publishes `local-inthub-pims-hl7-input`; the transformer reads it
through `local-inthub-pims-transformer-sub`, publishes
`local-inthub-pims-hl7-transformed` (session `pims-to-mpi`); and a dedicated
`hl7-sender` reads that through `local-inthub-pims-mpi-sender-sub`. The
transformer's topic ingress and egress are `02-configuration.md` §5.2's; no
code is specific to PIMS. The server's `pims` flow gets the standard routing
set (`07-hl7-server.md` §6.2), whose `MessageType` is the source `MSH-9.2`,
which is all upstream's `build_pims_properties` adds, so nothing in
`hl7-server` changes.

---

## §6 Tests

§6.1 Unit: every row of §3, §4 and §5, driven from a minimal message that
isolates the field under test.

§6.2 Unit: §3.4, §3.6 and §5.6, including every failure case and the exact
error text.

§6.3 Unit: §4.3.2's health-board table, including the unmapped case leaving
the field unset.

§6.4 Unit: §5.1.1's trigger table, including the catch-all.

§6.5 Unit: §5.3.1 and §5.3.2's guard conditions — right `CX.5`, wrong
`CX.5`, empty `CX.1`, missing repetition.

§6.6 Golden: every message in `hl7_chemo_transformer/tests/chemo_messages.py`
and `hl7_pims_transformer/tests/pims_messages.py`, plus each flow's file in
`local/sample_messages/`, transformed and compared byte for byte against the
Python's output.

§6.6a Unit: every row of §5.10 and §5.11: a blank target leaves the field
unset and clears `PID-8`'s copy, each failure of §5.10.3, the request URL and
encoding of §5.10.4, the retry and status handling of §5.10.5 against a local
listener, and `MessageType` on the way out.

§6.7 Integration: the runtime of §1, asserting that the outgoing message
carries the incoming application properties unchanged (§1.3.1), and that a
transform failure abandons rather than completes (§1.7).

---

## §7 WDS → WIS (`hl7-wds-transformer`)

Reference implementation:
`Integration-Hub-Beta/transformers/hl7_wds_transformer/` (upstream, after
`9e797ac`). Implemented by `crates/hl7-wds-transformer`.

§7.1 The transformer is a **filter**, not a mapping. `MSH-9.2` of the
**flat** re-parse (§5.0's reason applies: group detection hides `MSH-9`
components from some structures) is trimmed and compared **case-sensitively**
with `A28` and `A31`.

§7.2 A message whose trigger is one of those two is returned **unchanged**:
the output is the re-parsed message's ER7. It stays ER7 because the
downstream `soap-sender` (`envelope_format = wis`) converts ER7 to v2 XML
and wraps it; converting here too would do the conversion twice.

§7.3 Any other trigger, including an empty one, is a transform error whose
text is, byte for byte,

```
WDS transformer: unsupported trigger event '{trigger}'. Allowed: A28, A31. Message will be dead-lettered.
```

The message is abandoned under §1.6 and §1.7. The Python's wording says
"dead-lettered"; here the runtime abandons and the bus's delivery count does
the dead-lettering (`03-message-bus.md` §5.2.2). The text is kept because
operators grep for it. The trigger is a code, not patient data, and is the
one value this transformer ever puts in a message.

§7.4 Name `WDS` (audit text, §1.4 and §1.5). Configuration is
[`02-configuration.md`](02-configuration.md) §5 and nothing else.

§7.5 Tests: the Python's three cases (A28 and A31 pass, A08 fails with the
trigger and `dead-lettered` in the text) plus the empty trigger, a lower-case
`a28` (rejected: §7.1 is case-sensitive), and output equal to the re-parsed
input.

---

## §8 Core reference (`hl7-core-reference-transformer`)

Reference implementation:
`Integration-Hub-Beta/transformers/hl7_core_reference_transformer/`
(RISP → MPI). Implemented by `crates/hl7-core-reference-transformer`, over
`crates/wrds-service` (§9).

§8.1 **Scope filter.** `MSH-9.2` (trimmed) in `{A28, A31, A40}` is mapped;
anything else is returned **unchanged** and WRDS is not called. This is
defence in depth: the ingress queue is expected to carry nothing else.

§8.2 **Six fields, and only six, may change.** Everything else in the
message passes through byte for byte.

| Type string | Path | Replaced |
| --- | --- | --- |
| `Sex` | `PID-8` | the whole field |
| `Title` | `PID-5.5` | `XPN.5` only |
| `Language` | `PID-15.1` | `CE.1` only |
| `Marital Status` | `PID-16.1` | `CE.1` only |
| `Religion` | `PID-17.1` | `CE.1` only |
| `Ethnicity` | `PID-22.1` | `CE.1` only |

§8.3 **`PID` is located in the flat re-parse** (§5.0). A merge trigger
(`A40`) uses `ADT_A39`, which nests `PID` in a `PATIENT` group; the Python's
`.pid` shortcut silently returned an empty proxy for it, and every field read
as empty. Reading the flat message cannot make that mistake. A message with
no `PID` is returned unchanged with a warning; it is not an error.

§8.4 **Empty rules.** A source field that is empty, or is the HL7 explicit
null (`""`), is left exactly as it is and takes no part in the lookup. A
field that *is* looked up but has no matching row, or whose `ToCode` is
empty, is **replaced with the empty value**.

§8.5 **One lookup per message.** If no field qualifies under §8.4, WRDS is
not called. Otherwise exactly one `GetResultSet` (§9) is made, filtered on
`FromSystem = MSH-3.1` and `ToSystem = MSH-5.1`, retrieving `FromCode`,
`type` and `ToCode`, `ExactMatch` true, against `WRDS_LOOKUP_TABLE_NAME`.
Each field is then translated from the rows already fetched.

§8.6 **A WRDS failure is a transform error** (§1.6, row 1): the message is
abandoned and retried. A message is never forwarded with codes that were
meant to be translated and were not. The error text names the failure class
(timeout, HTTP status, fault, unparseable), never a field value.

§8.7 **Configuration**, in addition to
[`02-configuration.md`](02-configuration.md) §5:

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `WRDS_ENDPOINT_URL` | url | — | **Required.** The SOAP POST endpoint. |
| `WRDS_TIMEOUT_SECONDS` | u64 | `30` | Whole-request timeout; positive. |
| `WRDS_CLIENT_CERT_PATH` | path | *(unset)* | One PEM file holding the client certificate chain and its private key, for mTLS. |
| `WRDS_LOOKUP_TABLE_NAME` | string | `FioranoCodeTranslation` | |
| `WRDS_LOCAL_FIXTURE_PATH` | path | *(unset)* | §9.6. Local testing only. |

§8.8 Name `CoreReference`. Tests: the Python's mapper and transformer cases
(all six translated; unrelated fields untouched; one lookup with the right
filters; all-empty skips the call; explicit null untouched; no row clears the
field; no `PID` unchanged; out-of-scope passes through), plus a merge trigger
whose `PID` is nested (§8.3), and a WRDS failure abandoning the message.

---

## §9 `wrds-service`

Reference implementation: `Integration-Hub-Beta/shared_libs/wrds_service/`.
Implemented by `crates/wrds-service`, a library with no binary.

§9.1 A client for the WRDS (Welsh Reference Data Service) `GetResultSet`
SOAP 1.1 operation. It is unrelated to `soap-sender`.

§9.2 **Request.** `POST` to the endpoint with
`Content-Type: text/xml; charset=utf-8`, no `SOAPAction`, a body of
`soap:Envelope/soap:Body/GetResultSetRequest` in
`http://www.wales.nhs.uk/namespaces/MessageRelease2`, holding one
`AttributeValuePair` per filter, a `LookupTable`, one `AttributeToRetrieve`
per requested name, and `ExactMatch` as `1` or `0`. Every name and namespace
sits in `http://www.wales.nhs.uk/nrds`. Text is escaped for `&`, `<` and `>`.

§9.3 **Response.** Any status other than `200` is an error. The body is
parsed with a reader that **refuses a DTD** (no entity expansion, no
external reference; the Python used `defusedxml`). A SOAP `Fault` is an
error carrying its `faultstring`. Otherwise each `Row` of
`GetResultSetResponse` becomes one map from `Attribute/Name` to `Value`
(empty when absent); a pair with no name is skipped.

§9.3.1 **A non-`200` status is never parsed**, so a SOAP fault returned with
status `500` surfaces as the HTTP-status error, not as a fault. This is the
Python's behaviour (`wrds_service.py`: the status check raises
`WRDSServiceError("... returned HTTP n")` before the body is read, and the
fault branch is reachable only from a `200`). Verified against upstream
2026-11-04; `wrds-service` matches, and a change here would be a deviation.

§9.4 **Translation is client-side.** `to_code(rows, from_code, type)` returns
the `ToCode` of the **first** row whose `FromCode` equals `from_code` and
whose `type` equals `type`, comparing attribute **names** case-insensitively
(WRDS returns the requested `type` back as `Type`) and values exactly. No
match, or an empty `ToCode`, is the empty string.

§9.5 **Errors** are one type with a class (timeout, connection, HTTP status,
fault, malformed, fixture). None carries a value from a request or a row.
HTTPS uses the workspace trust roots (`ihb-tls`) and, when a client
certificate is configured, presents it. Plain `http` is allowed, for tests.

§9.6 **Fixture mode.** With a fixture path set, `get_result_set` reads that
file as a response body (§9.3) and makes no network call; the filters are
not applied. An unreadable file is an error.

§9.7 Tests: the Python's service tests (envelope contents, escaping, row
parsing, fault, non-200, timeout, DTD refused, case-insensitive `to_code`,
fixture mode), plus a round trip against a local listener.

---

## §10 LIMS WRRS classifier (`hl7-lims-wrrs-transformer`)

Reference implementation:
`Integration-Hub-Beta/transformers/hl7_lims_wrrs_transformer/` (upstream
`#418`, `#429`, `#431`, after `9e797ac`). Implemented by
`crates/hl7-lims-wrrs-transformer`.

§10.1 **It classifies; it does not transform.** An inbound LIMS `A31`/`A40`
is published **unchanged** once for every rule it satisfies, to a topic
whose subscriptions filter on the application property `MappingType`. It is
therefore *not* a [`Transformer`](#1-the-runtime): §1 is strictly one message
in and one message out, and this service is one in and N out. It has its own
receive loop and reuses only the configuration of
[`02-configuration.md`](02-configuration.md) §5 (topic ingress, topic
egress).

§10.2 **Rules.** The trigger event is `MSH-9.2` (trimmed), **not** `MSH-9.3`.

| `MappingType` (default) | Rule |
| --- | --- |
| `LIMS_NHS_ID_UPDATE` | `MSH-9.2` is `A31` or `A40`, and **one** `PID` has `PID-3` identifiers whose assigning authority is both `NHS` and `328` (§10.3) |
| `ADT_A40` | `MSH-9.2 = A40`. `MRG` is not required here; a missing one is the mapping stage's to refuse |

Rules are evaluated in that order and the result keeps it, so an `A40` with
both authorities yields `[LIMS_NHS_ID_UPDATE, ADT_A40]`. An empty result is
an unsupported message (§10.5).

§10.3 **Authority codes.** Every `PID` segment is considered, wherever it
sits (`ADT_A39` nests it in a patient group). Both authorities must appear
within the **same** `PID`: `NHS` on one patient and `328` on the other does
not satisfy the rule. Within a `PID`, each `PID-3` repetition contributes
the first subcomponent of every component **after the first** (`CX.1` is
the identifier, never an authority, so an identifier value of `328` does not
count). Components rather than `CX.4` alone, because the REST ingress
publishes `xml_to_er7` output, which drops empty components:
`1^^^NHS^NH` arrives as `1^NHS^NH`. Matching is exact, never a substring.
The accepted trade-off of the compacted layout is that an `NHS` or `328`
sitting in another `CX` component (type, facility) is also accepted.

§10.4 **Output.** Per matching rule, one message:

- body: the inbound body, byte for byte;
- session: `EGRESS_SESSION_ID`;
- application properties: **every inbound property**, then `MappingType`,
  `ProcessingId` (a new UUID per path) and `OriginalMessageId` (the inbound
  Service Bus `message_id`, else `MSH-10`);
- Service Bus `message_id`: `{OriginalMessageId}-{MappingType}`. Used only
  for duplicate detection. Two paths of one message must not share an id, or
  the broker drops the second.

§10.5 **Dead letters.** The message is dead-lettered (not abandoned: it
can never succeed, and retrying burns the delivery count) and the batch
continues:

| Case | Reason | Description |
| --- | --- | --- |
| Body not valid UTF-8, or not parseable as ER7 | `Invalid HL7 message` | message id and correlation id, no payload |
| No rule matched | `Unsupported message type` | trigger event, message id, correlation id |

Both also raise `MESSAGE_FAILED`. The trigger event is a code, not patient
data; no other message content appears in a description or a log line.

§10.6 **Partial failure.** Each path is sent independently, so a failed
send does not stop the others. If **any** path failed, the inbound message is
abandoned (with everything behind it, §1.7) and redelivered. Because §10.4's
ids are deterministic, a path already published is dropped on redelivery,
**provided the classified topic has duplicate detection on**. The local
emulator caps its history window at `PT5M` (`#431`: a longer window crashed
the emulator on start-up).

§10.7 **Configuration**: [`02-configuration.md`](02-configuration.md) §5,
with a topic ingress and `EGRESS_TOPIC_NAME` **both required**, plus

| Variable | Default | Meaning |
| --- | --- | --- |
| `MAPPING_TYPE_NHS_ID_UPDATE` | `LIMS_NHS_ID_UPDATE` | Must match the subscription filter |
| `MAPPING_TYPE_A40` | `ADT_A40` | Must match the subscription filter |

Blank is the default. Equal values are a startup failure. `MAX_BATCH_SIZE`
defaults to **1** here (the Python's `config.ini`), not 100: each inbound
message may fan out to several sends, and one message per receive keeps a
partial failure's blast radius to one message.

§10.8 Tests: every classifier case of the Python (`A31` both authorities;
`A40` both rules in order; `A40` without NHS; `A40` with no `MRG`; `A31`
missing either authority; `A04` with both; no `PID`; authorities split
across patient groups; custom `MappingType` values), the compacted
`xml_to_er7` layout for each, an identifier value of `328` or `NHS` under
another authority, one repetition per authority; the processor cases
(single path, two paths with distinct `ProcessingId` and ids, inbound
properties preserved, `MSH-10` fallback, unsupported, unparseable, invalid
UTF-8, one failed path not stopping the other and the message being
abandoned, a retry reusing the same ids); the configuration cases.
