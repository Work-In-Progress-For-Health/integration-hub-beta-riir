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

§2.2 A frame is decoded as UTF-8. Invalid UTF-8 is a rejection; the
connection is closed and the event is logged as `MESSAGE_FAILED`.

§2.3 **Frame size.** A frame that would exceed `MAX_MESSAGE_SIZE_BYTES` MUST
cause the connection to close. Before closing, the service MUST log at error

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

§2.6 The socket read timeout is 10 seconds
(`size_limited_mllp_server.py`, `timeout=10`). A timeout closes the
connection.

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

§3.3.3 Any other flow value performs no field-level rules here; its
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

§4.3 The server only ever sends `AA`. A message it cannot accept gets **no
reply at all** — the connection carries the error by closing. This is the
Python's behaviour (every failure path re-raises out of `reply`) and the
source systems' retry logic depends on it.

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

§6.2 When `HL7_VALIDATION_FLOW` is `mpi`, the MPI routing properties of
[`03-message-bus.md`](03-message-bus.md) §4 are merged in.

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
`MESSAGE_FAILED` with validation result `Unsupported message type`, and no
reply is sent.

§7.2 A message that cannot be parsed at all is logged as
`Invalid HL7 Message: {detail}`, emitted as `MESSAGE_FAILED` with
validation result `Invalid HL7 message format`, and no reply is sent.

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

§9.5 Integration: a frame exceeding `MAX_MESSAGE_SIZE_BYTES` closes the
connection and emits the truncated event of §2.3.

§9.6 Integration: two frames written in one `write` receive two ACKs (§2.5).

§9.7 Integration: a Service Bus send failure produces no ACK (§5.3), and a
message store failure still produces one (§5.2).

§9.8 Golden: for each flow's sample message, the bytes sent to the bus and
the `XmlPayload` in the store event match the Python's output.
