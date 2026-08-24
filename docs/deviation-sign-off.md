# Deviation sign-off pack

Implements [`spec/18-deployment.md`](../spec/18-deployment.md) §7.2 step 1
and closes `tasks.md` P9.6.

Thirteen places where the Rust knowingly differs from the Python, from
[`plan.md` §9](../plan.md#9-deliberate-deviations-from-the-python). §7.2
requires every deviation touching a container to be confirmed with the Hub
team **before that container is cut over**.

This document exists to make that confirmation possible: what the Python
does, what the Rust does instead, where to look, and what is actually being
decided. Every Python file and line reference below was checked against the
source in `Integration-Hub-Beta/` while this was written.

**Nothing here is signed off. Signing off is the Hub team's, not the
author's.** The Decision column is deliberately empty.

---

## Read this one first

**Deviation 8 is not a rewrite question.** The Python's XML → ER7 converter
misplaces HL7 components, and it is doing so in production now, on the LIMS
SOAP flow. It needs the Hub team's attention **whether or not any Rust is
ever deployed**. Do not let it ride along as a cutover detail.

---

## Summary

| # | Deviation | Containers affected | Kind |
|---|---|---|---|
| 8 | XML → ER7 moves components | `hl7-soap-server` (LIMS) | **Live Python defect — act independently** |
| 1 | Pipelined MLLP frames each get a reply | `hl7-server`, `hl7-mock-receiver` | Fixes a Python defect |
| 2 | The last-resort NACK is framed | `hl7-mock-receiver` | Fixes a Python defect |
| 3 | Oversized frame closes the connection | `hl7-server`, `hl7-mock-receiver` | Behaviour change, bounded |
| 9 | Shutdown stops listening first | `hl7-server` | Fixes a message-loss window |
| 5 | Configuration validated once, at startup | all | Fails earlier and louder |
| 6 | Message-store inserts are chunked | `message-store-service` | Internal; contract unchanged |
| 7 | Blank optional variables are absent, uniformly | all | Consistency |
| 4 | No `AZURE_MONITOR_OWNER` handshake | all | Internal |
| 10 | Bad `Content-Length` gets a bare `400` | `hl7-soap-server` | Visible difference |
| 11 | BusWatch reports peeked messages as the count | `buswatch` | Visible difference |
| 12 | BusWatch broker timeouts are seconds | `buswatch` | Visible difference |
| 13 | BusWatch uses the brand's blue | `buswatch` | Cosmetic |

**Every container is covered.** Deviations 4, 5 and 7 apply to *all* of
them, so a container not named above — `http-mock-receiver`, `hl7-sender`,
the transformers, `message-replay-job`, `dashboard` — still has three rows
to fill in before it is cut over. It has no container-specific deviation,
which is not the same as having none.

---

## 8 — XML → ER7 moves components

**Status: a defect in the Python, live in the LIMS SOAP flow.**

**Python.** `shared_libs/hl7_validation/hl7_validation/convert.py`
`_extract_text_from_element`, lines 496–513, builds a component list from
an element's **non-empty** children:

```python
for child in elem:
    child_text = _extract_text_from_element(child)
    if child_text:
        append(child_text)
...
return "^".join(children_texts)
```

Position therefore comes from order among the survivors, so dropping one
empty component shifts every component after it left.

**Measured**, on `local/sample_messages/lims-to-mpi.sample.xml`:

```
source   PID|1||B0000010612^^^328^PI||…|||Welsh Parliament^Cardiff Bay^CARDIFF^^CF99 1SN
Python   PID|1||B0000010612^328^PI||…|||Welsh Parliament^Cardiff Bay^CARDIFF^CF99 1SN
Rust     PID|1||B0000010612^^^328^PI||…|||Welsh Parliament^Cardiff Bay^CARDIFF^^CF99 1SN
```

The identifier's assigning authority (`PID-3.4`) arrives at MPI in
`PID-3.2`. The postcode lands in the wrong address component.

**Rust.** Reads the component's position from the element's name, so an
absent component leaves a gap. Byte-identical to the source ER7.

**What is being decided.** Not whether the Rust should match the Python —
matching would mean knowingly sending MPI misfiled identifiers. What needs
deciding is what to do about the Python **now**, and whether anything
downstream has come to depend on the wrong shape.

Evidence: [`spec/06-validation.md`](../spec/06-validation.md) §5.9.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 1 — Pipelined MLLP frames each get a reply

**Python.** Reads until the buffer *ends* with `<FS><CR>`. Two frames
arriving in one TCP read are treated as one message and answered once, so
the sender is still waiting for a second ACK that never comes.

**Rust.** `hl7-2-mllp::Framer` splits the buffer into frames and answers
each. Already true of the repository's own `hl7_mock_receiver_rust`.

**What is being decided.** Whether any sending system depends on receiving
one ACK for two frames. Nothing observed does.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 2 — The last-resort NACK is framed

**Python.** `hl7_mock_receiver/hl7_mock_receiver/error_handler.py` — the
success path returns `nack_message.to_mllp()` (line 22), and the fallback
returns a bare string (line 24):

```python
return "MSH|^~\\&|||||||NACK||P|2.5|||AL\rMSA|AE|UNKNOWN|Error processing message\r"
```

No `<VT>` … `<FS><CR>`. A peer waiting for the trailer waits until its own
timeout.

**Rust.** Every reply is framed, including this one.

**What is being decided.** Nothing controversial; recorded because it
changes what a peer sees on the error path.

> Path note: `plan.md` §9 cites this as
> `hl7_mock_receiver/error_handler.py`. The file is one level deeper, at
> `hl7_mock_receiver/hl7_mock_receiver/error_handler.py`.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 3 — An oversized frame closes the connection

**Python.** Keeps buffering. `MAX_MESSAGE_SIZE_BYTES` exists
(`hl7_server/hl7_server/app_config.py`, default 1 MB) but a frame that
never terminates grows the buffer.

**Rust.** Bounded by the same variable and the same default; exceeding it
closes the connection.

**What is being decided.** That a sender transmitting an over-large
message gets a closed connection rather than a slow one. The limit and its
default are unchanged.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 9 — Shutdown stops listening before it closes the sender

**Python.** [`spec/07-hl7-server.md`](../spec/07-hl7-server.md) §8.2
records the order: Service Bus sender, message store client, health check,
MLLP listener, then join. Taken literally that contradicts §8.3 — closing
the sender while the listener still accepts opens a window where a message
can be received and cannot be forwarded.

**Rust.** Stops accepting first, drains in flight, then closes the sender
and health check. The order of the last two is unchanged.

**What is being decided.** Confirmation that "accepted but not forwarded"
is the outcome to avoid — §5.3 calls it the one failure this system must
never produce. Nothing outside the process can observe the difference
except by not losing a message.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 5 — Configuration is validated once, at startup, completely

**Python.** Reads some settings lazily; a bad value surfaces minutes later
on the first message that needs it.

**Rust.** Parses the whole configuration before binding a socket, and exits
non-zero listing **every** problem at once.

**What is being decided.** That a misconfigured container should fail to
start rather than start and fail later. This changes what a bad deploy
looks like: a container that will not come up, instead of one that is
healthy until traffic arrives.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 6 — Message-store inserts are chunked

**Python.** `message_store_service/message_store_service/db_client.py`
uses `fast_executemany` with no bound on batch size.

**Rust.** TDS has a parameter limit, so batches are split at it. The
transaction boundary stays **per batch**, so the abandon-on-failure
contract is unchanged.

**What is being decided.** Nothing externally visible. Recorded because the
number of round trips changes.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 7 — Blank optional variables are absent, uniformly

**Python.** Does this in `_read_env` but not `_read_int_env`, which would
call `int("")` and raise.

**Rust.** Everywhere, for every type. `FOO=` means unset.

**What is being decided.** That a templated container definition whose
source was empty is treated as "not configured" rather than as a bad value.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 4 — No `AZURE_MONITOR_OWNER` handshake

**Python.** Three libraries can each call `configure_azure_monitor`;
`AZURE_MONITOR_OWNER` arbitrates which one wins
(`shared_libs/event_logger_lib/event_logger_lib/event_logger.py`, line 74).

**Rust.** Telemetry initialises once, from one place. The variable is
accepted and ignored, so leaving it set breaks nothing.

**What is being decided.** Nothing operationally visible.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 10 — An unparseable `Content-Length` gets a bare `400`

**Python.** [`spec/08-hl7-soap-server.md`](../spec/08-hl7-soap-server.md)
§2 pairs the condition with a `400` and the fault string
`Invalid Content-Length header.`

**Rust.** `hyper` rejects the request as a protocol violation and answers
`400` with an empty body before any handler runs. The status matches; the
envelope does not. RFC 9112 §6.3 requires such a message to be rejected,
and the protocol layer is where that belongs. The application-level check
remains for any value `hyper` admits.

**What is being decided.** Whether any client parses that fault body. A
client that only checks the status is unaffected.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 11 — BusWatch reports the messages it peeked as the queue count

**Python.** `buswatch/buswatch/servicebus_reader.py :: get_queue_runtime`
returns `None` for every count in emulator mode, so the reference page
shows no counts at all.

**Rust.** Reports a lower bound — the messages actually peeked. Dead-letter
counts stay absent, because reaching a dead-letter sub-queue costs a second
receiver per queue against §6.1's connection quota.

**What is being decided.** That "at least N" is more useful than "unknown".
The number is a lower bound and must not be read as a queue depth.

Evidence: [`spec/14-buswatch.md`](../spec/14-buswatch.md) §4.3.1–§4.3.2.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 12 — BusWatch's broker timeouts are seconds, not the estate's minute

**Python.** `MAX_WAIT` is 60 seconds.

**Rust.** Seconds. An empty queue is BusWatch's normal case — clearing an
already-clear queue, asking an idle queue for its next session — and a
60-second wait costs a page everything. The shorter wait is passed to the
operation rather than imposed by wrapping it in a cancel: cancelling a
receive loses messages the broker has already handed over, and cancelling a
session acceptance can leave the session locked, which would let a
read-only developer tool block the service it is watching.

**What is being decided.** A developer-tool timeout only. No service uses
these values.

Evidence: [`spec/03-message-bus.md`](../spec/03-message-bus.md) §5.1.2a–b,
[`spec/14-buswatch.md`](../spec/14-buswatch.md) §3.3.1–§3.3.2.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## 13 — BusWatch uses the brand's blue

**Python.** `buswatch/buswatch/templates/message_detail.html` sets
`--nhs-blue: #2C3E72`.

**Rust.** [`spec/14-buswatch.md`](../spec/14-buswatch.md) §7.2 gives NHS
Wales Blue as `#325083`, and the specification is normative here. The two
pages will not match.

**What is being decided.** Which value is right. Flagged rather than
silently reconciled — if `#2C3E72` is the correct brand value then the
specification is wrong and should be corrected.

| Confirmed by | Date | Decision |
|---|---|---|
| | | |

---

## Recording the outcome

A container may be cut over once every row touching it is filled in. The
mapping is the Summary table above; `docs/cutover-runbook.md` §2 step 1 is
where it is checked.
