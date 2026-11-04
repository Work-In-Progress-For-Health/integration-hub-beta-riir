# 10 — Senders

Egress: delivering a message from Service Bus to a destination system over
MLLP or SOAP. Implemented by `crates/hl7-sender`,
`crates/hl7-subscription-sender`, `crates/soap-sender`,
`crates/soap-subscription-sender`.

Reference implementation: `Integration-Hub-Beta/hl7_sender/`,
`hl7_subscription_sender/`, `soap_sender/`, `soap_subscription_sender/`.

The four are one pipeline with two axes: **source** (queue or topic
subscription) and **transport** (MLLP or SOAP). §2 is common; §3 and §4 are
the transports; §5 is the subscription variant.

---

## §1 Shape

Each sender is a receive loop that, per message: emits an audit event,
optionally persists the message, waits for its throttle slot, delivers it,
evaluates the acknowledgement, and reports handled or not handled.

§1.1 A sender MUST report **not handled** for anything short of a positive
acknowledgement, so the message is abandoned and retried. It MUST NOT
complete a message it did not deliver.

---

## §2 Common processing

`hl7_sender/application.py :: _process_message`.

```
1  decode the body as UTF-8
2  read the incoming application_properties
3  log CorrelationId / WorkflowID / SourceSystem / MessageReceivedAt at info
4  MESSAGE_RECEIVED event, audit text "Message received for HL7 sending"
5  parse the ER7, read MSH-10 as the message id, log it at info
6  if this is the first delivery attempt (§2.2), send to the message store
7  throttle (§2.4)
8  deliver (§3 or §4)
9  evaluate the acknowledgement
10 on success: messages_sent metric +1
11 MESSAGE_PROCESSED event, audit text
   "Message sent successfully, received ACK: {ack}"
12 return the acknowledgement's verdict
```

§2.1 Step 11 emits `MESSAGE_PROCESSED` **even when the acknowledgement was
negative**. The Python does this, and the verdict is carried by the return
value rather than the event. A negative acknowledgement is therefore visible
as a `MESSAGE_PROCESSED` whose audit text names the code — not as a
`MESSAGE_FAILED`.

§2.2 **First-attempt detection.** The Service Bus delivery count is read;
zero or less means a first attempt. A missing or unparseable value MUST be
treated as a first attempt. On a retry, the store send is skipped and the
skip is logged at info naming the correlation id and the delivery count.

§2.3 **Batch sizing.** The receive batch size is `MAX_BATCH_SIZE`, reduced
so a whole batch fits inside the Service Bus lock-renewal window:

```
window    = LOCK_RENEWAL_DURATION_SECONDS (300) - 30
allowable = floor(window / throttle_interval) + 1
batch     = clamp(allowable, 1, MAX_BATCH_SIZE)
```

With no throttle configured the batch is `MAX_BATCH_SIZE`. A reduction MUST
be logged at warn naming both sizes, the limit and the interval.

§2.4 **Throttling.** `message_throttler.py`. Configured by
`MAX_MESSAGES_PER_MINUTE`; absent means unthrottled. The interval is
`60 / max_messages_per_minute` seconds.

§2.4.1 The first message is never delayed.

§2.4.2 Subsequent messages wait until `last_target + interval`. If that
moment has already passed — the sender was idle, or is behind — the target
is reset to *now* rather than to the overdue moment, so a burst is not
emitted to catch up. This is the subtle part and MUST be reproduced: the
recorded time is the *target*, not the actual send time.

§2.4.3 A configured value of zero or less is a startup failure.

§2.5 **Store event.** Built per [`03-message-bus.md`](03-message-bus.md) §6
with `RawPayload` = the message body, `XmlPayload` = the schema-unaware
v2.xml conversion, `SessionId` = the sender's own `INGRESS_SESSION_ID`, and
the received metadata for the rest. A conversion failure is logged at error,
emitted as `VALIDATION_FAILED` with
`Failed to generate XML payload for message store: {error}`, and the event
is still sent with a null `XmlPayload`.

§2.6 **Failure classification.**

| Failure | Log | `MESSAGE_FAILED` validation result |
| --- | --- | --- |
| Timeout or connection error | `Failed to send message {id}: {e}` | `Message sending failed - connection/timeout error` |
| Anything else | `Unexpected error while processing message: {e}` | `Unexpected processing error` |

Both return not handled.

---

## §3 MLLP delivery

`hl7_sender/hl7_sender_client.py`.

§3.1 One long-lived TCP connection to `RECEIVER_MLLP_HOST:RECEIVER_MLLP_PORT`,
with a read timeout of `ACK_TIMEOUT_SECONDS`.

§3.2 Before each send, the connection is checked for closure and rebuilt if
the peer has gone away. A rebuild is logged at info.

§3.3 The message is MLLP-framed, written, and a framed reply is read.

§3.4 **Timeout retry.** A read timeout closes the connection, opens a new
one, and retries the send **exactly once**. A second timeout closes the
connection and fails with
`No ACK received within {n} seconds`, classified as a timeout by §2.6.

§3.5 Any other socket error closes and rebuilds the connection, and fails
with `Connection error while sending message: {e}`, classified as a
connection error.

§3.6 The reply's MLLP framing is stripped before evaluation.

### §3.7 Acknowledgement evaluation

`ack_processor.py :: get_ack_result`.

| Condition | Verdict |
| --- | --- |
| Reply has no `MSA` segment | failure, logged `Received a non-ACK message` |
| `MSA-1` in `{AA, CA}` | success, logged `Valid ACK received.` at info |
| Any other `MSA-1` | failure, logged `Negative ACK received: {code} for: {control_id}` where the control id is the reply's `MSH-10` |
| The reply cannot be parsed | failure, logged with a backtrace |

§3.7.1 **`AR` is dead-lettered; everything else is retried.** Upstream
`62b8c34` (`ack_processor.py :: AckOutcome`) classifies a reply as one of:

| Outcome | When | Settlement | Event | Metrics |
| --- | --- | --- | --- | --- |
| `SUCCESS` | `MSA-1` in `{AA, CA}` | complete | `MESSAGE_PROCESSED` | `messages_sent` |
| `AE` | any other `MSA-1` except `AR`, recognised or not | abandon; the rest of the batch waits | `MESSAGE_FAILED`, validation result `Recoverable NACK (AE) - message will be retried` | `messages_nack_ae`, `messages_retry_attempt` |
| `AR` | `MSA-1 = AR` | **dead-letter** with reason `AR` and description `Application Reject ACK (AR) for message_id={MSH-10}, correlation_id={id}`; the rest of the batch waits | `MESSAGE_FAILED`, validation result `Non-recoverable NACK (AR) - message routed to dead-letter/escalation path` | `messages_nack_ar` |
| `INVALID` | no `MSA`, or the reply cannot be parsed | abandon | `MESSAGE_FAILED`, validation result `Recoverable NACK (INVALID) - message will be retried` | none |

The three NACK metrics carry `ack_code`, `correlation_id`, `message_id` and
`endpoint` (`04-observability.md` §3.1.3). The dead-letter description
carries identifiers and **never** payload (`index.md` §0.8): a dead-letter
queue is read by people who are not entitled to the message.

This supersedes the earlier rule that `AR` and `AE` were both retried until
`MaxDeliveryCount`. It applies to the two HL7 senders only; the SOAP senders
are unchanged (§4.4), and keep the single-state `Rejected` verdict and
the `MESSAGE_PROCESSED` event that goes with it (§2.1).

---

## §4 SOAP delivery

`soap_sender/soap_sender_client.py`.

§4.1 An HTTP client with `Content-Type: text/xml; charset=utf-8`, an
`Authorization: ApiKey {key}` header when `SOAP_API_KEY` is set, and a
client certificate when `SOAP_CLIENT_CERT_PATH` is set.

§4.2 The ER7 is wrapped:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/">
  <soapenv:Header/>
  <soapenv:Body>
    <SendHL7Message>
      <hl7Message>{escaped}</hl7Message>
    </SendHL7Message>
  </soapenv:Body>
</soapenv:Envelope>
```

with `&`, `<` and `>` escaped — and only those three, matching the Python.

§4.3 A request timeout retries **once** with a fresh client, then fails with
`No SOAP response within {n} seconds`. A connection error fails immediately
with `SOAP connection error: {e}`.

### §4.4 Response evaluation

`soap_ack_processor.py :: get_ack_result`.

| Condition | Verdict |
| --- | --- |
| HTTP status not in `{200, 202}` | failure |
| Body contains the substring `Fault` | failure — checked before parsing, and independent of status |
| A `Status` element with text in `{AA, CA}` | success |
| A `Status` element with any other text | failure |
| Body is not parseable as XML | fall through to the next row, with a warning |
| HTTP 200 or 202, no fault, no `Status` element | **success** |

§4.4.1 The last row is deliberate: some endpoints answer a minimal 200 with
an empty body. It is also the reason the `Fault` substring check in row 2
exists at all — a fault must not slip through as "no status element".

§4.4.2 At most 500 characters of a faulted body, and 200 of an unparseable
one, are logged. Response bodies may echo message content, so what is logged
MUST pass through redaction.

§4.5 `WS_SECURITY_ENABLED` is reserved and MUST be `false`. A `true` value
is a startup failure until this section specifies the header.

### §4.6 The WIS envelope (`SOAP_ENVELOPE_FORMAT=wis`)

Upstream `soap_sender` after `0f0164b` (the WDS → WIS flow).

§4.6.1 `soap-sender` (the queue variant) reads `SOAP_ENVELOPE_FORMAT`,
trimmed and lower-cased. `wis` selects the envelope below; **any other value,
including unset, selects §4.2's**, as the Python does. The subscription
variant does not read it, as the Python's does not.

§4.6.2 The WIS envelope carries HL7 v2 **XML**, not ER7. The ER7 is
converted with `ihb_validation::convert::er7_to_xml` and the result is
escaped for `&`, `<`, `>` **and `"`** (unlike §4.2) and placed as text:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<ns1:Envelope xmlns:ns1="http://Cypris.Nhs.Wales.Uk/CaptureFromFiorona/Input">
  <ns1:Body>
    <ns3:CaptureFromFiorona xmlns:ns3="http://Cypris.Nhs.Wales.Uk/">
      <ns3:inputString>{escaped v2 XML}</ns3:inputString>
    </ns3:CaptureFromFiorona>
  </ns1:Body>
</ns1:Envelope>
```

§4.6.3 A message that cannot be converted is a delivery failure of the
`Unexpected` class (§2.6): abandoned and retried, not posted. The error text
carries the converter's structural reason, never message content.

§4.6.4 §4.3 and §4.4 apply unchanged. Tests: the Python's four envelope
assertions (element names, escaped namespace `urn:hl7-org:v2xml`, the
`ADT_A05` structure, well-formed XML), `"` escaped, the default format
unchanged, and an unconvertible message not posted.

---

## §5 The subscription variant

§5.1 Identical to §2–§4 except that the receiver is a topic subscription
(`INGRESS_TOPIC_NAME`, `INGRESS_SUBSCRIPTION_NAME`) and the session is
optional.

§5.2 `hl7-subscription-sender` does **not** write to the message store: the
message was already stored by whichever component published to the topic.
It has no `MESSAGE_STORE_QUEUE_NAME`.

§5.3 Everything a receiver labels with its queue name uses
`"{topic}/{subscription}"`, per
[`03-message-bus.md`](03-message-bus.md) §2.2.

---

## §6 Tests

§6.1 Unit: §2.2's first-attempt detection, including missing and
unparseable delivery counts.

§6.2 Unit: §2.3's batch sizing, at each boundary — unthrottled, a fast
interval, and an interval so slow that only one message fits.

§6.3 Unit: §2.4's throttle, with a virtual clock: the first send is
immediate, a second within the interval waits exactly the remainder, and a
send after a long idle does not burst (§2.4.2).

§6.4 Unit: §3.7 and §4.4's evaluation tables, every row.

§6.5 Unit: §4.2's envelope, including the escaping and, crucially, that `"`
and `'` are *not* escaped.

§6.6 Integration: `hl7-sender` against a real MLLP listener that answers
`AA`, `AE`, a non-ACK message, and nothing at all — asserting the verdict
and the disposition in each case.

§6.7 Integration: §3.4's single retry, by having the listener drop the first
connection and answer on the second.

§6.8 Integration: `soap-sender` against `wiremock` covering every row of
§4.4.

---

## §7 Database sender (`db-sender`)

Reference implementation: `Integration-Hub-Beta/senders/db_sender/`
(upstream `#422`, `#433`, after `9e797ac`; the local service was renamed
`wrrs-stmsg-dbsender` in `#433`). Implemented by `crates/db-sender`.

§7.1 **Shape.** A generic outbound service: one topic subscription in, one
SQL Server stored procedure call out, per message. It is **not** the §2
pipeline: no throttle, no message-store write, no acknowledgement to
evaluate. The first deployment is RISP → WRRS (`dbo.prQueueMessage` in
database `DRS`); another database flow is a new environment file, not new
code. §1.1 holds: a message is completed only after the procedure
**committed**.

§7.2 **Per message**, in order:

```
1  decode the body as UTF-8 (invalid: dead-letter, reason InvalidMessageEncoding)
2  read the incoming application_properties for the correlation id
3  extract the MSH only, for logging (§7.5)
4  MESSAGE_RECEIVED event, text "Message received for execution of {proc}"
5  BEGIN; EXEC {proc} @Message, @IsPriority; COMMIT
6  messages_sent metric, best effort
7  MESSAGE_PROCESSED event, best effort: the procedure has committed, so a
   logging failure MUST NOT cause redelivery
8  complete the message
```

§7.3 **Failure classes** (`db_client.py`):

| Class | Meaning | Action |
| --- | --- | --- |
| Transient | connection or login transport failure, timeout, deadlock (`1205`), `233`, `10054`, `10060`, or a message containing `timeout`, `deadlock` or `communication link failure` (case-insensitive) | log at warn, **abandon** the message and the rest of the batch (`03-message-bus.md` §5.2.2); no `MESSAGE_FAILED` |
| Fatal | any other error the server reported (procedure missing `2812`, permission, conversion, login `18456`) | `MESSAGE_FAILED` event, **dead-letter** with reason `StoredProcedureExecutionFailed` and description `SQLState: {state}, Code: {code}: {message}`; the batch continues |
| Unexpected | anything else | log, abandon |

TDS does not carry a SQLSTATE, so `{state}` is `UNKNOWN`, which is the
Python's own default. The connection is discarded after **any** failure
(`12-message-store.md` §5.2).

§7.4 **Server messages are sanitised before they are logged, evented or
dead-lettered.** SQL Server echoes parameter values into some messages;
those would carry a patient identifier into a dead-letter description.
Three patterns are replaced, case-insensitively: the parenthesised list
after `The duplicate key value is` becomes `(***)`; the quoted text after
`Truncated value:` becomes `'***'`; the quoted text after `converting [the]
[type] value` becomes `'***'`. Quotes may contain a doubled `''`.

§7.5 **Only the MSH is ever logged** (Caldicott principle 3). The extractor
accepts ER7 (the first line starting `MSH`) or HL7 v2 XML (the `MSH` element,
at any depth, namespaced or not) and returns the MSH text, `MSH-10` and
`MSH-9`. Empty input, a body with no MSH and a malformed XML body return the
placeholders `[Empty message body]`, `[No MSH segment identified]` and
`[XML MSH extraction failed - structure malformed]`, with `UNKNOWN` ids.
The MSH text goes only to the audit events (which redact, `04-observability.md`
§2). Deviation from the Python: **tracing log lines carry the control id and
message type, never the MSH text.**

§7.6 **Configuration**, in addition to the shared variables
(`SERVICE_BUS_CONNECTION_STRING` / `SERVICE_BUS_NAMESPACE`,
`HEALTH_CHECK_HOST`, `HEALTH_CHECK_PORT`, `HL7_LOG_REDACTION_ENABLED`):

| Variable | Default | Meaning |
| --- | --- | --- |
| `INGRESS_TOPIC_NAME`, `INGRESS_SUBSCRIPTION_NAME` | — | **Required** |
| `INGRESS_SESSION_ID` | *(unset)* | Session; unset is a plain subscription receiver |
| `DB_SERVER`, `DB_PORT`, `DB_NAME` | `localhost`, `1433`, `DRS` | |
| `DB_STORED_PROCEDURE` | `dbo.prQueueMessage` | One to three `.`-separated identifiers (`[A-Za-z0-9_@#$]`). Anything else is a startup failure: the name is spliced into `EXEC` |
| `DB_USERNAME`, `DB_PASSWORD` | `sa`/`MSSQL_SA_PASSWORD` fallback | Both or neither |
| `DB_ENCRYPT`, `DB_TRUST_SERVER_CERTIFICATE` | `Yes`, `No` | `Yes`/`No` only |
| `DB_CONNECTION_TIMEOUT`, `DB_COMMAND_TIMEOUT` | `30`, `60` | Seconds, positive |
| `MANAGED_IDENTITY_CLIENT_ID` | *(unset)* | |
| `DEFAULT_IS_PRIORITY` | `0` | Non-zero sets `@IsPriority` |
| `MAX_BATCH_SIZE` | `10` | |
| `WORKFLOW_ID`, `MICROSERVICE_ID` | `risp-to-wrrs`, `wrrs_stmsg_dbsender` | |
| `HEALTH_BOARD`, `PEER_SERVICE` | `SHARED`, `WRRS` | |

Deviations, both from `12-message-store.md` §5.8: this build cannot encrypt
to SQL, so `DB_ENCRYPT=Yes` (the default) is a **startup failure** naming
§5.8, not a plaintext connection; and managed-identity authentication (no
username and password) is likewise a startup failure until it is
implemented. The connection is lazy and long-lived (§5.1).

§7.7 Tests: the Python's configuration, MSH-extractor and sanitiser cases;
the application cases against a fake procedure runner (success calls once
with the exact body; fatal dead-letters with the sanitised description and a
`MESSAGE_FAILED`; transient abandons with no `MESSAGE_FAILED`; unexpected
abandons; a body that is not UTF-8 dead-letters without a call; patient
data absent from every dead-letter description). The Python's "a failing
`MESSAGE_PROCESSED` still completes" case has no counterpart: the event
logger cannot fail here, and step 7 follows the commit; and §7.3's classification table.
**Not run here:** a test against a real SQL Server, for want of one.
