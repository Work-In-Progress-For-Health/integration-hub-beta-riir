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

§3.7.1 `AR` (application reject) and `AE` (application error) are both
failures and are both retried. Nothing distinguishes a permanent rejection
from a transient one at this layer; Service Bus's `MaxDeliveryCount`
eventually dead-letters it.

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
