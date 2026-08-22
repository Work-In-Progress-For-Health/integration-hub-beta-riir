# 02 — Configuration

Every environment variable this workspace reads, with its type, default and
parse rule. Normative: these names and defaults are the deployment contract.

Implemented by `crates/ihb-config`.

Reference implementation: the `app_config.py` of each Python service, and
`Integration-Hub-Beta/shared_libs/transformer_base_lib/app_config.py`.

---

## §1 Parse rules

These rules apply to every variable in this document unless a row says
otherwise.

§1.1 **Blank is absent.** A variable set to the empty string, or to
whitespace only, MUST be treated exactly as if it were unset. This is the
Python's dominant behaviour (`_read_required_env` tests
`value.strip() == ""`) and the Rust MUST apply it *uniformly*, including to
integer variables where the Python would have raised on `int("")`. See
[`plan.md` §9.7](../plan.md#9-deliberate-deviations-from-the-python).

§1.2 **Values are trimmed.** Leading and trailing whitespace is stripped
before use.

§1.3 **A missing required variable is a startup failure.** The process MUST
exit non-zero before binding any socket, with a message naming the variable:

```
missing required configuration: EGRESS_QUEUE_NAME
```

§1.4 **All problems are reported at once.** Unlike the Python, which fails
on the first missing variable, a service MUST collect every configuration
error and report them together, one per line. This turns a five-restart
debugging loop into one.

§1.5 **Booleans.** Where a variable is a boolean, the strings `false`, `0`,
`no` and `off` (case-insensitive, after trimming) mean false and **any other
non-blank value means true**. Absent means the row's default. This
reproduces `event_logger.py :: _REDACTION_DISABLED_VALUES` and
`servicebus_client_factory.py :: _read_bool_env`, which differ from each
other in the Python; the union above is normative.

§1.6 **Integers.** Parsed as `i64` then range-checked into the target type.
A non-numeric non-blank value is a startup failure naming the variable and
saying what was expected. It MUST NOT be silently defaulted.

§1.7 **Comma-separated lists.** Split on `,`, each element trimmed, empty
elements dropped. A list variable that resolves to zero elements after this
is a startup failure if the row says the list must be non-empty.

§1.8 **Precedence.** No variable is read from a file. There is no `.env`
loading in a container; the `local/*.env` files are consumed by the compose
runner, not by the process. `configparser`-based `config.ini` files in the
Python are replaced by environment variables — see §7.

---

## §2 Common to every service

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `LOG_LEVEL` | level | `INFO` | Root log level. Servers default `INFO`; the Python's workers defaulted `ERROR`, which is preserved per-service in §3–§6. Accepts `TRACE`, `DEBUG`, `INFO`, `WARN`/`WARNING`, `ERROR`. |
| `AZURE_LOG_LEVEL` | level | `WARN` | Level for the Azure SDK targets. |
| `RUST_LOG` | filter | *(unset)* | If set, overrides `LOG_LEVEL` and `AZURE_LOG_LEVEL` with a `tracing-subscriber` `EnvFilter` directive. Rust-only addition; absent means the two variables above are used. |
| `WORKFLOW_ID` | string | — | **Required.** Flow identifier; a telemetry dimension. |
| `MICROSERVICE_ID` | string | — | **Required.** This component's identifier; a telemetry dimension and the OTel service name. |
| `HEALTH_BOARD` | string | — | **Required** for services that emit metrics. Metric dimension. |
| `PEER_SERVICE` | string | — | **Required** for services that emit metrics. Metric dimension and the message store's default `TargetSystem`. |
| `HEALTH_CHECK_HOST` | string | `127.0.0.1` | Health-check bind address. |
| `HEALTH_CHECK_PORT` | u16 | `9000` | Health-check port. Absent means the default; see [`05-health-check.md`](05-health-check.md) §2. |
| `APPLICATIONINSIGHTS_CONNECTION_STRING` | string | *(unset)* | Blank or absent disables telemetry export entirely. |
| `INSIGHTS_UAMI_CLIENT_ID` | string | *(unset)* | Client id of a user-assigned managed identity for telemetry ingestion. Absent uses the ambient credential. |
| `OTEL_SERVICE_NAME` | string | `MICROSERVICE_ID` | Set only if absent or blank. |
| `HL7_LOG_REDACTION_ENABLED` | bool (§1.5) | `true` | `false` logs full payloads. Never `false` in an environment holding real patient data. |
| `AZURE_MONITOR_OWNER` | string | — | **Accepted and ignored.** See [`plan.md` §9.4](../plan.md#9-deliberate-deviations-from-the-python). |
| `SSL_CERT_FILE` | path | *(unset)* | PEM bundle of additional trusted roots. |

### §2.1 Service Bus connection, common to every bus client

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `SERVICE_BUS_CONNECTION_STRING` | string | *(unset)* | Full connection string. **Wins when both are set.** |
| `SERVICE_BUS_NAMESPACE` | string | *(unset)* | Namespace short name; `.servicebus.windows.net` is appended and the ambient Azure credential is used. |

§2.1.1 If neither is set, a service that requires the bus MUST fail at
startup. `hl7-mock-receiver` is the exception: it MAY run with neither and
log what it would have forwarded.

§2.1.2 `MESSAGE_STORE_ENABLED` (bool, default `true`) disables message-store
sends. When false, `send_to_store` is a no-op that logs at debug and the
store sender is never created.

---

## §3 `hl7-server`

`Integration-Hub-Beta/hl7_server/hl7_server/app_config.py`.

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `HOST` | string | `127.0.0.1` | MLLP bind address. |
| `PORT` | u16 | `2575` | MLLP port. |
| `EGRESS_QUEUE_NAME` | string | — | Destination queue. |
| `EGRESS_TOPIC_NAME` | string | — | Destination topic. |
| `EGRESS_SESSION_ID` | string | — | **Required.** Session for the outgoing message. |
| `MESSAGE_STORE_QUEUE_NAME` | string | *(unset)* | Message-store queue. Absent means no store sender. |
| `HL7_VERSION` | string | *(unset)* | If set, `MSH-12` must equal it. |
| `SENDING_APP` | csv | *(unset)* | If set, `MSH-3` must be in the list. |
| `HL7_VALIDATION_FLOW` | string | *(unset)* | Flow schema group; see [`06-validation.md`](06-validation.md) §3. |
| `HL7_VALIDATION_STANDARD` | string | *(unset)* | If set, validate against this HL7 release. One of `2.4`, `2.5`, `2.5.1`, `2.6`. |
| `MAX_MESSAGE_SIZE_BYTES` | usize | `1048576` | Largest frame accepted. |
| `LOG_LEVEL` | level | `INFO` | |

§3.1 Exactly one of `EGRESS_QUEUE_NAME` and `EGRESS_TOPIC_NAME` MUST be
set. Neither is a startup failure —
`"Either EGRESS_QUEUE_NAME or EGRESS_TOPIC_NAME must be provided"`. Both is
a startup failure —
`"Cannot specify both EGRESS_QUEUE_NAME and EGRESS_TOPIC_NAME."`

§3.2 `MAX_MESSAGE_SIZE_BYTES` that is absent, non-positive, or unparseable
resolves to the default. A value above `104857600` (100 MiB, the Azure
Service Bus Premium limit) is a startup failure. Note the asymmetry: a
*negative or zero* value is silently defaulted, while an *oversized* value
fails. This reproduces `_read_and_validate_message_size` and is deliberate.

§3.3 `HL7_VALIDATION_STANDARD` set to a release outside
`{2.4, 2.5, 2.5.1, 2.6}` is a startup failure.

---

## §4 `hl7-soap-server`

`Integration-Hub-Beta/hl7_soap_server/hl7_soap_server/app_config.py`.

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `HOST` | string | `127.0.0.1` | HTTP bind address. |
| `PORT` | u16 | `8080` | HTTP port. |
| `SOAP_ENDPOINT_PATH` | path | `/soap` | MUST start with `/`, else a startup failure. |
| `EGRESS_QUEUE_NAME` / `EGRESS_TOPIC_NAME` | string | — | As §3.1. |
| `EGRESS_SESSION_ID` | string | — | **Required.** |
| `MESSAGE_STORE_QUEUE_NAME` | string | — | **Required** for this service. |
| `HL7_SCHEMA_GROUP` | string | `phw` | Which flow's schemas validate the payload. |
| `ALLOWED_HL7_STRUCTURES` | csv, non-empty | `ADT_A05,ADT_A39` | Accepted payload root elements. |
| `ALLOWED_ASSIGNING_AUTHORITIES` | csv, non-empty | `328` | Accepted assigning authorities. |
| `MAX_REQUEST_SIZE_BYTES` | usize | `1048576` | As §3.2, same 100 MiB ceiling. |
| `TLS_CERT_FILE` | path | *(unset)* | With `TLS_KEY_FILE`, enables TLS. |
| `TLS_KEY_FILE` | path | *(unset)* | |

§4.1 TLS is enabled only when **both** files are configured. One without the
other is a startup failure (the Python silently ran plaintext; that is a
security defect and is corrected here).

---

## §5 Transformers

`Integration-Hub-Beta/shared_libs/transformer_base_lib/app_config.py`.

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `INGRESS_QUEUE_NAME` | string | — | **Required.** |
| `INGRESS_SESSION_ID` | string | *(unset)* | |
| `EGRESS_QUEUE_NAME` | string | — | **Required.** |
| `EGRESS_SESSION_ID` | string | *(unset)* | |
| `MAX_BATCH_SIZE` | usize | `100` | Messages requested per receive. |
| `LOG_LEVEL` | level | `ERROR` | The Python's transformer default. |

§5.1 `MAX_BATCH_SIZE` came from a `config.ini` shipped inside each Python
package (`[DEFAULT] MAX_BATCH_SIZE = 100`). In Rust it is an environment
variable with the same default. An unparseable value MUST fail at startup
rather than silently falling back to `1` as the Python did — a batch size of
1 in production is a 100× throughput regression that would be attributed to
Rust.

---

## §6 Senders

### §6.1 `hl7-sender`

`Integration-Hub-Beta/hl7_sender/hl7_sender/app_config.py`.

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `INGRESS_QUEUE_NAME` | string | — | **Required.** |
| `INGRESS_SESSION_ID` | string | — | **Required.** |
| `RECEIVER_MLLP_HOST` | string | — | **Required.** |
| `RECEIVER_MLLP_PORT` | u16 | — | **Required.** |
| `MESSAGE_STORE_QUEUE_NAME` | string | — | **Required.** |
| `ACK_TIMEOUT_SECONDS` | u64 | `30` | |
| `MAX_MESSAGES_PER_MINUTE` | u32 | *(unset)* | Absent means unthrottled. A value ≤ 0 is a startup failure. |
| `MAX_BATCH_SIZE` | usize | `100` | Reduced automatically per [`10-senders.md`](10-senders.md) §2.3. |
| `LOG_LEVEL` | level | `ERROR` | |

### §6.2 `hl7-subscription-sender`

As §6.1 with these differences:

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `INGRESS_TOPIC_NAME` | string | — | **Required.** Replaces `INGRESS_QUEUE_NAME`. |
| `INGRESS_SUBSCRIPTION_NAME` | string | — | **Required.** |
| `INGRESS_SESSION_ID` | string | *(unset)* | Optional here, unlike §6.1. |
| `MESSAGE_STORE_QUEUE_NAME` | — | — | Not read by this service. |

### §6.3 `soap-sender` and `soap-subscription-sender`

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `SOAP_ENDPOINT_URL` | url | — | **Required.** |
| `SOAP_TIMEOUT_SECONDS` | u64 | `30` | |
| `SOAP_API_KEY` | string | *(unset)* | Sent as `Authorization: ApiKey <key>`. |
| `SOAP_CLIENT_CERT_PATH` | path | *(unset)* | PEM client certificate for mTLS. |
| `WS_SECURITY_ENABLED` | bool | `false` | Reserved; MUST be `false` until specified. |
| ingress variables | | | Queue variant as §6.1; subscription variant as §6.2. |

---

## §7 Persistence

### §7.1 `message-store-service`

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `INGRESS_QUEUE_NAME` | string | — | **Required.** The message-store queue. |
| `MAX_BATCH_SIZE` | usize | `100` | |
| `SQL_SERVER` | string | — | **Required.** |
| `SQL_DATABASE` | string | — | **Required.** |
| `SQL_USERNAME` | string | *(unset)* | |
| `MSSQL_SA_PASSWORD` | secret | *(unset)* | Note the name: the password variable is not `SQL_PASSWORD`. |
| `SQL_ENCRYPT` | `Yes`/`No` | `Yes` | |
| `SQL_TRUST_SERVER_CERTIFICATE` | `Yes`/`No` | `No` | |
| `MANAGED_IDENTITY_CLIENT_ID` | string | *(unset)* | User-assigned identity for SQL. Absent uses the system-assigned identity. |

§7.1.1 `SQL_USERNAME` and `MSSQL_SA_PASSWORD` MUST be supplied together or
not at all. One without the other is a startup failure naming the missing
one. Both present selects password authentication; both absent selects
managed-identity authentication.

### §7.2 `message-replay-job`

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `REPLAY_BATCH_ID` | uuid | — | **Required.** Not a valid UUID is a startup failure. |
| `REPLAY_BATCH_SIZE` | usize | `100` | Must be a positive integer; blank means the default. |
| `PRIORITY_QUEUE_NAME` | string | — | **Required.** |
| SQL variables | | | As §7.1. |

---

## §8 Mock receivers

### §8.1 `hl7-mock-receiver`

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `HOST` | string | `127.0.0.1` | |
| `PORT` | u16 | `2576` | |
| `EGRESS_QUEUE_NAME` | string | — | **Required.** |
| `EGRESS_SESSION_ID` | string | *(unset)* | |
| `MAX_MESSAGE_SIZE_BYTES` | usize | `16777216` | 16 MiB, the `hl7-2-mllp` default. |

### §8.2 `http-mock-receiver`

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `HOST` | string | `127.0.0.1` | |
| `PORT` | u16 | `8080` | |
| `EGRESS_QUEUE_NAME` | string | *(unset)* | Absent means log-only mode. |
| `EGRESS_SESSION_ID` | string | *(unset)* | |

---

## §9 Observation surfaces

### §9.1 `buswatch`

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `SERVICEBUS_CONNECTION_STRING` | string | `Endpoint=sb://localhost` | Note: **not** `SERVICE_BUS_CONNECTION_STRING`. Buswatch's own spelling. |
| `BUSWATCH_QUEUE_NAMES` | csv | *(unset)* | Blank means discover queues. |
| `BUSWATCH_PEEK_COUNT` | usize | `25` | Messages peeked per queue. |
| `BUSWATCH_DETAIL_SEARCH_LIMIT` | usize | `250` | Messages scanned when resolving a sequence number. |
| `HOST` | string | `127.0.0.1` | Where the page binds. Loopback by default: [`14-buswatch.md`](14-buswatch.md) §1.2 means this is not a page to expose by accident. The container image overrides it to `0.0.0.0`. |
| `PORT` | u16 | `8000` | The container image overrides it to `8080`, which is what the Python's `Dockerfile` exposes. |
| `BUSWATCH_MAX_PARALLEL_QUEUE_FETCHES` | usize | `2` | Queues refreshed at once ([`14-buswatch.md`](14-buswatch.md) §3.3). The Python's spelling, kept for the same reason `SERVICEBUS_CONNECTION_STRING` is. Bounded because the emulator's connection quota is what an unbounded fan-out exhausts. |

§9.1.1 BusWatch MUST start with no queues configured and none discovered,
serving a page that says what to set. A developer tool that refuses to start
is a developer wondering why.

### §9.2 `dashboard`

| Variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `AZURE_TENANT_ID` | string | *(unset)* | |
| `AZURE_CLIENT_ID` | string | *(unset)* | |
| `AZURE_CLIENT_SECRET` | secret | *(unset)* | |
| `AZURE_SUBSCRIPTION_ID` | string | *(unset)* | |
| `AZURE_RESOURCE_GROUP` | string | *(unset)* | |
| `AZURE_SERVICE_BUS_NAMESPACE` | string | *(unset)* | |
| `AZURE_LOG_ANALYTICS_WORKSPACE_ID` | string | *(unset)* | |
| `AZURE_APP_INSIGHTS_RESOURCE_ID` | string | *(unset)* | Narrows the queries that honour it to one resource ([`15-dashboard.md`](15-dashboard.md) §4.3.3). |
| `AZURE_CONTAINER_APPS_ENVIRONMENT` | string | *(unset)* | |
| `AZURE_CONTAINER_APPS_RESOURCE_GROUP` | string | *(`AZURE_RESOURCE_GROUP`)* | Defaults to the main resource group. |
| `FLASK_SECRET_KEY` | secret | *(unset)* | Session signing key; kept under its Python name for deployment compatibility. **Absent means a key generated at startup**, not the Python's hardcoded default — see §9.2.3. |
| `COSMOS_ENDPOINT` | url | *(unset)* | Blank disables alarm persistence; reads return empty. |
| `COSMOS_KEY` | secret | *(unset)* | Present selects key auth and auto-creation; blank selects AAD RBAC. |
| `COSMOS_DATABASE` | string | `integration-hub` | The Python's default. See §9.2.2. |
| `COSMOS_CONTAINER` | string | `alarms` | Partition key `/pk`. |
| `COSMOS_DISABLE_SSL_VERIFY` | bool | `false` | True only for the local emulator, and refused against any other host. |
| `QUEUE_WARNING_THRESHOLD` | u64 | `10` | |
| `QUEUE_CRITICAL_THRESHOLD` | u64 | `50` | |
| `DLQ_WARNING_THRESHOLD` | u64 | `1` | |
| `API_CACHE_TTL` | u64 | `30` | Seconds `/api/status` is cached ([`15-dashboard.md`](15-dashboard.md) §2.2.1). |
| `DEFAULT_LANGUAGE` | string | `en` | `en` or `cy`. |
| `DASHBOARD_REDACT_MESSAGES` | bool | `true` | [`15-dashboard.md`](15-dashboard.md) §1.3. No Python equivalent: the Python never redacts. |
| `ALERT_EMAIL_ENABLED` | bool | `false` | |
| `ALERT_EMAIL_TO` | string | *(unset)* | |
| `ALERT_EMAIL_FROM` | string | *(unset)* | |
| `ALERT_EMAIL_MAX_RETRIES` | u32 | `3` | Retries on an ACS `429`. |
| `ALERT_EMAIL_RETRY_BACKOFF_SECONDS` | f64 | `2` | Doubled per attempt, unless the response carries `Retry-After`. |
| `AZURE_KEY_VAULT_URL` | url | *(unset)* | Where the ACS connection string is read from. Blank falls back to `ACS_CONNECTION_STRING`. |
| `ACS_EMAIL_SECRET_NAME` | string | `acs-email-connection-string` | |
| `ACS_CONNECTION_STRING` | secret | *(unset)* | Local development only. |
| `ACS_SECRET_CACHE_TTL` | u64 | `3600` | Seconds the Key Vault fetch is cached. |

§9.2.1 The dashboard MUST start and serve every page when no Azure
configuration is present, returning empty state rather than errors. This is
load-bearing for local development and for the test suite.

§9.2.2 `COSMOS_DATABASE` defaults to **`integration-hub`**. An earlier draft
of this table said `integration-hub-dashboard`, which is not what
`dashboard/config.py` reads. The default decides which Cosmos database
alarm configuration is loaded from, so getting it wrong would have made
every alarm rule silently disappear at cutover while the dashboard reported
no error at all — it would simply have found an empty database. Corrected
against the source.

§9.2.3 `FLASK_SECRET_KEY` absent MUST mean **a key generated at startup**.
The Python defaults it to the literal `dev-secret-key-change-in-production`,
which is in a public repository: any deployment that has not set the
variable is signing its sessions with a key an attacker already has. A
generated key costs a logout on restart and is the safe end of that trade.
Raised as [`15-dashboard.md`](15-dashboard.md) §9.3.

§9.2.4 `FLASK_DEBUG`, `DEMO_MODE`, `SPLASH_SCREEN_ENABLED`,
`ENVIRONMENT_LABEL_MAP` and `ENVIRONMENT_COLOR_MAP` are **not** read. Debug
mode is a Flask concept with no analogue here; demo mode is superseded by
§1.2, which makes every page work with no Azure at all; the other three are
presentation settings deferred with the i18n work of
[`15-dashboard.md`](15-dashboard.md) §6.

---

## §10 The `ihb-config` contract

§10.1 `ihb-config` MUST expose readers for: required string, optional
string, required/optional integer of a given type, boolean per §1.5,
comma-separated list, path, URL, UUID, and a level.

§10.2 Every reader MUST apply §1.1 and §1.2 before doing anything else.

§10.3 A service's configuration type MUST be constructed by a single
`from_env()` that accumulates errors per §1.4 and returns
`Result<Config, ConfigErrors>` where `ConfigErrors` renders one problem per
line.

§10.4 A configuration type MUST NOT implement `Debug` in a way that prints a
secret. Fields holding `SERVICE_BUS_CONNECTION_STRING`, `MSSQL_SA_PASSWORD`,
`AZURE_CLIENT_SECRET`, `COSMOS_KEY`, `FLASK_SECRET_KEY` and `SOAP_API_KEY`
MUST render as `<redacted>`. There MUST be a test asserting a known secret
value does not appear in the `Debug` output.

§10.5 `ihb-config` MUST NOT read the process environment at any point other
than an explicit call. Its parsing functions MUST take values, not names, so
they are testable without mutating the global environment.
