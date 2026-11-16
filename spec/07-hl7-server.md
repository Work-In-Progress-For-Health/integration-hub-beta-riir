# 07 — HL7 MLLP ingress server

The generic MLLP receiver: one deployment per flow. Implemented by
`crates/hl7-server`.

Reference implementation: `Integration-Hub-Beta/hl7_server/`.

---

## §1 Responsibilities

Receive an HL7 v2 message over MLLP, validate it, persist it, forward it to
Service Bus, and acknowledge it — in that order, with the ordering
constraints of §5.

The service is generic. Every per-flow difference is configuration:
`HL7_VERSION`, `SENDING_APP`, `HL7_VALIDATION_FLOW`, the egress
queue or topic, and the session.

---

## §2 Transport

§2.1 The listener binds `HOST:PORT` and speaks MLLP: `<VT> payload <FS><CR>`.
Framing is `hl7-2-mllp`.

§2.2 A frame is decoded as UTF-8. Invalid UTF-8 is a rejection; a generic `AE`
NACK (§4.3.2) is sent, the connection is closed and the event is logged as
`MESSAGE_FAILED`.

§2.3 **Frame size.** A frame that would exceed `MAX_MESSAGE_SIZE_BYTES` MUST
cause a generic `AE` NACK (§4.3.2) to be sent and the connection to close.
Before closing, the service MUST log at error

```
Message size ({n} bytes) exceeds maximum allowed size ({limit} bytes). Connection will be closed.
```

and MUST emit a `MESSAGE_FAILED` event whose content is the **first 1000
bytes** of what was accumulated, decoded lossily, with the validation result
`Message size limit exceeded`. The 1000-byte cap is normative
(`size_limited_mllp_request_handler.py :: MAX_PARTIAL_MESSAGE_LOG_SIZE`);
that content passes through redaction like any other.

§2.4 **Start block.** A connection whose first byte is not `<VT>` is closed
without a reply and without an event.

§2.5 **Pipelined frames.** Several frames arriving in one read MUST each get
their own reply. This differs from the Python, which read until its buffer
*ended* with `<FS><CR>` and so answered two frames once. See
[`plan.md` §9.1](../plan.md#9-deliberate-deviations-from-the-python).

§2.6 The socket read timeout is `MLLP_TIMEOUT_SECONDS`, **30 seconds** by
default (`size_limited_mllp_server.py`; it was `timeout=10` until upstream
`f7f9c5b`, 6 Nov 2026, which made it configurable because connections are now
persistent and a sender may idle between messages). Absent, unparseable or
non-positive resolves to 30. An idle timeout between frames, and a timeout
in the middle of a frame, both close the connection; the second also logs a
`MESSAGE_FAILED` event carrying the first 1000 bytes
(`"Socket timeout while receiving message body"`, validation result
`"MLLP Timeout"`).

§2.6.1 **Persistent connections.** Upstream now keeps a connection open
across messages (`f7f9c5b`); before, it closed after each reply. This server
already did (§2.5), so nothing changes here beyond the timeout. Line
terminators between frames are skipped.

§2.6.2 **A frame that is not an HL7 message is answered, not dropped**
(upstream `47b66ae`, #436). Where the frame parses as no message (empty
segments, a leading `<CR>`, no `MSH`), the sender receives an `AE` NACK
whose `MSA-2` echoes the sender's own `MSH-10` when one can be read from the
first segment (at most 199 characters), and a fresh control id otherwise.
Previously the NACK always carried a fresh id, so a sender could not match
it to what it sent.

§2.7 An empty read (peer closed) ends the connection cleanly.

---

## §3 Validation

Three layers, in this order (`hl7_validator.py`, then `generic_handler.py`).

### §3.1 Version

§3.1.1 When `HL7_VERSION` is configured, `MSH-12` MUST equal it exactly.
Failure raises a validation error with the message
`Message has wrong version`.

§3.1.2 When it is not configured, no version check is performed.

### §3.2 Sending application

§3.2.1 When `SENDING_APP` is configured, `MSH-3` MUST be a member of the
comma-separated list, compared after trimming each element.

§3.2.2 Failure message, exactly:

```
Message sending application '{value}' is not in allowed authority codes.
```

### §3.3 Flow-specific field rules

Selected by `HL7_VALIDATION_FLOW`.

#### §3.3.1 `phw` — date of birth

`custom_validation/phw_validation.py`. `PID-7` is mandatory and MUST be
exactly 8 digits in `YYYYMMDD` form, parseable as a real date, with a year
of 1800 or later.

| Condition | Message |
| --- | --- |
| No `PID` segment, or `PID-7` blank | `PID.7 (Date of birth) is required for PHW.` |
| Not 8 digits | `PID.7 (Date of birth) must be a valid date in YYYYMMDD format for PHW.` |
| Not a real date | `PID.7 (Date of birth) must be a valid date in YYYYMMDD format for PHW.` |
| Year before 1800 | `PID.7 (Date of birth) - year of birth must be 1800 or later for PHW.` |

#### §3.3.2 `mpi` — outbound

`custom_validation/mpi_outbound_validation.py`.

| Condition | Message |
| --- | --- |
| `MSH-9.2` blank | `MSH.9.2 MessageType is missing from the MPI outbound message` |
| `MSH-9.2` not in `{A28, A31, A40}` | `Unsupported message type '{value}' for MPI outbound flow` |
| No `PID-2` assigning authority codes | `PID.2.4.1 UpdateSources is missing from the MPI outbound message` |

#### §3.3.3 `paris` — message type

`custom_validation/paris_validation.py` (upstream `9e797ac`). Paris sends
the same three triggers as MPI outbound, and the dedicated Paris sender topic
(§6.2) carries nothing else, so the trigger is checked at the door.

| Condition | Message |
| --- | --- |
| `MSH-9.2` blank | `MSH.9.2 MessageType is missing from the Paris message` |
| `MSH-9.2` not in `{A28, A31, A40}` | `Unsupported message type '{value}' for Paris flow` |

The rule has no `PID` requirement, unlike §3.3.2: Paris's `PID-2` is not a
routing key.

#### §3.3.3a `wds` — message type

`custom_validation/wds_validation.py`. The WDS flow carries `A28`, `A31` and,
since upstream `2060a7f` (#445), `A40`. This rule existed upstream before the
sync and was missed here (§3.3.4 said `wds` had none).

| Condition | Message |
| --- | --- |
| `MSH-9.2` blank | `MSH.9.2 MessageType is missing from the WDS message` |
| `MSH-9.2` not in `{A28, A31, A40}` | `Unsupported message type '{value}' for WDS flow` |

§3.3.4 Any other flow value performs no field-level rules here; its
validation is the schema validation of §3.4.

### §3.4 Flow schema validation

§3.4.1 Performed when `HL7_VALIDATION_FLOW` is set **and is not `mpi`**.

§3.4.2 It both validates and produces the v2.xml rendering, which becomes
the message store's `XmlPayload`
([`06-validation.md`](06-validation.md) §8.3).

§3.4.3 On success, a `VALIDATION_SUCCESS` event is emitted with

```
XML validation passed for flow '{flow}'
```

§3.4.4 On failure the service MUST, in order:

1. Log at error:
   `XML validation failed for flow '{flow}': {error} (CorrelationId: {id})`
2. Emit `VALIDATION_FAILED` with that text and the correlation id.
3. Emit `MESSAGE_FAILED` with that text and the validation result
   `XML schema validation failed`.
4. **Send the message to the message store with a null `XmlPayload`.** A
   message rejected at the hub is exactly the one an operator will want to
   inspect, so it is stored even though it is not forwarded.
5. Abort — no Service Bus send, no ACK.

### §3.5 Basic XML rendering

§3.5.1 When §3.4 did not run (no flow, or the `mpi` flow), the service
attempts a schema-unaware ER7 → v2.xml conversion for the message store.

§3.5.2 A failure here is **not fatal**. It is logged at error and emitted as
`VALIDATION_FAILED` with

```
Failed to generate XML payload for message store: {error} (CorrelationId: {id})
```

and processing continues with a null `XmlPayload`.

### §3.6 Standard-release validation

§3.6.1 Performed when `HL7_VALIDATION_STANDARD` is set, after §3.4/§3.5.

§3.6.2 Success emits `VALIDATION_SUCCESS` with
`Standard HL7 v{version} validation passed`.

§3.6.3 Failure logs at error, emits `VALIDATION_FAILED` and `MESSAGE_FAILED`
with validation result `Standard HL7 validation failed`, and aborts.

---

## §4 ACK construction

`hl7_ack_builder.py`. Every field below is normative; the source systems
parse these replies.

| Field | Value |
| --- | --- |
| `MSH-1` | `\|` |
| `MSH-2` | `^~\&` |
| `MSH-3` | Inbound `MSH-5` |
| `MSH-4` | Inbound `MSH-6` |
| `MSH-5` | Inbound `MSH-3` |
| `MSH-6` | Inbound `MSH-4` |
| `MSH-7` | Now, local time, `%Y%m%d%H%M%S` |
| `MSH-9.1` | `ACK` |
| `MSH-9.2` | Inbound `MSH-9.2` (the trigger event) |
| `MSH-9.3` | `ACK` |
| `MSH-10` | Inbound `MSH-10` |
| `MSH-11` | Inbound `MSH-11`, or `P` when the inbound omits it |
| `MSH-12` | Inbound `MSH-12` |
| `MSA-1` | `AA` |
| `MSA-2` | Inbound `MSH-10` |

§4.1 `MSH-11` is copied from the inbound message so a test message is never
acknowledged as production — HL7 v2.5 §2.9.2.2. The fallback to `P` exists
because `MSH-11` is required in an ACK. This differs from
`hl7-mock-receiver`, which always sends `P`; see
[`11-mock-receivers.md`](11-mock-receivers.md) §2.4.

§4.2 The ACK is MLLP-framed on the way out.

§4.3 The server sends `AA` only for a message it forwarded (§5.3). Every
other outcome gets an explicit negative acknowledgement, then the connection
closes. Upstream `62b8c34` replaced the earlier "no reply, the close is the
signal" behaviour; senders now classify `AE` as retry and `AR` as
dead-letter (`10-senders.md` §3.7.1), so a missing reply is no longer the
retry signal and MUST NOT be relied on.

| `MSA-1` | Failure |
| --- | --- |
| `AR` | Field-level validation (§3.1–§3.3), flow-schema validation (§3.4), standard-release validation (§3.6) |
| `AE` | Unparseable message (§7.2), unsupported type (§7.1), Service Bus send failure (§5.3), oversized frame (§2.3), frame that is not UTF-8 (§2.2), anything unexpected |

§4.3.1 **Parsed** failures echo the inbound header exactly as §4 does
(`MSH-3`..`MSH-6` swapped, `MSH-9.2`, `MSH-10`, `MSH-11` with the `P`
fallback), with `MSA-1` the code above, `MSA-2` the inbound `MSH-10` and
`MSA-3` the reason. `MSH-12` is the inbound value, or `2.5` when absent.

§4.3.2 **Unparsed** failures get a *generic* NACK: `MSH-3`..`MSH-6` and
`MSH-9.2` empty, `MSH-9` `ACK^^ACK`, `MSH-11` `P`, `MSH-12` `2.5`, `MSA-1`
`AE`, and a generated `MSH-10` (a UUID's first 20 hex characters, the
`MSH-10` limit) repeated in `MSA-2`.

§4.3.3 `MSA-3` has line breaks replaced by spaces and is capped at 199
characters (the `ST` limit). HL7 delimiters in the reason are escaped
(`\F\`, `\S\`, `\T\`, `\R\`, `\E\`); upstream does not, and an
unescaped `|` in a reason corrupts the segment for a strict parser.

§4.3.4 A failure to *write* the NACK is logged and the connection closes;
it never changes the event already emitted. Telemetry failure never
prevents the NACK (`MESSAGE_FAILED` emission is already best-effort here).

---

## §5 Ordering

The order of operations in `generic_handler.py :: reply` is normative
because each step's failure mode depends on it.

```
1  MESSAGE_RECEIVED event          } before anything can fail, so a message
2  messages_received metric +1     } that kills the process is still recorded
3  parse ER7
4  §3.1–§3.3 validation
5  VALIDATION_SUCCESS event
6  mint tracking metadata (§6)
7  §3.4 or §3.5 — produce XmlPayload
8  §3.6 standard validation
9  flow routing properties (§6.2)
10 send to message store          } non-blocking: failure is logged only
11 send to Service Bus            } blocking: failure aborts, no ACK
12 build and return the ACK
13 MESSAGE_PROCESSED event
```

§5.1 Step 10 precedes step 11 deliberately: there is a persisted copy before
the message is handed on.

§5.2 Step 10's failure MUST be caught and logged as
`Failed to send to message store: {error}` and MUST NOT abort.

§5.3 Step 11's failure MUST be logged as
`Failed to send message {control_id} to Service Bus: {error}` and MUST
abort. Acknowledging a message that was not forwarded is the one failure
this system must never produce.

§5.4 Step 9's failure MUST be caught, logged at warn as
`Failed to build flow-specific routing properties: {error}`, and MUST NOT
abort — routing properties are an optimisation, not a correctness
requirement.

---

## §6 Tracking metadata

§6.1 Minted per message, per
[`03-message-bus.md`](03-message-bus.md) §4: `MessageReceivedAt`,
`CorrelationId`, `WorkflowID`, `SourceSystem` (from `MSH-3`, or `""`).

§6.2 Routing properties (step 9), so that a Service Bus subscription filter
can match without opening the payload
([`03-message-bus.md`](03-message-bus.md) §4, upstream `d9cc216`, `8c1d012`,
`9e797ac`):

1. **Every flow except `mosaiq`** gets the *standard* routing set:
   `SendingApplication`, `TriggerEvent`, `MessageType`, `UpdateSources`,
   `AssigningAuthorities`.
2. `mpi` and `wds` then add `DateDeath` and `ReasonDeath` and overwrite
   `MessageType`, `UpdateSources` and `AssigningAuthorities` with the same
   values, so the result is the five MPI properties of `03` §4 **plus**
   `SendingApplication` and `TriggerEvent`.
3. `mosaiq` gets **only** `SendingApplication` and `TriggerEvent`. The
   standard set is deliberately skipped for it, and MUST be: the dedicated
   Mosaiq topic's subscription filters on those two and a stray
   `MessageType` would change what other rules see.
4. `paris` gets the standard set and nothing flow-specific.
5. `pims` gets the standard set and nothing flow-specific (upstream
   `build_pims_properties`, `04b30e7`, returns only `MessageType` =
   `MSH-9.2`, which the standard set already carries; the PIMS topic
   subscriptions filter on `SourceSystem`, set for every flow, and
   `MessageType` alone). The value is the **source** trigger (`A04`, `A08`,
   `A40`); the transformer corrects it for the next hop
   (`09-transformers.md` §5.11).

§6.2.1 Each is built from the parsed message alone, so a message that
cannot yield a value gets `""` for it (`03` §4.1), never a failure (§5.4).

§6.3 The Service Bus `message_id` is the inbound `MSH-10`.

---

## §7 Accepted message types

The Python registers handlers by `MSH-9` and rejects anything else with an
`UnsupportedMessageType` error. The registered keys are
(`hl7_server_application.py`):

```
ADT^A31^ADT_A05   ADT^A28^ADT_A05   ADT^A40^ADT_A39
ADT^A31           ADT^A28           ADT^A40
ADT^A04^ADT_A01   ADT^A08^ADT_A01   ADT^A40^ADT_A40
```

§7.1 A message whose `MSH-9` is not one of these MUST be rejected. The
failure is logged as `Unsupported Message Type: {detail}` and emitted as
`MESSAGE_FAILED` with validation result `Unsupported message type`, and an
`AE` NACK echoing the header is sent (§4.3, §4.3.1).

§7.2 A message that cannot be parsed at all is logged as
`Invalid HL7 Message: {detail}`, emitted as `MESSAGE_FAILED` with
validation result `Invalid HL7 message format`, and a generic `AE` NACK is
sent (§4.3.2).

§7.3 The list in §7 is a single allow-list applied by one handler; there is
no per-type behaviour. It MAY be made configurable, but its default MUST be
exactly this set.

---

## §8 Shutdown

§8.1 `SIGTERM` and `SIGINT` begin a graceful shutdown.

§8.2 Shutdown order (`hl7_server_application.py :: stop_server`): the
Service Bus sender, the message store client, the health-check listener, the
MLLP listener, then join the listener task.

§8.3 In-flight messages MUST be allowed to complete. A message that has been
acknowledged must have been forwarded, and shutdown must not create the one
failure §5.3 forbids.

§8.2a **The Rust inverts the first three steps of §8.2**, and does so to
satisfy §8.3. Closing the Service Bus sender while the MLLP listener is
still accepting opens a window in which a message is received and cannot be
forwarded; the only correct response to a message in that window is to
refuse it, which is the outcome §5.3 forbids. So: stop accepting, drain,
then close the sender and the health check. The relative order of those last
two is unchanged. See [`plan.md` §9](../plan.md#9-deliberate-deviations-from-the-python).

---

## §9 Tests

§9.1 Unit: each validation rule of §3, including every error message string.

§9.2 Unit: ACK construction, every field of §4, for HL7 2.3.1, 2.4, 2.5 and
2.5.1 inbound messages, with and without `MSH-11`.

§9.3 Unit: the ordering of §5, by asserting on the sequence of calls made to
a recording fake bus.

§9.4 Integration: a real MLLP listener over a real TCP socket, driven with
the sample messages in `Integration-Hub-Beta/local/sample_messages/`,
asserting the ACK bytes and the message that landed on the fake bus.

§9.5 Integration: a frame exceeding `MAX_MESSAGE_SIZE_BYTES` is answered with a
generic `AE` NACK, closes the connection and emits the truncated event of
§2.3.

§9.6 Integration: two frames written in one `write` receive two ACKs (§2.5).

§9.7 Integration: a Service Bus send failure produces an `AE` NACK and no `AA` (§5.3, §4.3), and a
message store failure still produces one (§5.2).

§9.8 Golden: for each flow's sample message, the bytes sent to the bus and
the `XmlPayload` in the store event match the Python's output.
