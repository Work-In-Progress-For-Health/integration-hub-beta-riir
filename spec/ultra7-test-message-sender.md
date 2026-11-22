# Ultra7 — test message sender

A developer tool that holds a named set of synthetic test messages and an
endpoint, and sends them over MLLP, REST or SOAP, optionally many times with
one field changing on each send. Implemented by `crates/ultra7`.

Reference implementation: `Integration-Hub-Beta/dev_tools/ultra7/` (a Tk
desktop GUI; last touched by upstream `2f86e4b`).

---

## §1 Scope

§1.1 Ultra7 is a **developer tool**. It is not built into any production
image, has no pipeline under `pipeline-ado/`, and is not in
`local/docker-compose.yml`. `cargo xtask verify-pipelines` MUST NOT require
one for it.

§1.2 Messages sent with it MUST be synthetic. Ultra7 applies no redaction,
because it exists to put a body on the wire, and it stores bodies in plain
JSON under the user's home directory. No fixture, doc example or sample in
this repository may be real patient data.

§1.3 **No telemetry, no `tracing`.** The crate emits nothing to a log sink.
A send result is printed on the invoking terminal only (§9.3). This is how
the "patient data never reaches a log" rule is kept for a tool whose
purpose is to show a body.

---

## §2 What is and is not ported

§2.1 **Ported** (all headless logic of the Python): the project data model
and its JSON file format (§3), the iteration field (§4), message format
detection and pretty-printing (§5), file export naming (§6), the three
senders (§7) and the repeat-send loop (§8).

§2.2 **Not ported: the Tk GUI.** The window, sidebar, editor pane with
highlighting and tracked iteration range, log panel, endpoint dialog,
iteration dialog, themes and `settings.json` (theme name only) have no
counterpart. A terminal binary (§9) replaces them; a project file is the
editing surface, and an iteration range is given as character offsets in it.
The Python tests for those widgets (`test_editor_pane`, `test_log_panel`,
`test_send_controls` widget parts, `test_themes`, `test_settings`) are
therefore not ported. The one non-widget behaviour in `send_controls`,
the repeat/delay clamping and the send loop, is §8.

---

## §3 Project files

§3.1 A project is one JSON file `<sanitised name>.json` in the projects
directory, default `~/.ultra7/projects/`. The format is the Python's
`Project.to_dict()` so that a project file works in both tools:
`name`, `endpoint`, `messages`, `repeat_count`, `delay_ms`.

§3.2 Reading is lenient and writing is complete, as the Python. Missing
`endpoint` fields default to: `kind` `mllp`, empty `host`/`url`/`soap_action`,
no `port`, no headers, `timeout_seconds` 5.0. A missing message `id` is
generated (8 hex characters of a v4 UUID), `enabled` defaults to `true`,
`iteration` to none. Iteration defaults: `mode` `increment`, `step` 1,
`pad_width` 0, empty `values`, `timestamp_format` `%Y%m%d%H%M%S`.

§3.3 `project_filename` replaces every character outside `A-Za-z0-9 _-` with
`_`, trims whitespace, and fails on an empty result. A name therefore cannot
contain a path separator.

§3.4 Saving is atomic: write `<file>.json.tmp`, then rename. Listing returns
names sorted alphabetically. Deleting a missing project is not an error.

---

## §4 Iteration field

§4.1 An iteration spec names a range of a message's content by **character**
offsets (`start` inclusive, `end` exclusive) and a mode. Offsets are
characters, not bytes, as the Python's, so a project file means the same in
both tools.

§4.2 `apply_iteration(content, spec, index)` (`index` 0-based) returns
`content` unchanged when the range is not `0 <= start < end <= len`.
(A stale range in a hand-edited project file must not stop a run; this is a
fallback for invalid input, `rust-checked-arithmetic` RCA-DECL-6, kept because
Ultra7 is a tool for sending broken messages on purpose. **Raised for a
decision**; see `docs/checked-arithmetic-audit.md`.) The `increment` arithmetic
is checked in `i128`: a result that does not fit leaves the original unchanged,
as a non-integer original does, and is never saturated.
Otherwise it replaces the range:

- `increment`: parse the original as an integer (surrounding whitespace
  allowed); not an integer leaves it unchanged. The value is
  `base + step * index`; a non-negative value is zero-padded to `pad_width`,
  or to the original's length when `pad_width` is 0; a negative value is not
  padded.
- `list`: `values[index % len]`; an empty list leaves it unchanged.
- `timestamp`: the current local time rendered with `timestamp_format`, a
  `strftime` subset (§4.3).

§4.3 The `strftime` subset is `%Y %y %m %d %H %M %S %f %j %%`; any other
`%x` is copied verbatim. `%f` is microseconds, six digits. Local time falls
back to UTC when the offset cannot be determined.

---

## §5 Format detection and pretty-printing

§5.1 `detect_format`: empty or blank is `hl7`; content starting `{` or `[`
that parses as JSON is `json`; content starting `<` that parses as XML is
`xml`; everything else is `hl7`.

§5.2 `pretty_print` re-indents `json` and `xml` with two spaces and fails
with a message for `hl7`, or for content that does not parse. Blank lines are
dropped from XML output.

§5.3 **Deviation.** JSON object keys come out in sorted order, not source
order, because `serde_json` is built without `preserve_order`
(`spec/serde-json-float-roundtrip-arbitrary-precision/index.md`). Numbers
round-trip exactly. XML comments and processing instructions are kept;
the XML declaration is not re-emitted.

---

## §6 Export

§6.1 Extension by format: `.hl7`, `.xml`, `.json`.

§6.2 `sanitize_filename` replaces characters outside `A-Za-z0-9 _-` with `_`,
trims, and falls back to `message` when empty. `unique_filename` returns
`stem+ext`, or `stem (2)+ext`, `stem (3)+ext` ... when already used.

---

## §7 Senders

§7.1 A send never panics and never returns an `Err` for a transport failure:
every outcome is a `SendResult { ok, latency_ms, response_summary, error }`.

§7.2 **MLLP.** Host and port are required (`Host and port are required`).
Segment separators `\r\n` and `\n` are normalised to `\r` before framing
(`0x0B` body `0x1C` `0x0D`). The reply is read until the end block, EOF or the
timeout. An empty reply is the failure `No ACK received (connection closed)`.
`MSA-1` is read from the first `MSA|` segment; `AA` and `CA` succeed, any
other code fails with `NAK: <code>`, and a reply with no `MSA-1` succeeds.

§7.3 **REST.** A URL is required (`URL is required`) and its scheme MUST be
`http` or `https` (`Unsupported URL scheme: <s>`). The body is POSTed with
`Content-Type` `application/hl7-v2`, `application/xml` or
`application/json` by format, unless the endpoint's headers set one.
Success is a 2xx status (`HTTP <status>: <body>`); another status is
`HTTP <code>` with the body as the summary.

§7.4 **SOAP.** As §7.3, with the body wrapped in a SOAP 1.1 envelope unless
the first 200 characters already contain `Envelope`, `Content-Type`
`text/xml; charset=utf-8`, and `SOAPAction` set from the endpoint when
non-empty. Endpoint headers win over the defaults.

§7.5 **Deviation: no `https`.** The Python uses `urllib`; this crate speaks
HTTP/1.1 over a plain `std` socket so a developer tool does not pull a TLS
stack and HTTP client into the build graph. An `https://` URL passes §7.3's
scheme check and then fails with `https is not supported by this build; use
an http:// endpoint or a local proxy`. Redirects are not followed.

---

## §8 Sending a project

§8.1 `repeat_count` is clamped to at least 1 and `delay_ms` to at least 0.
A batch send sends every **enabled** message, in order, `repeat_count`
times; with none enabled nothing is sent and the outcome is
`No enabled messages to send`.

§8.2 Message `m` on repeat `r` (0-based) is sent with its iteration applied at
index `r`. The announce line is `Sending <name> [<r+1>/<n>] to <destination>`;
destination is `<host> on port <port>` for MLLP, and the URL's host (with
` on port <p>` only when the URL names one) otherwise.

§8.3 Selected-once sends the named messages once each, ignoring repeat,
delay and the enabled flag, at index 0. Naming none is `No message
selected`; naming one that is not in the project is an error.

§8.4 Cancellation is checked before each send. The delay is slept after every
send when non-zero.

---

## §9 Command line

§9.1 `ultra7 [--dir DIR] <command>`; `DIR` defaults to §3.1's. Commands:
`projects`; `send PROJECT [--repeat N] [--delay-ms N] [--only NAME]...`;
`export PROJECT OUT_DIR`; `detect FILE`; `format FILE`.

§9.2 Exit status is 0 on success; 1 when any send failed or a command
failed; 2 on a usage error.

§9.3 Each send prints one line, `<label> OK|ERROR <latency>ms <detail>`,
on stdout (§1.3).

---

## §10 Tests

§10.1 Every Python test is ported by behaviour, in files named for the
Python module (`tests/models.rs`, `iteration.rs`, `detect.rs`,
`export_storage.rs`, `senders.rs`), with Rust-only cases added for
`§3.2` (a Python-written file loads), `§4.1` (character offsets), `§5.3`,
`§7.5` and header injection. Ported: `test_models`, `test_iteration`, `test_detect`, `test_pretty_print`,
`test_export`, `test_storage` and `test_senders` (the last against in-process
`std` servers on `127.0.0.1:0`, no external service). Each cites the clause
it protects.
