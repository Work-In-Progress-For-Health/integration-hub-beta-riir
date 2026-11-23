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

§4.4.1a **Checked arithmetic** (`rust-checked-arithmetic`): `soap-sender` denies
`arithmetic_side_effects`, `as_conversions`, `indexing_slicing`, `string_slice`,
`integer_division` and `unreachable`. `status_element` reads the `Status`
element on remainders, not byte offsets, with the same results as before on
400,000 generated bodies.

§4.4.2 At most 500 characters of a faulted body, and 200 of an unparseable
one, are logged. Response bodies may echo message content, so what is logged
MUST pass through redaction.

§4.4.3 **Checked arithmetic** (`rust-checked-arithmetic`): `fhir-sender` denies
`arithmetic_side_effects`, `as_conversions`, `indexing_slicing`, `string_slice`,
`integer_division` and `unreachable`. An attempt number that cannot be advanced
ends the attempts (the delivery fails as "failed after N attempt(s)"); the
back-off is `base * 2^(attempt - 1)` computed as `2^attempt / 2`, so it has no
subtraction to underflow; `join_url` finds the path after the authority
without matching the lowercased origin as a prefix (the same results as before
on 200,000 generated URL pairs and the cases with an uppercase host).

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

§7.3a **A non-zero return code is a failure** (deviation from upstream,
recorded in `docs/upstream-sync-2026-10.md`). Upstream's `db_sender` ignores
the procedure's return code; its own `mock_db_receiver` (#446) documents that
the real WRRS `dbo.prQueueMessage` rejects a message with `RETURN 55555` and
raises **no SQL error**, so upstream completes a message that was never
stored. Here the statement is `DECLARE @rc INT; EXEC @rc = {procedure} …;
SELECT @rc;` and the call fails unless the final result set carries `0`:

- a non-zero code rolls the transaction back and is **Fatal** (the content
  was refused; a retry is refused again): dead-letter, reason
  `StoredProcedureExecutionFailed`, description
  `SQLState: UNKNOWN, Code: {abs(code)}: the stored procedure returned code
  {code}; nothing was stored`;
- a missing return code is Fatal too: no evidence of storage is never
  evidence of success;
- the description carries the **code only**. The procedure has no message
  text, and none is taken from the message, so no patient data can reach the
  dead-letter queue by this route (§7.4).

*Assumption, unverified against production:* every non-zero code means "not
stored". If the production procedure returns non-zero for a harmless case,
those messages are dead-lettered rather than lost and can be replayed; narrow
this clause to the codes that mean rejection. Tested on SQL Edge with the
`SCENARIO-REJECT`, `SCENARIO-ERROR` and `SCENARIO-TIMEOUT` tokens in
`local/sql/init-drs-db.sql` (`crates/db-sender/tests/against_sql_server.rs`).

§7.4 **Server messages are sanitised before they are logged, evented or
dead-lettered.** SQL Server echoes parameter values into some messages;
those would carry a patient identifier into a dead-letter description.
Three patterns are replaced, case-insensitively: the parenthesised list
after `The duplicate key value is` becomes `(***)`; the quoted text after
`Truncated value:` becomes `'***'`; the quoted text after `converting [the]
[type] value` becomes `'***'`. Quotes may contain a doubled `''`.
**The redactor fails closed:** if it cannot prove that a cut lands inside the
message (a split that is out of range or off a character boundary, which no
input reaches today), the whole message becomes `***`, never the unredacted
rest (RCA-P-9). The scanners hold remainders of the text, not byte offsets;
they were checked against the offset-based version on 400,000 generated
messages, and the two agreed on every one.

§7.5 **Only the MSH is ever logged** (Caldicott principle 3). The extractor
accepts ER7 (the first line starting `MSH`) or HL7 v2 XML (the `MSH` element,
at any depth, namespaced or not) and returns the MSH text, `MSH-10` and
`MSH-9`. Empty input, a body with no MSH and a malformed XML body return the
placeholders `[Empty message body]`, `[No MSH segment identified]` and
`[XML MSH extraction failed - structure malformed]`, with `UNKNOWN` ids.
The MSH text goes only to the audit events (which redact, `04-observability.md`
§2). Deviation from the Python: **tracing log lines carry the control id and
message type, never the MSH text.** Second deviation: upstream's regex salvage
of a closed `<MSH>..</MSH>` from XML that does not parse is **not** ported.
Malformed XML is invalid data, and a salvage would be a fallback for it
(`rust-checked-arithmetic` RCA-DECL-6, RCA-IN-6): it returns the placeholder, so
the control id and trigger are `UNKNOWN` and the message dead-letters on its
missing identifiers as §7.8 says. An `MSH` line with no character after the
name has no separator and is not an `MSH` (no `|` is assumed).

§7.6 **Configuration**, in addition to the shared variables
(`SERVICE_BUS_CONNECTION_STRING` / `SERVICE_BUS_NAMESPACE`,
`HEALTH_CHECK_HOST`, `HEALTH_CHECK_PORT`, `HL7_LOG_REDACTION_ENABLED`):

| Variable | Default | Meaning |
| --- | --- | --- |
| `INGRESS_TOPIC_NAME`, `INGRESS_SUBSCRIPTION_NAME` | — | **Required** |
| `INGRESS_SESSION_ID` | *(unset)* | Session; unset is a plain subscription receiver |
| `DB_SERVER`, `DB_PORT`, `DB_NAME` | `localhost`, `1433`, `DRS` (`Clinical_Results` for `trrr`) | |
| `DB_STORED_PROCEDURE` | `dbo.prQueueMessage`; `dbo.prQueueIdUpdate` for `queue_id_update`; `dbo.spUpdateIdentifier` for `update_identifier`; `dbo.TRRRIDUpdate` or `dbo.TRRRIDMerge` for `trrr` | One to three `.`-separated identifiers (`[A-Za-z0-9_@#$]`). Anything else is a startup failure: the name is spliced into `EXEC` |
| `DB_USERNAME`, `DB_PASSWORD` | `sa`/`MSSQL_SA_PASSWORD` fallback | Both or neither |
| `DB_ENCRYPT`, `DB_TRUST_SERVER_CERTIFICATE` | `Yes`, `No` | `Yes`/`No` only |
| `DB_CONNECTION_TIMEOUT`, `DB_COMMAND_TIMEOUT` | `30`, `60` | Seconds, positive |
| `MANAGED_IDENTITY_CLIENT_ID` | *(unset)* | |
| `DB_PROCEDURE_TYPE` | `queue_message` | `queue_message`, `queue_id_update` (§7.8), `update_identifier` (§7.9) or `trrr` (§7.10), case-insensitive. **Anything else is a startup failure**; upstream treats it as `queue_message`, so a typo runs the wrong procedure and fails at the first message |
| `SOURCE_SYSTEM` | `328` | The authority an `A31` is taken to come from when neither the `SourceSystem` property nor `MSH-3.1` says (§7.8) |
| `DEFAULT_IS_PRIORITY` | `0` | Non-zero sets `@IsPriority` |
| `MAX_BATCH_SIZE` | `10` | |
| `TRRR_MAPPING_TYPE` | `NHS_ID_UPDATE` | `trrr` only (§7.10): `NHS_ID_UPDATE` or `MERGE`, case-insensitive; anything else is a startup failure |
| `WORKFLOW_ID`, `MICROSERVICE_ID`, `PEER_SERVICE` | `risp-to-wrrs`, `wrrs_stmsg_dbsender`, `WRRS`; for `trrr`, `lims-to-trrr`, `db_sender_trrr` (`db_sender_trrr_merge` for `MERGE`), `TRRR` | |
| `HEALTH_BOARD`, `PEER_SERVICE` | `SHARED`, `WRRS` | |

`DB_ENCRYPT=Yes` (the default) makes TLS mandatory
(`12-message-store.md` §5.8): a server that will not encrypt, or whose
certificate does not verify, is a connection failure. `DB_TRUST_SERVER_CERTIFICATE=Yes`
skips verification and is off by default. With neither `DB_USERNAME` nor
`DB_PASSWORD` the sender uses its managed identity (`MANAGED_IDENTITY_CLIENT_ID`
selects a user-assigned one); `12-message-store.md` §5.4. The connection is
lazy and long-lived (§5.1).

§7.8 **Queue ID Update** (`DB_PROCEDURE_TYPE=queue_id_update`; upstream
`#455`, `handlers/queue_idupd_handler.py`). The same service, a second
procedure: instead of passing the message whole, it reads patient identifiers
out of an `A31` or `A40` and calls `dbo.prQueueIdUpdate` with six
parameters. The first deployment is LIMS → WRRS (`wrrs_idupd_dbsender`).
Implemented by `ids.rs` (reading), `idupdate.rs` (choosing the parameters) and
`SqlRunner::execute_id_update`.

| Trigger | `@Provider…` | `@Old…` | `@New…` |
| --- | --- | --- | --- |
| `A31` | the `PID-3` identifier whose authority is the **source system**, and that authority | `NULL`, `NULL` | the `PID-3` identifier with authority `NHS`, and `NHS` |
| `A40` | `NULL`, and the old authority | `MRG-1`'s identifier and authority | the `PID-3` identifier of the **same authority as `MRG-1`**, and that authority |
| any other | dead-letter `UnsupportedTriggerEvent` | | |

The **source system** of an `A31` is, in order: the Service Bus
`source_system` or `SourceSystem` property (trimmed, non-blank), `MSH-3.1`,
`SOURCE_SYSTEM`. The trigger is `MSH-9.2` (else `MSH-9.1`), read from ER7 or
HL7 v2 XML.

A `CX` is read in two layouts, because the estate produces both: standard
`ID^^^AUTH^TYPE` (authority is component 4) and compacted `ID^AUTH` or
`ID^AUTH^TYPE` (component 2). Only the first sub-component of an authority is
used (`NHS&x&y` is `NHS`); authorities compare case-insensitively; `PID-3` and
`MRG-1` are searched repetition by repetition, in order.

Dead letters, decided **before any database call**, each logged as
`MESSAGE_FAILED` (`Message rejected for {proc}: {reason} - {description}`):

| Reason | When | Description |
| --- | --- | --- |
| `UnsupportedTriggerEvent` | not `A31`/`A40` | `Trigger event {t} is not supported for Queue ID Update` |
| `MissingOldIdentifier` | `A40`, no `MRG-1` id | `Missing old identifier` |
| `MissingOldAssigningAuthority` | `A40`, no `MRG-1` authority | `Missing old assigning authority` |
| `MissingNewIdentifier` | `A40`, no `PID-3` id of that authority | `Missing new identifier` |
| `MissingLimsIdentifier` | `A31`, no id of the source system | `Missing identifier with authority {source}` |
| `MissingNhsIdentifier` | `A31`, no `NHS` id | `Missing NHS identifier` |
| `IdentifierTooLong` | §7.8.1 | `The {field} is longer than {n} characters` |

**No description contains an identifier**: it names the authority or the
field, never the value (§7.5). A database failure is classified as §7.3 says,
unchanged.

§7.8.1 **A value wider than its column is refused.** The procedure's
parameters are `varchar(20)` (identifiers) and `varchar(10)` (authorities),
and **SQL Server does not reject a longer string: it truncates it silently
and the `INSERT` succeeds.** For a patient identifier that stores a different
identifier. Found by `tests/against_sql_server.rs` (which pins the server
behaviour); upstream has the same hole, since `pyodbc` binds the same way.
`plan` therefore returns `IdentifierTooLong` for any value over its column,
and the runner is never given one.

§7.8.2 **Deviations from upstream.** (a) An unknown `DB_PROCEDURE_TYPE` is a
startup failure (§7.6). (b) XML that does not parse yields no identifiers, so
the message dead-letters as missing one; upstream falls back to regular
expressions over the broken text. The producers here (`rest-server`,
`hl7-soap-server`) emit ER7. (c) §7.8.1.

§7.9 **WCRS identifier update** (`DB_PROCEDURE_TYPE=update_identifier`;
upstream `#462`, `handlers/update_identifier_handler.py`). A third
procedure for the same service: an `ADT^A40` from CAV PMS becomes three
**string** parameters of `dbo.spUpdateIdentifier` (strings, so a leading zero
survives): `@OldIdentifier` (`MRG-1.1`), `@OldIdentifierAssigningAuthority`
(`MRG-1.4.1`, as it came) and `@NewIdentifier` (the `PID-3` identifier whose
authority matches `MRG-1.4.1`). Implemented by `idupdate::plan_update_identifier`,
`ids::count_segments` and `SqlRunner::execute_update_identifier`; it reads
identifiers the way §7.8 does.

Dead letters, before any database call: `UnsupportedTriggerEvent` (not
`A40`), **`MultiplePatientGroups`** (more than one `MRG` or `PID`),
`MissingOldIdentifier`, `MissingOldAssigningAuthority`, `MissingNewIdentifier`.
The multi-patient refusal is the clause that matters: the `MRG` is read from
the first segment but `PID-3` is searched in every `PID`, so a message with
two patients could pair one patient's old identifier with another's new one
and merge the wrong people. Upstream added it in the same PR, after review.
XML that does not parse is invalid data and is **not** guessed at: upstream's
text-scan fallback in `count_segments` is not ported, because it would name a
fallback for malformed input (RCA-DECL-6). A broken body counts as none and the
identifier readers find nothing in it either, so it dead-letters as
`MissingOldIdentifier` (as §7.8 does) and a broken two-patient message is still
never delivered. **Not
checked:** the widths of `spUpdateIdentifier`'s parameters (§7.8.1 does for
`prQueueIdUpdate`); no WCRS database was available, so a value wider than its
column may be silently truncated. Find the widths before first deployment.

§7.10 **TRRR identifier update** (`DB_PROCEDURE_TYPE=trrr`; upstream `#458`,
`senders/db_sender_trrr`). Upstream shipped this as a second image; here it
is a fourth procedure of the same one (the design of §7: *the next database
flow is another environment file, not another crate*). One instance applies
the one mapping `TRRR_MAPPING_TYPE` names and **never infers it from the
message**; the subscription does the routing, and the sender maps whatever
arrives. Implemented by `trrr.rs`, which is deliberately not `ids.rs`: TRRR
reads an authority as `CX.4.1` only (never the compacted `ID^AUTH` layout),
compares authorities **exactly** (§7.8 ignores case), and never trims an
identifier.

| `TRRR_MAPPING_TYPE` | Subscription (locally) | Accepts | Procedure | Parameters |
| --- | --- | --- | --- | --- |
| `NHS_ID_UPDATE` | `…-lims-trrr-db-sender-sub`: `A31`, `A40`, source `328` | `A31`, `A40` | `dbo.TRRRIDUpdate` | `LIMSIdentifier`, `NewIdentifier`, `NewIdentifierAssigningAuthority` (`NHS`) |
| `MERGE` | `…-lims-trrr-merge-sender-sub`: every `A40` | `A40` | `dbo.TRRRIDMerge` | `OldIdentifier`, `OldIdentifierAssigningAuthority`, `NewIdentifier`, `NewIdentifierAssigningAuthority` |

For `NHS_ID_UPDATE` the LIMS identifier is the `PID-3` repetition whose
authority is **`328`** for an `A31`, and **`MSH-3.1`** for an `A40` (so one
mapping serves several sending systems); the NHS number is the repetition with
authority `NHS`. `MERGE` pairs `MRG-1` with the `PID-3` of the same authority,
as §7.9. **An `A40` is delivered to both subscriptions and so makes two TRRR
calls**; an `A40` with no `NHS` identifier is dead-lettered by
`NHS_ID_UPDATE` and still merged.

Every refusal is dead-lettered with reason **`MappingFailed`**, before any
call, and the description is one of: `Empty message body`, `Malformed XML
message`, `Unsupported trigger event: {MSH-9.2 or UNKNOWN}`, `Multiple patient
groups are not supported` (merge only), `Missing old identifier`, `Missing old
assigning authority`, `Missing new identifier`, `Missing sending LIMS
authority`, `Missing LIMS identifier`, `Missing NHS identifier`. None contains
an identifier. Database failures are §7.3's, unchanged.

**Deviation.** Upstream defaults to `DB_SERVER=TES06SRVSQLNOD1`,
`DB_USERNAME=TRRR`, `DB_PASSWORD=trrr`. Those are not defaults here: a
credential in source is a defect, and with neither set the sender uses its
managed identity (§7.6). The local environment files name `sa` and take the
password from `.secrets`. **Not run:** nothing here has met a TRRR or
`Clinical_Results` database other than the local mock; the compose flow
(`lims-to-trrr`) has not been brought up.

§7.7 Tests: the Python's configuration, MSH-extractor and sanitiser cases;
the application cases against a fake procedure runner (success calls once
with the exact body; fatal dead-letters with the sanitised description and a
`MESSAGE_FAILED`; transient abandons with no `MESSAGE_FAILED`; unexpected
abandons; a body that is not UTF-8 dead-letters without a call; patient
data absent from every dead-letter description). The Python's "a failing
`MESSAGE_PROCESSED` still completes" case has no counterpart: the event
logger cannot fail here, and step 7 follows the commit; and §7.3's classification table.
**Run against SQL Edge**, behind the `sqlserver` feature
(`tests/against_sql_server.rs`): both procedures, the `NULL`s of §7.8, a fatal
refusal followed by a reconnect, and §7.8.1's truncation. The first of these
was the first time either path had met a real server.

---

## §8 FHIR sender (`fhir-sender`)

Reference implementation: `Integration-Hub-Beta/senders/fhir_sender/`
(upstream `#441`, `#442`, `#448`). Implemented by `crates/fhir-sender`. The
WPAS → PROMS flow's last hop: the Bundle `xml-fhir-proms-transformer`
produces ([`16-proms-transformer.md`](16-proms-transformer.md)) goes to
Promptly.

§8.1 **Shape.** One session on one queue in, one `$process-message`
delivery to Promptly out, per message. Like §7 it is **not** the §2
pipeline: no throttle, no message-store write, no HL7 acknowledgement. §1.1
holds: a message is completed only when Promptly reported a non-error
`OperationOutcome`.

The WPAS-Promptly IG's asynchronous pattern:

```
POST  {FHIR_ENDPOINT_URL}          Content-Type: application/fhir+json
  202 Accepted + Content-Location  (or 200 + OperationOutcome: done)
GET   {Content-Location}           until 200 + OperationOutcome
```

§8.2 **Outcomes.** One class per failure, each with one Service Bus action.

| Result | Class | Action |
| --- | --- | --- |
| `200`, or `202` then `200`, with an `OperationOutcome` having no `error`/`fatal` issue | | complete |
| `400`, `404`, `409`, any other `4xx` | `Permanent` | **dead-letter**, reason `PromptlyRejected` |
| `200` with an `error` or `fatal` issue | `Permanent` | dead-letter, reason `PromptlyRejected` |
| `401`, with OAuth2: one retry with a fresh token | | |
| `401` otherwise, `403`, a token endpoint that rejects the client | `Authorization` | log `ALERT`, **abandon** (redrive after the credentials are fixed) |
| `429` | | wait `Retry-After` (capped by `FHIR_MAX_RETRY_AFTER_SECONDS`), else the backoff; counts as an attempt |
| `5xx`, network error, timeout | | exponential backoff from `FHIR_BACKOFF_SECONDS`, up to `FHIR_MAX_ATTEMPTS`; then `Transient`, **abandon** |
| `202` without `Content-Location`; a `200` that is not an `OperationOutcome` | `Transient` | abandon |
| polling past `FHIR_POLL_TIMEOUT_SECONDS` | `Transient` | abandon |
| body is not a `Bundle` of `type=message`, or not UTF-8 | | dead-letter without a request, reason `InvalidFhirBundle` |

A dead-letter description is at most 1000 characters; a detail taken from
an `OperationOutcome` is at most 500 (`severity/code: details.text`, else
`diagnostics`, joined by `; `).

§8.3 **Authentication**, one mechanism per environment (`FHIR_AUTH_MODE`,
default `api_key`):

- `api_key`: `X-API-Key: {FHIR_API_KEY}`. A blank key is refused.
- `oauth2`: client-credentials grant against `FHIR_OAUTH_TOKEN_URL` (`https`
  only), sent as a form (`grant_type`, `client_id`, `client_secret`,
  `scope`), `Authorization: Bearer`. The token is cached and refreshed 60
  seconds before `expires_in` (default 3600). A `400`, `401` or `403` from
  the token endpoint is an `Authorization` failure; any other non-`200`, an
  unreachable endpoint or an unreadable response is `Transient`. The
  response body is never echoed.

Credentials are never printed: they are `Secret`s and appear in no `Debug`
output.

§8.4 **Safeguards.** (a) The endpoint MUST be `https`; plain `http` only
with `FHIR_ALLOW_INSECURE_HTTP=true` (exactly that word), for local mocks.
(b) Redirects are **never followed**: a `3xx` is a `Permanent` rejection.
(c) The status URL (`Content-Location`, absolute, scheme-relative,
path-absolute or relative) MUST have the endpoint's scheme and host and
port, because the credentials are sent to it; otherwise `Permanent`
(`Status URL origin differs from the configured endpoint`) **before** any
request is made to it. (d) A response is read to at most 1 MiB. (e) Every
request is bounded by `FHIR_TIMEOUT_SECONDS`, shortened to what remains of
the polling deadline.

§8.5 **Per message**, in order:

```
1  decode the body as UTF-8 (invalid: dead-letter, reason InvalidFhirBundle)
2  MESSAGE_RECEIVED event
3  the body is a Bundle with type "message" (else dead-letter, MESSAGE_FAILED)
4  deliver (§8.2): POST, then poll
5  Complete:   messages_sent metric, MESSAGE_PROCESSED event, complete
   Permanent:  MESSAGE_FAILED, dead-letter
   Authorization / Transient: MESSAGE_FAILED, abandon the message and the rest of the batch
```

§8.6 **No Bundle reaches a log or an event.** A message Bundle is a patient
record. Upstream passes the whole body to `MESSAGE_RECEIVED`,
`MESSAGE_PROCESSED` and `MESSAGE_FAILED`; this port records
`FHIR Bundle, {n} bytes (content not logged)` instead, which is a
deliberate deviation (`AGENTS.md`: patient data never reaches a log). The
`OperationOutcome` detail that §8.2 puts in `MESSAGE_FAILED` and in a dead
letter is Promptly's own text and could in principle echo a value from the
Bundle; it is kept because it is what an operator needs, and bounded to 500
characters. **Open:** confirm with Promptly that diagnostics never echo
patient values; if they can, redact them as §7.4 does for SQL server messages.

§8.7 **Configuration.**

| Variable | Default | Meaning |
| --- | --- | --- |
| `FHIR_ENDPOINT_URL` | — | **Required.** The full `$process-message` URL |
| `FHIR_AUTH_MODE` | `api_key` | `api_key` or `oauth2` (case-insensitive); anything else is a startup failure |
| `FHIR_API_KEY` | — | Required for `api_key` |
| `FHIR_OAUTH_TOKEN_URL`, `FHIR_OAUTH_CLIENT_ID`, `FHIR_OAUTH_CLIENT_SECRET`, `FHIR_OAUTH_SCOPE` | — | Required for `oauth2` |
| `FHIR_TIMEOUT_SECONDS` | `30` | Per request; positive |
| `FHIR_MAX_ATTEMPTS` | `3` | Per request, for `429`/`5xx`/network; positive |
| `FHIR_BACKOFF_SECONDS` | `2` | First backoff; doubles each attempt |
| `FHIR_MAX_RETRY_AFTER_SECONDS` | `60` | Cap on a `Retry-After` wait |
| `FHIR_POLL_INTERVAL_SECONDS` | `5` | Poll delay when none is given |
| `FHIR_POLL_TIMEOUT_SECONDS` | `120` | Longest wait for processing to finish |
| `FHIR_ALLOW_INSECURE_HTTP` | `false` | Local mocks only |
| `INGRESS_QUEUE_NAME` | — | **Required** |
| `INGRESS_SESSION_ID` | — | **Required.** The queue MUST be session-enabled: only a session receiver renews its lock (`03-message-bus.md` §5.1), and a status poll can outlast the queue's lock duration |
| `MAX_BATCH_SIZE` | `1` | Keep small: each message may poll for the full timeout, and a renewed lock is capped at five minutes |
| `SERVICE_BUS_CONNECTION_STRING`, `SERVICE_BUS_NAMESPACE` | | One of |
| `WORKFLOW_ID`, `MICROSERVICE_ID`, `HEALTH_BOARD`, `PEER_SERVICE` | — | **Required** |
| `HEALTH_CHECK_HOST`, `HEALTH_CHECK_PORT` | | As the other services |
| `LOG_LEVEL` | `ERROR` | As upstream; the local env file sets `INFO` |

Every problem is reported together (`02-configuration.md` §1.4). Numbers
that are unparseable, zero, negative or not finite are startup failures,
as upstream's `_read_positive_number`.

§8.8 **Tests.** The policy runs against a scripted transport and a clock
that advances when it sleeps, so no test waits: every row of §8.2; the
`Retry-After` cap and the backoff sequence (2 s, 4 s, none after the last
attempt); the 401 refresh happening once; the deadline both ways (`Processing
not complete` and `Polling deadline reached`); every origin variant of §8.4(c)
refused with no second request; the token form, its caching and its refresh
margin; secrets absent from `Debug`; the configuration table; the processor
dispositions; and the batch stopping at an abandon but not at a dead letter.
`tests/wire.rs` runs the real transport against a local socket: the headers
that arrive, a relative `Content-Location` resolved, a `302` left unfollowed,
and a refused connection ending `Transient`. **Not run here:** against
Promptly, or against a TLS peer. **Known, upstream:** the IG defines no
idempotency mechanism, so a retry after an unconfirmed `POST`, or after a
polling timeout, can resubmit a Bundle; so can redelivery after a
completion that failed to settle.
