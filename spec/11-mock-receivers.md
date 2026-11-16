# 11 — Mock receivers

Test doubles for the systems the Hub delivers to. Implemented by
`crates/hl7-mock-receiver` and `crates/http-mock-receiver`.

Reference implementation: `Integration-Hub-Beta/hl7_mock_receiver/`,
`Integration-Hub-Beta/hl7_mock_receiver_rust/` (an existing Rust port),
`Integration-Hub-Beta/http_mock_receiver/`.

These are the least clinically risky components in the estate — a defect
costs a confusing test run, not a patient — which is why the Python
repository's first Rust service was one of them.

---

## §1 The mock convention

§1.1 **The word `fail`, in any case, anywhere in the request, means "answer
`AE`"** (the sender should retry), and **`reject` means "answer `AR`"** (the
sender should dead-letter; upstream `330f1a7`). `reject` wins when both
appear. This is how a test drives either unhappy path without constructing a
malformed message, and `hl7-mock-receiver` honours both.

§1.2 A mock MUST always answer. A mock that goes silent looks like a hung
endpoint, which is the harder failure to diagnose than a rejection.

---

## §2 `hl7-mock-receiver`

§2.1 Listens for MLLP frames on `HOST:PORT` (default `127.0.0.1:2576`).

§2.2 Forwards the received payload **unchanged** to `EGRESS_QUEUE_NAME`,
under the message's own `MSH-10` as the Service Bus message id.

§2.3 Replies with an `ACK`:

| Condition | `MSA-1` |
| --- | --- |
| Forwarded successfully | `AA` |
| §1.1's `fail` trigger | `AE` |
| §1.1's `reject` trigger | `AR` |
| The queue would not take the message | `AE` |
| The message could not be parsed at all | the fallback of §2.5 |

§2.3.1 The third row matters: an acknowledgement that says `AA` when nothing
was forwarded is a lie, and a test that relies on the queue would then hang
rather than fail.

§2.4 ACK fields are as [`07-hl7-server.md`](07-hl7-server.md) §4 **except
that `MSH-11` is always the literal `P`**, whatever the inbound message
said. This differs from `hl7-server`, which copies it. The difference is
inherited from the Python mock and is preserved because tests assert on it.

§2.5 The fallback reply, for a payload that cannot be read as HL7 at all, is
byte-for-byte:

```
MSH|^~\&|||||||NACK||P|2.5|||AL<CR>MSA|AE|UNKNOWN|Error processing message<CR>
```

MLLP-framed. The Python returned it unframed while every other path framed
its reply, so a peer waiting for `<FS><CR>` never saw the end of it; framing
it is a deliberate correction
([`plan.md` §9.2](../plan.md#9-deliberate-deviations-from-the-python)).

§2.6 With neither `SERVICE_BUS_CONNECTION_STRING` nor
`SERVICE_BUS_NAMESPACE` set, the receiver still answers MLLP and logs what
it would have forwarded. This is how it runs on a laptop.

§2.7 A frame larger than `MAX_MESSAGE_SIZE_BYTES` (default 16 MiB) closes
the connection rather than being buffered without bound.

§2.8 Pipelined frames each get their own reply, per
[`07-hl7-server.md`](07-hl7-server.md) §2.5.

§2.9 Shuts down cleanly on `SIGTERM` and `SIGINT`.

### §2.10 Adoption

`Integration-Hub-Beta/hl7_mock_receiver_rust/` already implements all of the
above and its acknowledgements have been checked field by field against the
Python for HL7 2.3.1, 2.4 and 2.5, positive and negative. It is **adopted**
rather than rewritten: moved to `crates/hl7-mock-receiver`, re-pointed at
this workspace's shared crates, and its `hl7-v2-mllp` dependency updated to
the renamed `hl7-2-mllp`. Its tests come across unchanged and MUST keep
passing.

---

## §3 `http-mock-receiver`

A SOAP endpoint that accepts anything vaguely SOAP-shaped and answers.

§3.1 Listens on `HOST:PORT` (default `127.0.0.1:8080`).

§3.2 **Deliberately forgiving.** Unlike `hl7-soap-server`, a body that is
not well-formed XML is *not* an error: the parse failure is logged at warn
and the receiver still answers per §1.1, so a caller always gets a valid
response.

§3.3 SOAP version detection is by namespace substring in the raw body:
`http://www.w3.org/2003/05/soap-envelope` present means 1.2, otherwise 1.1.
The reply uses the detected version's namespace and content type:

| Version | Namespace | `Content-Type` |
| --- | --- | --- |
| 1.1 | `http://schemas.xmlsoap.org/soap/envelope/` | `text/xml; charset=utf-8` |
| 1.2 | `http://www.w3.org/2003/05/soap-envelope` | `application/soap+xml; charset=utf-8` |

§3.4 **Payload extraction.** Within the `Body` element:

1. The first immediate child whose local name is one of `hl7Message`,
   `HL7Message`, `message`, `Message`, `payload`, `Payload`, with non-blank
   text.
2. Otherwise, the first descendant whose trimmed text starts with `MSH`.
3. Otherwise, no payload.

§3.5 **Control id extraction** is done by splitting the ER7 on the field
separator, without a full parse: the 10th field of the first `MSH` segment,
or the literal `UNKNOWN`. Line endings `\r\n` and `\n` are normalised to
`\r` first.

§3.6 Success response:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<soapenv:Envelope xmlns:soapenv="{ns}">
  <soapenv:Header/>
  <soapenv:Body>
    <AcknowledgementResponse>
      <Status>AA</Status>
      <MessageControlID>{escaped id}</MessageControlID>
    </AcknowledgementResponse>
  </soapenv:Body>
</soapenv:Envelope>
```

§3.7 The fault response is a SOAP `Fault` in the detected version's
namespace, so `soap-sender`'s evaluation
([`10-senders.md`](10-senders.md) §4.4) rejects it.

§3.8 XML parsing MUST be hardened against DTDs, entity expansion and
external references, as
[`08-hl7-soap-server.md`](08-hl7-soap-server.md) §3.5 — the Python uses
`defusedxml` here even though this is only a mock, and the Rust keeps that
posture.

§3.8.1 **Deviation from §3.2 and §3.4: the Rust mock does not build an XML
tree.** It scans the body text for the §3.4 element names, which is what
makes it immune to DTDs and entity expansion without a parser (§3.8). Two
consequences, both confined to what is *logged* or *forwarded*, never to the
reply a caller receives:

1. A malformed body produces **no** warn line. §3.2's "logged at warn" is
   met by the Python's `defusedxml` parse failing; there is no parse here to
   fail.
2. The payload element is the first match for a §3.4 name **anywhere** in
   the body, not only an immediate child of `Body`. They differ only for a
   request that nests a payload-named element inside another element and
   also carries one as a direct child of `Body`.

Neither affects §1.1's reply rules or §3.7's fault. Chosen because the
mock's value is that it always answers (§1.2), and a parser is a new way to
fail to. Checked against the Python (`soap_handler.py`: `SafeET.fromstring`,
`_find_body`, immediate-child walk) on 2026-11-04. If a test ever needs the
Python's exact payload choice on such a request, the fix is a tree walk
confined to `Body`'s children, not a change to this clause.

§3.9 Forwarding to `EGRESS_QUEUE_NAME` is optional; absent means log-only
mode.

§3.10 **The Promptly mock** (upstream `#442`, `#454`). The same binary also
answers the WPAS-Promptly `$process-message` asynchronous API, so
`fhir-sender` ([`10-senders.md`](10-senders.md) §8) can be tested without
Promptly. `crates/http-mock-receiver/src/fhir.rs`.

| Request | Answer |
| --- | --- |
| `POST /fhir/r4/$process-message` | `202` and an absolute `Content-Location` built on the request's `Host`; or a rejection chosen by `Bundle.id` |
| `GET /fhir/r4/status/{id}` | `202` (`X-Progress: Processing`, `Retry-After: 1`) until the job's polls run out, then its final answer; `404` for an unknown id |

A body that is not JSON, or not a `Bundle` of `type` `message`, is a `400`
`OperationOutcome`. Everything else on any other path is the SOAP endpoint of
§3.1 to §3.8, unchanged.

Behaviour is chosen by `Bundle.id`:

| `Bundle.id` | Behaviour |
| --- | --- |
| anything else | `202`, two `202` polls, then `200` success |
| `mock-slow` | six `202` polls, then success |
| `mock-error-outcome` | `200` with an `error` `OperationOutcome` |
| `mock-status-400` | the final poll answers `400` |
| `mock-400`, `-401`, `-403`, `-404`, `-409`, `-429`, `-500` | the `POST` is rejected with that status; `429` adds `Retry-After: 1` |
| `mock-429-once`, `mock-500-once` | the first `POST` for that id fails; the retry is accepted |

State is in memory. A finished job keeps answering the same way for five
minutes (so a repeated `GET` after a dropped connection is consistent), then
is forgotten.

§3.10.1 **Forwarding.** An accepted (`202`) Bundle is forwarded, raw, to
`EGRESS_QUEUE_NAME` when one is configured, with `EGRESS_SESSION_ID`. A
rejected `POST` is **not**, so a `*-once` scenario puts one message on the
queue, and an end-to-end test can read the queue to confirm a message reached
the end of the flow. **Deviation:** upstream logs a failed forward and still
answers `202`. Here it answers `500`, for the reason §3.9 gives for SOAP: a
`202` that forwarded nothing makes a test waiting on the queue hang rather
than fail, and `fhir-sender` treats a `500` as transient and retries.

---

## §4 Tests

§4.1 Unit: §2.4's ACK fields for 2.3.1, 2.4 and 2.5 inbound, positive and
negative — the existing tests from `hl7_mock_receiver_rust`.

§4.2 Unit: §2.5's fallback, asserting the exact bytes and the framing.

§4.3 Unit: §3.4's extraction precedence, including the `MSH` fallback and
the no-payload case.

§4.4 Unit: §3.5's control id, including a message with fewer than 10 fields.

§4.5 Unit: §3.3's version detection and the content type it selects.

§4.6 Integration: `hl7-sender` against `hl7-mock-receiver` end to end,
asserting a message reaches the fake bus and the sender completes it.

§4.7 Integration: `soap-sender` against `http-mock-receiver` end to end,
including the `fail` trigger producing a fault that the sender rejects.

§4.8 Unit: every scenario of §3.10, the retention of a finished job against
an explicit clock, and the status URL built on the given origin.

§4.9 Integration: `fhir-sender`'s real client against the Promptly mock over a
socket: a success is polled to the end and forwarded once; a rejected `POST` is
`Permanent` and not forwarded; an error `OperationOutcome` after a `202` is
`Permanent`; a `403` is an authorization failure; `mock-500-once` is delivered
and forwarded once.

---

## §5 `hl7-topic-mock-receiver`

Reference implementation:
`Integration-Hub-Beta/mock_receivers/hl7_topic_mock_receiver/`. A second
binary of `crates/hl7-mock-receiver`, the way `hl7-subscription-sender` is a
second binary of `hl7-sender`.

§5.1 Everything in §1 and §2 applies, **except** that the message is
forwarded to a Service Bus **topic**, `EGRESS_TOPIC_NAME`, so several
subscriptions can consume the same traffic. Sending to a topic is the same
operation as sending to a queue; only the name differs.

§5.2 **The topic is optional.** Unlike §2's `EGRESS_QUEUE_NAME`, a missing
`EGRESS_TOPIC_NAME` is not a startup failure: the receiver then runs as in
§2.6, logging what it would have forwarded. It forwards only when the topic
**and** one of `SERVICE_BUS_CONNECTION_STRING` / `SERVICE_BUS_NAMESPACE` are
set. A topic with no connection information is also log-only, with a warning,
as the Python does (`service_bus_enabled`).

§5.3 `EGRESS_SESSION_ID`, `HOST`, `PORT` (default `2576`), the health-check
variables and `MAX_MESSAGE_SIZE_BYTES` are as §2. `EGRESS_QUEUE_NAME` is not
read.

§5.4 **Deviation, inherited from §2.3.** The Python forwards first and then
answers `AE`/`AR` for `fail`/`reject`; `hl7-mock-receiver` answers the NACK
without forwarding, and so does this binary, so the two mocks are
interchangeable. A topic subscriber therefore sees only messages that were
acknowledged `AA`, which is what the Python README means by "verified
traffic".

§5.5 Tests: `EGRESS_TOPIC_NAME` alone starts log-only; topic plus
connection string forwards; topic without connection is log-only; no
variables is valid.
