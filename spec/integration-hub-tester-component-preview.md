# Integration Hub tester — component preview

A developer tool that runs one message through the logic of one Integration
Hub component, with no Service Bus, no MLLP port and no Azure, and shows what
that component would produce. Implemented by `crates/integration-hub-tester`.

Reference implementation: `Integration-Hub-Beta/dev_tools/integration_hub_tester/`
(the "Component Tester", a Tk desktop GUI; last touched upstream by
`0ae06f4`, last functional change `c1d2a74`).

---

## §1 Scope

§1.1 The tester is a **developer tool**. It is not built into any production
image, has no pipeline under `pipeline-ado/`, and is not in
`local/docker-compose.yml`. `cargo xtask verify-pipelines` MUST NOT require
one for it. (The Python README says the same: "not tracked by build/deploy
pipelines".)

§1.2 Input MUST be synthetic. The tester applies no redaction, because its
purpose is to show a body, but it keeps no copy: it writes no file unless
`--output` names one (§6.4), has no history, and reads nothing from the
environment but `SOAP_MOCK_URL` (§4.11).

§1.3 **No telemetry, no `tracing` subscriber.** The components the tester
drives log through `tracing`; the tester installs no subscriber, so those
lines go nowhere. A result is printed on the invoking terminal only. This is
how "patient data never reaches a log" is kept for a tool whose job is to
display a message.

§1.4 **Nothing leaves the machine** except §4.11's one HTTP `POST`, and that
only to the URL the developer set (default `http://localhost:8080/soap`).
No Service Bus connection is opened: every outbound sink the production
code needs is the in-memory `ihb_bus::FakeBus`.

---

## §2 The plugin contract

§2.1 Every previewed component is a **plugin** with: an `id` (the
command-line name), a tab label, a one-line description, an input label, an
output label, a list of named synthetic samples, and a `run(input)` that
returns an **outcome**.

§2.2 An outcome is `{ output, summary, ok }`. `output` is the text shown;
`summary` is one line; `ok` says whether the component accepted the input.
The Python derived `ok` from a leading check mark in the summary string; the
Rust carries it as a field, and the summary is prefixed `ok:` or `failed:`.

§2.3 `run` returns `Err(reason)` only when the tester itself could not do the
job (an input the component cannot even be asked about). A component that
*rejects* the input is a successful run with `ok == false`: the rejection is
the thing being previewed.

§2.4 Input normalisation is shared: surrounding whitespace is trimmed and
every line ending (`\r\n` or `\n`) becomes the HL7 segment separator `\r`.
Output ER7 is displayed with `\r` shown as `\n`.

§2.5 Plugin ids, categories and order (the Python's `PLUGIN_CATEGORIES`):

| Category | `id` | Previews |
| --- | --- | --- |
| Transformers | `phw` | `hl7-phw-transformer` (`09` §3) |
| | `chemo` | `hl7-chemo-transformer` (`09` §4) |
| | `pims` | `hl7-pims-transformer` (`09` §5) |
| | `core-reference` | `hl7-core-reference-transformer` (`09` §8) |
| | `proms` | `xml-fhir-proms-transformer` (`16`) |
| Servers | `hl7-server` | `hl7-server` validation and ACK/NACK (`07`) |
| | `rest-server` | `hl7-rest-server` HL7 pipeline (`19`) |
| | `hl7-soap-server` | `hl7-soap-server` (`08`) |
| Senders | `hl7-sender` | MLLP framing (`10` §3) |
| | `hl7-sender-ack` | acknowledgement classification (`10` §3.7) |
| | `soap-sender` | SOAP envelope and post (`10` §4) |
| | `soap-subscription-sender` | as `soap-sender`, subscription path |

---

## §3 Rule: call the real code

§3.1 A plugin MUST call the component's own library function. It MUST NOT
carry a private copy of the component's logic, because a preview that
disagrees with the component is worse than none. Where the Python copied
code into the plugin (`soap_sender_plugin.py` re-implemented the envelope
and the response check), the Rust calls `soap_sender::envelope::{wrap,
evaluate}`.

§3.2 Only the outbound I/O boundary is replaced: Service Bus senders and the
message store become `FakeBus` sinks (§1.4), and the WRDS lookup of
`core-reference` becomes an in-memory table (§4.4).

---

## §4 Plugins

### §4.1 `phw`, `chemo`

Parse the input, run `Phw` / `Chemo`, print the transformed ER7.
`phw` summary is `ok: ` followed by `Phw::processed_audit_text`
(`09` §3.5). `chemo` summary is `ok: Chemocare transformation applied`.

### §4.2 `pims`

Parse, run `Pims`, print the ER7. **Deviation:** the Python built a
`ReferenceDataLookupClient` from `REFERENCE_DATA_API_BASE_URL`; the Rust
PIMS transformer has no lookup (`09` §5), so none is built and no
environment is read.

### §4.3 A transform failure

For §4.1 to §4.4, a parse failure or a `TransformError` is `Err(reason)`
(§2.3): the component could not produce a message.

### §4.4 `core-reference`

An input whose trigger (`MSH-9.2`) is not in `SUPPORTED_TRIGGERS` is
returned **unchanged** with the summary
`ok: <trigger> is out of scope - passed through unchanged`. Otherwise
`CoreReference` runs over a **demo table** (nine rows: Sex, Title, Language,
Marital Status, Religion, Ethnicity) held in memory. The summary says the
values are demo data, because a `ToCode` shown here is not a real WRDS
answer. No file is written (the Python wrote a temporary SOAP fixture; the
Rust implements `Rows` directly).

### §4.5 `proms`

`Proms::transform_payload`, pretty-printed (two-space indent, UTF-8 not
escaped). Summary: `ok: N entries: Type, Type, ...` from each entry's
`resourceType`. The four `TEMP` samples are ported (`SURGERY-PERF`, `SURGERY-DN`,
`PREOP-VIS`, `PREOP-AR`; upstream `8a7f6f3`), named as upstream names them,
with a `TEMP` suffix. Like upstream's, their field values are placeholders
pending real WPAS payloads; the NHS numbers are replaced from the test range
(§5.1). The table has ten samples.

### §4.6 `hl7-server`

In order, as the real pipeline (`07` §5):

1. Parse. On failure: `PARSE FAILURE`, then the generic `AE` NACK
   (`07` §4.3.2). `ok == false`.
2. A parsed-message summary (type, version, sending app and facility,
   control id, segments).
3. The accepted-type allow-list (`07` §7.1), then `check_fields` with
   `hl7_version = 2.5`, no sending-application list and no flow (the
   Python's generic check). On a `Rejection`: the NACK `build_nack` returns
   for its `ack_code` (`AR` for validation, `AE` otherwise). `ok == false`.
4. On success: the `AA` from `ack::build`. `ok == true`.

### §4.7 `rest-server`

Shows the JSON body a caller would `POST` (`{"messageContent": ...}`), runs
the input through `input::to_er7` and `Processor::process` with no flow
configured (`19` §3), and prints the status and body: `201` with the ACK,
or the `Failure` (`422` validation, `500` anything else) with its NACK. An
input that does not convert is a `500` with the generic NACK (`19` §2.2).

### §4.8 `hl7-soap-server`

Fixed configuration (the PHW recipe): schema group `phw`, structures
`ADT_A05`/`ADT_A39`, assigning authority `328`. An input starting `<` is
sent as given; otherwise the ER7 is converted to v2.xml for the `phw` flow
and wrapped in a SOAP envelope. The real `Processor::process` runs. Output:
the request, then `HTTP 200` with the success envelope or the fault's own
status (`400`, `403`, `500`) with its envelope (`08` §3.1).

### §4.9 `hl7-sender`

Never connects. Prints a message summary, the MLLP frame breakdown (start
block `0x0B`, payload, end block `0x1C`, carriage return `0x0D`; total byte
count), the payload one segment per line, and the control bytes in hex.
The input must parse.

### §4.10 `hl7-sender-ack`

`hl7_sender::ack::evaluate`. Output: outcome (`SUCCESS`, `AE`, `AR` or
`INVALID`), `MSA-1`, the reply's `MSH-10`, and what the sender would do
(`10` §3.7.1). `ok` is true only for `SUCCESS`.

### §4.11 `soap-sender`, `soap-subscription-sender`

Print the message summary and the envelope from `soap_sender::envelope::wrap`,
then `POST` it (`Content-Type: text/xml; charset=utf-8`, ten second timeout)
to `SOAP_MOCK_URL`, default `http://localhost:8080/soap`, and judge the
response with `soap_sender::envelope::evaluate` (`10` §4.4). A connection
failure is reported with a hint to start the mock receiver (`mock soap`,
§7) and is `ok == false`. The two ids differ only in the banner: the
Python kept two files to show the two senders produce one envelope, and the
Rust keeps two ids for the same reason, over one implementation.

§4.11.1 Only an `http` URL is attempted. Any other scheme (`file://`,
`ftp://`, none) is refused before a socket is opened with
`Unsupported URL scheme (only http/https allowed): {url}`, and `https` is
refused separately because the client speaks plain HTTP/1.1. This follows
upstream's Bandit B310 fix to `soap_sender_plugin._post_to_mock` and its
subscription twin (after `9e797ac`), which validate the scheme before
`urlopen`; the URL is operator-typed, not patient data.

---

## §5 Samples

§5.1.1 The `REFERRAL` PROMs sample carries the fields the revised mapping of
`16-proms-transformer.md` §6.3 and §6.11 reads (`home_phone`, `work_phone`,
`mobile_phone`, `email`, `ethnic_origin_code`, `ethnic_origin`,
`referrer_prac`) and `hbCode` `7A2` (upstream #423, `proms_plugin.py`), all
invented.

§5.1 Samples are synthetic: names such as `TEST^TEST`, identifiers in the
`1000000001` and `999` ranges, no real address or number. Where the Python's
samples carried a plausible NHS number, the Rust ones carry a number from the
NHS test range.

§5.2 Every sample of every plugin MUST run to completion (`run` is `Ok`), and
its `ok` MUST equal what its name promises (a sample named `... NACK` is
`ok == false`). A test enforces both for the whole table.

---

## §6 Command line

§6.1 `integration-hub-tester list` prints categories and plugin ids with
their descriptions.

§6.2 `integration-hub-tester samples <id>` lists a plugin's samples.

§6.3 `integration-hub-tester run <id> [--sample NAME | --file PATH]` reads
the input from the sample, the file, or standard input, in that order of
precedence. `NAME` matches a sample case-insensitively, exactly or as a
unique substring.

§6.4 `--output PATH` also writes the output text to `PATH`. Without it,
nothing is written to disk (§1.2).

§6.5 The output goes to standard output and the summary to standard error.
The exit status is `0` when `ok`, `1` when the component rejected the input
or the tester could not run, and `2` for a usage error.

§6.6 **Not ported: the Tk window** (sidebar, tab strip, theme toggle,
`Ctrl+Return`, character count, undo). A terminal has no counterpart and the
logic it fronted is fully reachable from §6.3. A headless tool also makes
every plugin testable, which the Python's GUI was not: it has no tests.

---

## §7 Mock receiver launcher

§7.1 `integration-hub-tester mock <mllp|soap>` starts `hl7-mock-receiver`
or `http-mock-receiver` and waits for it, so §4.11 has something to post to.
It looks for the binary **beside its own executable**, then on `PATH`'s
usual lookup; if neither exists it fails with the `cargo build -p ...`
command to run. It does not open a terminal window (the Python did, with
`CREATE_NEW_CONSOLE` or a terminal emulator) and so has nothing to track or
terminate: `Ctrl+C` stops the tester and its child together.

§7.2 An unknown mode is a usage error naming the two valid ones.

---

## §8 Tests

§8.1 There are no upstream tests for the Python tester (`find` finds none),
so the Rust tests are written from this specification: one per plugin (§4),
the sample invariant (§5.2), the command line (§6) and the launcher (§7).
The §4.11 test posts to a throwaway local listener, not to a real receiver.
