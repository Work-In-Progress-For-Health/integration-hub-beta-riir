# 08 — HL7 SOAP ingress server

An HTTP endpoint that accepts an HL7 v2.xml payload inside a SOAP envelope,
validates it, and forwards it as ER7. Implemented by
`crates/hl7-soap-server`, over `hl7-2-soap`.

Reference implementation: `Integration-Hub-Beta/hl7_soap_server/`.

---

## §1 Endpoints

| Method | Path | Behaviour |
| --- | --- | --- |
| `POST` | `SOAP_ENDPOINT_PATH` | Process a SOAP request (§3) |
| `GET` | `SOAP_ENDPOINT_PATH?wsdl` | Return the generated WSDL (§6) |
| `GET` | `SOAP_ENDPOINT_PATH` (no `?wsdl`) | 405 with a `Client` fault, `SOAP endpoint accepts POST only.` |
| `POST` | any other path | 404 with a `Client` fault, `Unknown SOAP endpoint path.` |

§1.1 Every response has `Content-Type: text/xml; charset=utf-8` and an
explicit `Content-Length`.

§1.2 The request target MAY be absolute-form
(`https://host/soap?wsdl`) rather than origin-form; some reverse proxies,
including Azure Container Apps ingress, forward it that way. The path and
query MUST be extracted with a URL parse, not by splitting on `?`.

---

## §2 Request admission

Checked before the body is read, in this order
(`hl7_soap_server_application.py :: do_POST`):

| Condition | Status | Fault code | Fault string |
| --- | --- | --- | --- |
| Path mismatch | 404 | `Client` | `Unknown SOAP endpoint path.` |
| No `Content-Length` | 411 | `Client` | `Content-Length header is required.` |
| Unparseable `Content-Length` | 400 | `Client` | `Invalid Content-Length header.` |
| `Content-Length` ≤ 0 | 400 | `Client` | `SOAP request body is empty.` |
| `Content-Length` > `MAX_REQUEST_SIZE_BYTES` | 413 | `Client` | `SOAP request body exceeds configured limit of {limit} bytes. Received: {n} bytes.` |
| Body is not valid UTF-8 | 400 | `Client` | `SOAP request body must be UTF-8 encoded.` |

§2.1 The size check happens on the declared length, before reading. The
reader MUST additionally be bounded so a lying `Content-Length` cannot
allocate without limit.

§2.2 **An unparseable `Content-Length` is answered by the HTTP layer, not
this table.** The Rust serves HTTP through `hyper`, which treats a
`Content-Length` it cannot parse as a protocol violation and answers `400`
with an empty body before any handler runs. The **status is as this table
requires**; the fault envelope is absent. That is accepted: RFC 9112 §6.3
requires a recipient to reject such a message, and refusing it at the
protocol layer is more correct than parsing it at the application layer as
the Python did. The row's check is still implemented, and still applies to
any value `hyper` admits. See
[`plan.md` §9](../plan.md#9-deliberate-deviations-from-the-python).

---

## §3 Processing

`soap_processor.py :: SoapMessageProcessor.process`. In order:

```
1  MESSAGE_RECEIVED event, content = the whole SOAP request
2  messages_received metric +1
3  parse the envelope; take the single business payload from the body
4  structure = the payload element's local name
5  reject if structure not in ALLOWED_HL7_STRUCTURES
6  resolve the XSD group for (HL7_SCHEMA_GROUP, structure)
7  validate the payload against it
8  VALIDATION_SUCCESS event
9  extract the assigning authority; reject if not allowed
10 convert the payload XML to ER7
11 mint tracking metadata
12 send to the message store: RawPayload = the SOAP request,
                              XmlPayload = the payload element
13 send the ER7 to Service Bus, message_id = MSH-10
14 messages_sent metric +1
15 MESSAGE_PROCESSED event
16 return 200 with the success envelope
```

§3.1 Faults, with the status each carries:

| Condition | Status | Fault code | Fault string |
| --- | --- | --- | --- |
| Malformed XML | 400 | `Client` | `Malformed SOAP XML request.` |
| Root is not `Envelope` | 400 | `Client` | `SOAP Envelope element is missing.` |
| No `Body` | 400 | `Client` | `SOAP Body element is missing.` |
| Body has zero or several element children | 400 | `Client` | `SOAP Body must contain exactly one business payload element.` |
| Structure not allowed | 400 | `Client.Validation` | `Unsupported HL7 message structure '{s}'. Allowed values: {sorted, comma-space}` |
| No schema for the group and structure | 500 | `Server.Configuration` | `SOAP schema mapping is not configured.` |
| Schema validation failed | 400 | `Client.Validation` | `Payload schema validation failed: {reason}` |
| Authority not allowed | 403 | `Client.Authorization` | `Assigning authority '{a}' is not authorised. Allowed values: {sorted, comma-space}` |
| XML → ER7 conversion failed | 400 | `Client.Validation` | `Unable to convert XML payload to ER7 format.` |
| Anything else | 500 | `Server` | `Unexpected server error while processing SOAP request.` |

§3.1a **Checked arithmetic** (`rust-checked-arithmetic`): the crate denies
`arithmetic_side_effects`, `as_conversions`, `indexing_slicing`, `string_slice`,
`integer_division` and `unreachable`. `payload::extract` locates the payload
with `str::get` and `checked_*`, and an empty local name matches nothing (it
would otherwise never advance). Nothing in this crate substitutes a value for
invalid input: a payload that cannot be located is `None`, and `hl7-2-soap`
refuses the message.

§3.2 Element matching is on **local name only**. The same envelope arrives
as `soapenv:`, `soap:`, `SOAP-ENV:` or unprefixed depending on the sending
stack, and all four MUST be read alike.

§3.3 Every fault emits `VALIDATION_FAILED` and `MESSAGE_FAILED` with the
fault string and the validation result `SOAP request rejected`; the
catch-all emits `MESSAGE_FAILED` with `Unhandled SOAP server error`.

§3.4 The message store write is non-blocking: a failure is logged as
`Failed to send SOAP message to message store: {error}` and does not affect
the response.

§3.5 XML parsing MUST be hardened against entity expansion and external
entity resolution. The Python uses `defusedxml`; the Rust reader MUST NOT
resolve external entities and MUST bound entity expansion.

---

## §4 Assigning authority

`soap_processor.py :: _extract_assigning_authority`. The first non-empty of:

1. `MSH` → `MSH.3` → `HD.1`
2. `MSH` → `MSH.4` → `HD.1`
3. `PID` → `PID.3` → `CX.4` → `HD.1`

§4.1 None found is a 400 `Client.Validation` fault,
`Unable to determine assigning authority from payload.`

§4.2 The path walk descends by local name at each level and takes the first
descendant with non-blank text, across all matching branches at that level.

---

## §5 Responses

§5.1 Success:

```xml
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:tns="urn:inthub:hl7-soap-server"><soapenv:Body><tns:AckResponse><Status>Success</Status><MessageControlId>{id}</MessageControlId></tns:AckResponse></soapenv:Body></soapenv:Envelope>
```

§5.2 Fault:

```xml
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body><soapenv:Fault><faultcode>{code}</faultcode><faultstring>{text}</faultstring></soapenv:Fault></soapenv:Body></soapenv:Envelope>
```

§5.3 `{id}` and `{text}` are XML-escaped for `&`, `<`, `>`, `"` and `'`. A
missing message control id renders as empty.

§5.4 A fault string MUST NOT contain a field value; §3.1's schema-validation
fault carries the reason and path from
[`06-validation.md`](06-validation.md) §3.3.3 and nothing more.

---

## §6 WSDL

§6.1 The WSDL is generated on demand, addressed at the host the request
arrived on, so `soap:address` always matches the environment.

§6.2 The scheme is taken from `X-Forwarded-Proto` (first value, trimmed)
when present, else `https` if TLS is configured, else `http`.

§6.3 The host is the `Host` header, falling back to the bound address.

§6.4 Target namespace `urn:inthub:hl7-soap-server`; payload elements
`ADT_A05` and `ADT_A39` in `urn:hl7-org:v2xml`, declared as `xsd:anyType`.
The real HL7 structure is deliberately **not** imported: the relative
`xsd:include` paths break when the WSDL is copied out of the repository for
SoapUI, and the authoritative validation is server-side anyway.

§6.5 A generation failure returns 500 with the `Server` fault
`Unable to generate WSDL document.`

---

## §7 TLS

§7.1 TLS is enabled when both `TLS_CERT_FILE` and `TLS_KEY_FILE` are
configured. One without the other is a startup failure
([`02-configuration.md`](02-configuration.md) §4.1).

§7.2 The certificate chain and key are PEM.

---

## §8 Tests

§8.1 Unit: every admission check of §2 and every fault of §3.1, asserting
both status and fault string.

§8.2 Unit: §3.2, with the same envelope in four prefix spellings.

§8.3 Unit: §4's precedence, including the fallback chain and the failure.

§8.4 Unit: §5.3's escaping, with a control id containing every escaped
character.

§8.5 Integration: `POST` of `local/sample_messages/lims-to-mpi.sample.xml`
yields 200 and the expected ER7 on the fake bus.

§8.6 Integration: `GET ?wsdl` in origin-form and absolute-form both return
the WSDL, and the address reflects `X-Forwarded-Proto`.

§8.7 Security: an XML bomb and an external-entity reference are both
rejected without expansion (§3.5).
