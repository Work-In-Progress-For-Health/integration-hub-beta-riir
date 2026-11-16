# 19 — REST ingress servers

Two HTTP ingress servers that the Python repository gained after the Rust
rewrite began (upstream `26 Aug 2026` onward; see
[`docs/upstream-sync-2026-10.md`](../docs/upstream-sync-2026-10.md)).

| Crate | Python | Role |
| --- | --- | --- |
| `crates/hl7-rest-server` | `servers/hl7_rest_server` | RISP (and other) REST → HL7: a JSON body carrying ER7 or v2.xml, answered with a raw HL7 ACK/NACK |
| `crates/rest-server` | `servers/rest_server` | One image, two `PIPELINE`s: `generic` (SOAP or plain XML → Service Bus) and `hl7` (the `hl7-rest-server` pipeline, unchanged) |

Reference implementation: `Integration-Hub-Beta/servers/rest_server/` and
`Integration-Hub-Beta/servers/hl7_rest_server/`.

§0.1 **`hl7-rest-server` is the library the `hl7` pipeline lives in.** Upstream
copied `hl7_rest_server` into `rest_server/hl7/` and then retired the
standalone service; the copy has since gained behaviour the original lacks
(§7.2). Here there is one implementation: `rest-server` depends on the
`hl7_rest_server` library for `PIPELINE=hl7`, and the standalone
`hl7-rest-server` binary is the same pipeline with the standalone service's
own environment contract (§8). A defect in the pipeline is fixed once.

§0.2 Everything in `spec/index.md` §0.8 applies. An HTTP response body, in
particular, MUST NOT carry a field value from the message (§2.5, §10.4).

---

## §1 `hl7-rest-server` endpoints

| Method | Path | Behaviour |
| --- | --- | --- |
| `POST` | `/hl7MessageReceiver` | Accept `{"messageContent": "…"}`, return an ACK/NACK (§2) |
| `GET` | `/hl7MessageReceiver/ping` | Liveness (§1.2) |
| `GET` | `/hl7MessageReceiver/status` | Readiness (§1.3) |
| any | any other path | 404 `{"detail":"Not Found"}` |
| other method on a known path | | 405 `{"detail":"Method Not Allowed"}` |

§1.1 `/docs`, `/redoc` and `/openapi.json` are **not served**. Upstream
generated them from FastAPI and gated them to `ENVIRONMENT` `DEV`/`SIT`. This
workspace has no OpenAPI generator, and a hand-written copy of the contract
would drift from the code. `ENVIRONMENT` is still read, defaulted to `DEV`,
upper-cased, and returned by `/ping`. See §11.

§1.2 `/ping` returns `200` with a JSON object of exactly four keys:
`environment` (the configured value), `revision` (`"0"`), `version` (`"1.0"`)
and `clientIp` (the peer address, without port).

§1.3 `/status` returns `200` with a JSON object of: `application`
(`"HL7 Message Receiver"`), `description` (`Version 1.0.0 API is working,
response time N ms`), `status` (`200`), `timeout` (boolean, `N > 100`),
`lastRequested` and `lastHealthy` (ISO timestamps of the previous request and
of the previous request that did not time out, or `"Never"`), `version`,
`revision`. The timestamps are UTC (`spec/index.md` §0.7); upstream wrote naive
local time. It performs no dependency check; it reports the sender as
configured because the process does not start otherwise.

---

## §2 `POST /hl7MessageReceiver`

| Situation | Status | Content type | Body |
| --- | --- | --- | --- |
| Accepted and forwarded | 201 | `text/plain` | the raw ACK, `MSA\|AA\|…` |
| Validation failure | 422 | `application/json` | `{"StatusCode":422,"ErrorMessage":"<AR NACK>"}` |
| Body not JSON, no `messageContent`, or not a string | 400 | `application/json` | `{"StatusCode":400,"ErrorMessage":"Invalid or missing request body."}` |
| `messageContent` over `MAX_MESSAGE_SIZE_BYTES` | 400 | `application/json` | `…"Message exceeds the maximum allowed size."` |
| Unparseable (XML that does not convert, ER7 that does not parse) | 500 | `application/json` | `{"StatusCode":500,"ErrorMessage":"<AE NACK>"}` |
| Any other failure, including Service Bus | 500 | `application/json` | as above |

§2.1 **The size check is on the UTF-8 byte length of `messageContent`**, after
JSON decoding. The raw body is additionally bounded at six times the limit
plus 1 KiB (JSON can expand a control character sixfold), so that a body
cannot allocate without limit; one over that bound is the same 400.

§2.2 `messageContent` is normalised in this order: `\r\n` → `\r`, then `\n` →
`\r`; then, if the trimmed content starts with `<`, converted from v2.xml to
ER7. A conversion failure is a **500 with a generic AE NACK**, not a 400:
the sender cannot tell which, and the Python answers 500.

§2.3 The `201` is returned **only after every send in §3 step 12 completed**
(`AGENTS.md`: a message is never acknowledged unless it was delivered).

§2.4 The `ErrorMessage` of a 422 is the NACK that §4 builds from the parsed
header; the `ErrorMessage` of a 500 is the generic NACK of §4.2.

§2.5 **A NACK's `MSA-3` carries the failure reason** (as upstream does), and
the reasons in §5 and §6 are upstream's strings. Those that name a value —
`Message sending application '{value}' is not in allowed authority codes.`,
the RISP sending-facility messages — name an MSH header field that the
sender chose and already knows; they are reproduced because operators match
on them. No reason quotes a PID, name, date or identifier.

---

## §3 The HL7 pipeline, in order

`hl7_message_processor.py :: Hl7MessageProcessor.process`.

```
1   MESSAGE_RECEIVED event, content = the ER7
2   messages_received metric +1
3   parse                                     failure: §3.1
4   common validation (§5.1)                  failure: §3.2
5   VALIDATION_SUCCESS "Valid HL7 message - Type: {MSH-9}"
6   tracking metadata (CorrelationId, MessageReceivedAt, WorkflowID, SourceSystem=MSH-3)
7   risp:      resolve routing targets (§6)   failure: §3.2
    otherwise: flow-schema validation + XML   failure: §3.3
8   if no XML yet: er7_to_xml for the store   failure: logged, XML omitted
9   HL7_VALIDATION_STANDARD validation        failure: §3.2
10  flow routing properties (§7)              failure: warn, continue
11  send to the message store                 non-blocking
12  send to Service Bus (every target)        failure: §3.4
13  ACK (§4.1), MESSAGE_PROCESSED event
```

§3.1 **Parse failure** (step 3): `VALIDATION_FAILED` with `HL7 parsing error:
{reason}`, then `MESSAGE_FAILED`; answered 500 with the generic NACK (§4.2).

§3.2 **Validation failure**: both events (`VALIDATION_FAILED`, then
`MESSAGE_FAILED` with `validation_result` `Message rejected - failed
validation`), then 422 with the AR NACK (§4.1).

§3.3 **Flow-schema validation** (step 7, flows other than `mpi` and `risp`)
is `ihb-validation`'s `validate_and_convert_parsed`. On failure the message
is **stored first, with no XML**, then rejected with `XML validation failed
for flow '{flow}': {reason}` (§3.2). On success the XML is the store's
`XmlPayload`. `mpi` and `risp` have no flow schema (`01-architecture.md`
§2.2.1).

§3.4 **A failed send stops everything.** A Service Bus failure on any target
raises: no ACK is built, the response is a 500 with the generic NACK, and the
source system retries. A target already sent to is **not** rolled back
(duplicates are the safe failure; a lost message is not). The reason is
`Failed to send message {MSH-10} to Service Bus: {error}` (as `hl7-server`,
`07-hl7-server.md` §5.3; upstream used the exception's text), and the
failure emits `MESSAGE_FAILED` with `validation_result` `Failed to forward
message`. Upstream emitted no event here.

§3.4.1 `messages_sent` is incremented once per successful target. Upstream's
HL7 pipeline never incremented it (its `generic` pipeline does); a counter
that is zero for a service delivering messages is a dashboard defect, not a
contract.

§3.5 The message store send is non-blocking: a failure is logged and the
pipeline continues (`07-hl7-server.md` §5.2). `MESSAGE_STORE_QUEUE_NAME` is
optional here, unlike `hl7-soap-server`; unset, step 11 is skipped.

§3.6 `HL7_VALIDATION_STANDARD` (step 9) runs after the XML is produced so that
the store still gets a copy of a message that fails it; the reason is
`Standard validation error: {reason}`. Its mechanism is `ihb-validation`'s
`validate_standard_parsed` (`06-validation.md` §6).

---

## §4 Acknowledgements

`hl7_ack_builder.py :: HL7AckBuilder`. The wire format is fixed by the API
contract.

§4.1 **ACK / AR NACK**, from a parsed message. Two segments joined by
**`\r\n`** (not `\r`; this is the Python's `SEGMENT_SEPARATOR` and the REST
caller is not an MLLP peer):

```
MSH|^~\&|DHCW|cymru.nhs.uk|{MSH-3.1}|{MSH-4.1}|{YYYYMMDDHHMMSS}||ACK|{MSH-10}|P|{MSH-12}
MSA|{AA|AR}|{MSH-10}|{text}
```

`MSH-12` falls back to `2.5.1` when empty. `text` is `Message received
successfully.` for `AA`, and the reason for `AR`. An `AR` with an empty
`MSH-10` uses a generated control id.

§4.2 **Generic AE NACK**, when there is no parsed header: `MSH-5` and `MSH-6`
blank, a generated control id, `MSH-12` `2.5.1`, `MSA|AE|{id}|{reason}`.

§4.3 A generated control id is the first 20 hex characters of a UUID v4.

§4.4 **Deviation: `text` is sanitised.** Upstream put the reason into `MSA-3`
verbatim. A reason containing `\r`, `|` or `^` would then corrupt the
segment, so — exactly as `hl7-server` does (`07-hl7-server.md` §4.3.3) —
line breaks become spaces, the HL7 delimiters are escaped, and the text is
capped at 199 characters.

---

## §5 Validation

§5.1 **Common validation**, in this order, first failure wins:

1. `HL7_VERSION`, when set: `MSH-12` must equal it, else `Message has wrong
   version`.
2. `SENDING_APP`, when set **and the flow is not `risp`**: `MSH-3` must be in
   the comma-separated list (each entry trimmed), else `Message sending
   application '{value}' is not in allowed authority codes.`
3. The flow's own rules (§5.2, §5.3), when `HL7_VALIDATION_FLOW` is `mpi` or
   `risp`. **No other flow has field rules here**: upstream applies `phw`'s
   date-of-birth rules in `hl7_server` only, and the REST pipeline does not
   (it relies on the flow schema, §3.3).

§5.2 **`mpi`**, identical to `07-hl7-server.md` §3.3.2: `MSH-9.2` present, one
of `A28`/`A31`/`A40`, and `PID-2.4.1` yields at least one update source.

§5.3 **`risp`**, `custom_validation/risp_validation.py`:

| Rule | Failure text |
| --- | --- |
| `MSH-12` must be `2.5.1` | `Currently this HL7 version '{v}' is unsupported by the HL7 Message Receiver API;` |
| trigger (`MSH-9.2`) in `A28`,`A31`,`A40`: `MSH-3.1` must be `349` | `The sending facility '{f}' is not included in the list of permitted Assigning Authorities. (trigger '{t}');` |
| structure in `ORU_R01`,`OMG_O19`: `MSH-3.1` all digits and `350`–`358` | `Message sending facility '{f}' is not included in the list of permitted Assigning Authorities. ` (trailing space) |
| anything else | `Unsupported message type/trigger '{structure or trigger}'` |

The structure is `MSH-9.3`, falling back to `{MSH-9.1}_{MSH-9.2}` when both
are present, else empty. The trigger is checked **before** the structure.

---

## §6 RISP routing

`risp_routing.py :: RispFlowRouter`. Only when `HL7_VALIDATION_FLOW=risp`.

| Trigger / structure | Targets (in order) |
| --- | --- |
| `A28`, `A31` | `mpi_transformer`: the ER7 as received, to `EGRESS_*` |
| `A40` | `mpi_transformer` (ER7), **then** `wrrs` (v2.xml), to `WRRS_*` |
| `ORU_R01`, `OMG_O19` | `wrrs` only (v2.xml) |

§6.1 Validation (§5.3) runs again inside `resolve_targets`, so the router is
safe to call on its own.

§6.2 The `wrrs` payload is the v2.xml rendering, and **doubles as the message
store's `XmlPayload`** (the first XML target's).

§6.3 Each target is sent with its own copy of the tracking properties, with
`WorkflowID` overridden to that destination's workflow: `WORKFLOW_ID` for
`mpi_transformer`, `WRRS_WORKFLOW_ID` for `wrrs`.

§6.4 **`ORU_R01` / `OMG_O19` are validated against the custom `2_5_1`
schemas** (`06-validation.md` §10.5), failure text `XSD schema validation
failed for '{structure}': {reason}`. **Deviation:** upstream converts, then
validates the XML against the XSD. Here the schema is read as a grammar
(`06-validation.md` §10.2) and the message's segment sequence is matched to
it (§10.3), which checks segment presence, order, cardinality and group
nesting, but **not datatypes or field lengths**. The v2.xml is produced by
`er7_to_xml`. Closing the gap needs the XSD on disk in the container
(`06-validation.md` §9.6); until then this is the weaker check, and it is
recorded in §11.

§6.5 `A40 → WRRS` conversion failure: `Failed to convert '{trigger}' message
to HL7 v2.xml for WRRS: {reason}`, a validation failure (§3.2).

---

## §7 Routing properties

`custom_message_properties.py`. Added after the tracking properties
(`03-message-bus.md` §4), failure to build non-fatal (§3 step 10).

§7.1 `mpi`: the five properties of `03-message-bus.md` §4 (second table), as
`hl7-server`.

§7.2 `risp` (upstream commit `e090e23` neighbourhood; present in
`rest_server/hl7/`, absent from the retired `hl7_rest_server/`):

| Property | Source |
| --- | --- |
| `MessageType` | `MSH-9.2`, the trigger event (`A28`, `R01`, `O19`) |
| `MessageStructure` | the resolved structure (§5.3), e.g. `ORU_R01`, `ADT_A40` |

Combined with `SourceSystem` (`MSH-3`) this lets Service Bus subscriptions on
RISP's topic filter by trigger and source. Both properties ride on **every**
target's message (the MPI-bound copy and the WRRS copy).

§7.2.1 `MessageType` is upstream `b2d0978`; `MessageStructure` is `2b26c7f`.
`MessageStructure` is `MSH-9.3` when present, **even where the trigger would
imply another structure** (an `A40` declaring `ADT_A39` publishes `ADT_A39`),
and `{MSH-9.1}_{MSH-9.2}` when `MSH-9.3` is absent (`ADT^A28` publishes
`ADT_A28`).

§7.2.2 RISP's egress is a **topic** locally and upstream
(`EGRESS_TOPIC_NAME`, `WRRS_TOPIC_NAME`; `01-architecture.md` §2.1.3); the
queue variants of `02-configuration.md` §11 remain valid configuration.

§7.2.3 The `generic` pipeline takes the same egress choice, read by the same
function as `hl7-rest-server` (§0.1): exactly one of `EGRESS_QUEUE_NAME` and
`EGRESS_TOPIC_NAME`, with `02-configuration.md` §3.1's two messages for both
and for neither. LIMS publishes to a topic (`local/rest-server-lims.env`;
`01-architecture.md` §2.1.3a). Tests: `rest-server`
`queue_and_topic_together_are_refused`, `neither_queue_nor_topic_is_refused`,
`a_topic_alone_is_a_topic_egress`.

---

## §8 `hl7-rest-server` configuration

See `02-configuration.md` §11.

---

## §9 `rest-server`, the `generic` pipeline

`message_processor.py`, `asgi_app.py`, `content_adapters/`, `validators/`.

| Method | Path | Behaviour |
| --- | --- | --- |
| `POST` | `ENDPOINT_PATH` (default `/ingest`) | Process (§9.2) |
| `GET` | `/health` | `200` `{"status":"ok"}` |

(`/docs`, `/redoc` and `/openapi.json` are not served; §1.1 applies. Any other
path is `404` `{"detail":"Not Found"}`, and any other method on a known path
is `405` `{"detail":"Method Not Allowed"}`, as FastAPI answers.)
`PIPELINE=hl7` serves §1 instead and nothing in this section.

§9.1 **Admission.** The body is read as a stream and refused as soon as it
passes `MAX_REQUEST_SIZE_BYTES`: `413`, `Request body exceeds configured limit
of {n} bytes.` (fault code `Client`). Then empty body → `400` `Request body is
empty.`; not UTF-8 → `400` `Request body must be UTF-8 encoded.`. All in the
content adapter's response format (§9.4).

§9.2 **Processing**, in order:

```
1   MESSAGE_RECEIVED event, messages_received metric
2   content adapter: extract payload XML, structure id, source identifier,
                     message control id, message type            (§9.4)
3   validator: validate payload XML                                (§9.5)
4   VALIDATION_SUCCESS "Schema validation passed for structure '{id}'"
5   allow-list: if ALLOWED_SOURCE_IDENTIFIERS set and the source identifier
    is not in it: 403 Client.Authorization
6   OUTPUT_FORMAT: er7 = xml_to_er7(payload); raw = payload unchanged
7   tracking metadata; add MessageType when the adapter extracted one (§10)
8   message store (non-blocking): RawPayload = the whole request body,
                                  XmlPayload = the payload XML
9   Service Bus send, message id = control id (omitted when absent)
10  messages_sent metric, MESSAGE_PROCESSED event
11  200 with the adapter's success body
```

§9.3 **Failures.** A request-level failure carries its own status and fault
code: `RequestError(code, message, status)`. A validation failure is always
`400` with code `Client.Validation`. Both emit `VALIDATION_FAILED` and
`MESSAGE_FAILED`. Anything else is `500`, code `Server`, text `Unexpected
server error while processing request.` with `MESSAGE_FAILED`. **Step 9
failing is the 500**, and no success body is built (§2.3 applies).

§9.4 **Content adapters.**

*`soap`* (`text/xml; charset=utf-8`): parse the envelope (any prefix; matched
on local name). Errors, all `Client` / 400: `Malformed SOAP XML request.`,
`SOAP Envelope element is missing.`, `SOAP Body element is missing.`,
`SOAP Body must contain exactly one business payload element.`. The payload
is that element, serialised **with the namespace declarations in scope at that
element** so it parses on its own. Structure id: its local name. Source
identifier, first non-blank of `MSH/MSH.3/HD.1`, `MSH/MSH.4/HD.1`,
`PID/PID.3/CX.4/HD.1` (local names, any prefix), else `Client.Validation` /
400 `Unable to determine assigning authority from payload.` Control id:
`MSH/MSH.10`. Message type: `MSH/MSH.9/MSG.2` (§10).
Success: `<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:tns="urn:inthub:rest-server"><soapenv:Body><tns:AckResponse><Status>Success</Status><MessageControlId>{id}</MessageControlId></tns:AckResponse></soapenv:Body></soapenv:Envelope>`.
Error: a `soapenv:Fault` with `faultcode` and `faultstring`.

*`xml-raw`* (`application/xml; charset=utf-8`): the body is the payload.
Malformed → `Client` / 400 `Malformed XML request.`. Structure id: the root's
local name. Source identifier and control id: the first non-blank text at
`SOURCE_IDENTIFIER_LOCATOR` / `MESSAGE_CONTROL_ID_LOCATOR` (slash-separated
local names from the root), or absent. Success
`<ack><status>Success</status><messageControlId>{id}</messageControlId></ack>`;
error `<error><code>{code}</code><message>{message}</message></error>`.

§9.4.1 All interpolated values in a response are XML-escaped (`& < > " '`).

§9.4.2 XML is parsed without DTD or external-entity resolution (OWASP A05;
`defusedxml` upstream, `roxmltree` here, which does neither).

§9.5 **Validators** (`VALIDATOR_TYPE`):

| Value | Behaviour |
| --- | --- |
| `none` | no schema validation (size and well-formedness only) |
| `hl7-xsd` | structure id required and in `ALLOWED_HL7_STRUCTURES` (`400`, `Unsupported HL7 message structure '{id}'. Allowed values: {sorted csv}`); the flow named by `VALIDATION_SCHEMA` must have the structure (else `500` `Server.Configuration`, `Schema mapping is not configured for group '{g}' and structure '{s}'.`); then validate: `Payload schema validation failed: {reason}` |
| `hl7apy` | convert to ER7 (`Unable to convert XML payload to ER7 format for validation.`), parse (`Unable to parse converted ER7 payload for structure verification.`), the structure derived from `MSH-9` must equal the declared one (`Declared message structure '{d}' does not match the structure '{a}' specified in MSH.9 of the payload.`) and be allowed, then standard-release validation (`Payload validation failed: {reason}`) |
| `xsd` | validate against the XSD file at `VALIDATION_SCHEMA`: `Payload schema validation failed: {reason}`; an unreadable file is `Configured schema file '{path}' could not be read` |

§9.5.1 **`hl7-xsd` validates through the flow dictionary**, not the XSD: the
payload is converted to ER7 and checked by `validate_and_convert`, exactly as
`hl7-soap-server` does (`08-hl7-soap-server.md` §3 steps 6, 7 and 10). That
is `06-validation.md` §3's substitution, inherited.

§9.5.2 **`hl7apy` is `hl7-2`'s standard-release validation.** There is no
hl7apy in Rust (`06-validation.md` §10.7). The release is the message's own
`MSH-12` and must be one of `SUPPORTED_VERSIONS`; the structure comes from
the same resolution `hl7-xsd` uses (`06-validation.md` §2). Findings are
reduced to path and reason (§0.2). Recorded in §11.

§9.5.3 **`hl7-2` finds less than hl7apy does.** It reports a field that breaks
its release's table; it does **not** report a missing required segment (an
`ADT_A05` with no `PID` passes). `hl7apy` mode therefore accepts some payloads
upstream rejected. The structure check (§9.5) and the allow-list are
unaffected. Deployments that need the stricter check use `hl7-xsd`, whose
flow schema does require the segment.

§9.5.4 **`ALLOWED_TRIGGER_EVENTS`** (upstream `#455`; `generic` pipeline). A
comma-separated list; empty means any. After the source allow-list (step 5)
and before the output is built, the extracted trigger event (`MSH-9.2`) must be
in the list, else `Client.Validation` / **400**
`Unsupported HL7 trigger event '{event}'. Allowed values: {sorted, ", "}`,
where an absent trigger reads `unknown`. Nothing is stored or sent. The LIMS
deployment sets `A28,A31,A40`, so a message the downstream subscriptions would
never route is refused at the door.

**Not ported from `#455`:** the `hl7apy` validator's trigger-to-structure
tolerance (no deployed use here; `hl7apy` mode here is already the lenient
one, §9.5.3).

§9.7 **The secondary WRRS destination** (upstream `#455`; `generic`
pipeline). Optional: with neither `WRRS_QUEUE_NAME` nor `WRRS_TOPIC_NAME`
set, nothing changes. Naming one (not both) requires `WRRS_EGRESS_SESSION_ID`
and `WRRS_WORKFLOW_ID`, every missing one reported together at startup.
`WRRS_TRIGGER_EVENTS` (csv, default `A31,A40`) lists the triggers sent
there too. After the primary send (step 9) and before the success events, a
message whose trigger is listed is sent again with the same body (the output
of step 6), the **WRRS session**, `MessageId` = the control id, and the usual
tracking properties with the **WRRS workflow id**, the same correlation id and
receipt time, and `MessageType`. The send is blocking and **fatal**: a failure
is a `500` (`Server`, the generic text), since a `200` acknowledges delivery.
Upstream does the same, so a retry after a WRRS failure repeats the primary
send; consumers de-duplicate on the message id. No deployed environment here
sets it (LIMS reaches WRRS through the topic's subscription filter), so this
is covered by unit tests only, not a Compose run. The `hl7` pipeline ignores
these variables beyond its own `risp` rules.

§9.6 `OUTPUT_FORMAT=er7` failure is `Client.Validation` / 400 `Unable to
convert XML payload to ER7 format.`.

---

## §10 `MessageType` on generic-pipeline messages (`e090e23`)

§10.1 When the content adapter extracts a message type (the `soap` adapter:
`MSH.9/MSG.2`, the trigger event, e.g. `A31`) and it is non-empty, the
Service Bus message carries an application property **`MessageType`** with
that value, so subscriptions can filter on trigger event. When it is absent
or blank the property is **not set** (not set empty): a subscription rule on
a property that exists differs from one on a property that does not.

§10.2 It is set on the forwarded message only. The message store's event has
no property bag.

§10.3 `xml-raw` never sets it.

§10.4 The value is a trigger code (`A31`), an MSH field; it is not patient
data and may appear in a property.

---

## §11 Deviations and what is not ported

| Item | Upstream | Here | Why |
| --- | --- | --- | --- |
| `/docs`, `/redoc`, `/openapi.json` | FastAPI-generated, `DEV`/`SIT` only (`hl7`) / always (`generic`) | not served | No generator; a hand-written contract drifts |
| `hl7apy` validator | hl7apy reference tables | `validate_standard_parsed` (§9.5.2); accepts a payload missing a required segment (§9.5.3) | `06-validation.md` §10.7 |
| RISP ORU/OMG schema check | XSD validation of the XML | grammar match on segment tags (§6.4) | XSD not on disk in the container |
| `hl7-xsd` | XSD | flow dictionary (§9.5.1) | `06-validation.md` §3 |
| NACK `MSA-3` | verbatim | sanitised (§4.4) | A reason with `\|` or `\r` corrupts the segment |
| `python-dotenv` `.env` | loaded at start | not read | `spec/02` §1: the environment is the only source |
| Request body for `hl7` | unbounded until parsed | bounded (§2.1) | OWASP A05 |
| `/status` timestamps | naive local time | UTC | `spec/index.md` §0.7 |
| Service Bus failure | exception text in the NACK, no event | §3.4 reason, `MESSAGE_FAILED` | Operators need to see it |
| `messages_sent` on `hl7` | never incremented | per successful target (§3.4.1) | A zero counter on a delivering service |
| RISP ORU sample | upstream's helper has no `PV1`; its tests mock the schema check | tests carry a `PV1` | The custom `2_5_1` schema requires `ORU_R01.VISIT` (§6.4) |

---

## §12 Tests

Each Python test module has a Rust counterpart; the mapping is the
`tests/` and `#[cfg(test)]` modules of the two crates, each test named for or
commenting the clause it protects.

| Python | Rust |
| --- | --- |
| `tests/hl7/test_hl7_ack_builder.py` | `hl7-rest-server` `ack` |
| `tests/hl7/test_risp_validation.py` | `hl7-rest-server` `rules` |
| `tests/hl7/test_risp_routing.py` | `hl7-rest-server` `risp` |
| `tests/hl7/test_message_input_adapter.py` | `hl7-rest-server` `input` |
| `tests/hl7/test_hl7_message_processor.py` | `hl7-rest-server` `process` |
| `tests/hl7/test_message_route.py`, `test_health_routes.py` | `hl7-rest-server/tests/endpoint.rs` |
| `tests/test_app_config.py` | `rest-server` `config`, `hl7-rest-server` `config` |
| `tests/test_content_adapters.py` | `rest-server` `adapter` |
| `tests/test_validators.py` | `rest-server` `validator` |
| `tests/test_message_processor.py` | `rest-server` `process` |
| `tests/test_asgi_app.py`, `test_rest_server_application.py` | `rest-server/tests/endpoint.rs` |
| `tests/test_hl7_soap_server_parity.py` | `rest-server/tests/endpoint.rs` (the LIMS sample through the `soap` + `hl7-xsd` + `er7` profile) |
