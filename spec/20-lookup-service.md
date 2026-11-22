# 20 — Lookup service

A reference-data lookup service for the transformers: a transformer asks it
for a substitution value (an `MSH-3` code to a health board, say) instead of
carrying a mapping table in its own source. Implemented by
`crates/lookup-service`, which is a library and a binary.

Reference implementation: `Integration-Hub-Beta/lookup_service/`
(upstream `b835acb`, "add lookup service (Phase 1 - not production ready)",
`0ae06f4`, a dependency bump only, `2a220e3`, #426, "Phase 1.5 auth and
Phase 1.25 DEV/TST trial switch", and `cd833d6`, #430, "trust
`X-Forwarded-Proto`"). See
[`docs/upstream-sync-2026-10.md`](../docs/upstream-sync-2026-10.md).

---

## §1 Status and scope

§1.1 **This is a Phase 1 service and is not production ready.** The Python
says so in its README and this port inherits the status unchanged; it is
recorded here so that nobody deploys it on the strength of a green build.
What is missing, by the Python's own phase plan:

| Phase | Missing | Consequence now |
| --- | --- | --- |
| 1.5 | (done, §12) Authentication, roles and CSRF | Bearer tokens for the lookup API, Easy Auth for the GUI and admin API; replaces the "writes only in `LOCAL`" rule |
| 2 | A client library, deployment | No client library; no deployment definition here (§10.4) |
| 3 | Versioned, atomic table replace | A replace is not atomic: a reader can briefly see a mix of old and new rows (§6.5) |
| 4 | Negative caching; cross-replica cache invalidation | A miss is never cached; an upload is only seen by the replica that handled it, so run one replica (§5.4) |
| 5 | XPath / JMESPath mapping paths and more transforms | Only CSV column names and `trim`/`upper`/`lower` (§6.3) |

§1.2 No transformer calls this service yet. Nothing in `crates/` depends on
`lookup-service`, and no flow in `01-architecture.md` includes it.

§1.3 **What lives in the tables is reference data, not patient data**, but
the rule of `spec/index.md` §0.8 is applied to it regardless, because a
lookup key is whatever a caller sends. A lookup key or value MUST NOT appear
in a log line, an error message or a span attribute; a `404` MUST NOT echo
the key (§4.4); request access logging MUST NOT be enabled, because the key
travels in the query string.

§1.4 The Cosmos DB store is **not ported** (§3.3). The Python's only writable
store is Cosmos, so in this workspace every deployed instance is read-only
and serves the seed files (§7) until that store is written.

---

## §2 Configuration

Per [`02-configuration.md`](02-configuration.md) §9.3. Blank is absent
(`02` §1).

§2.1 `ENVIRONMENT` is upper-cased. `LOCAL`, `DEV` and `SIT` expose the
Python's interactive API documentation; this port serves **no** API
documentation page in any environment (§9.2).

§2.2 `HOST` defaults to `0.0.0.0` and `PORT` to `8080`, as the Python does.
This differs from `buswatch` and `dashboard` (loopback by default) on purpose:
the Python image sets no `HOST` and a service that binds loopback inside a
container is unreachable.

§2.3 `ENVIRONMENT=LOCAL` is a developer machine: with authentication off
(§2.8) every caller is `local-dev`, and the CSRF cookie is plain (§12.5).
**Superseded:** until `2a220e3` writes were allowed only in `LOCAL`, because
there was no authentication. Who may write is now decided by role (§12.1) in
every environment; the store being writable is still separate (§8.1).

§2.4 `COSMOS_DISABLE_SSL_VERIFY=true` with a `COSMOS_ENDPOINT` that does not
name `localhost`, `127.0.0.1` or `cosmos-emulator` MUST stop the service
starting. It is the same refusal as `15-dashboard.md` §5.6.1 and for the same
reason: it would disable certificate verification against a real account.

§2.5 `COSMOS_ENDPOINT` set MUST stop the service starting with a message that
says the Cosmos store is not yet ported (§1.4). Falling back to the seed
files silently would serve a table that may differ from the one the operator
configured, which is the failure this system exists not to produce.

§2.6 An integer variable that is not a whole number, or is below its minimum
(`1` for every one here), is a configuration error reported with the others
(`02` §1.4). It is never replaced by its default.

§2.7 **Authentication is configured or deliberately off, never absent.**
With authentication enabled (`LOOKUP_AUTH_DISABLED` unset or false)
`LOOKUP_AUTH_TENANT_ID` and `LOOKUP_AUTH_AUDIENCE` MUST both be set, or the
service does not start: `Authentication is not configured: set
LOOKUP_AUTH_TENANT_ID and LOOKUP_AUTH_AUDIENCE`, with `(set
LOOKUP_AUTH_DISABLED=true for local development)` appended in `LOCAL`.
Serving unauthenticated because a variable was missing is how a typo opens a
service.

§2.8 **Switching authentication off.** `LOOKUP_AUTH_DISABLED` is exactly
`true`, `false`, `1` or `0` (any case); anything else is a configuration
error. `true` is accepted only in `LOCAL`, `DEV` and `TST`, else `LOOKUP_AUTH_DISABLED=true is only
allowed when ENVIRONMENT is one of LOCAL, DEV, TST`; and outside `LOCAL` it
also needs `LOOKUP_AUTH_DISABLED_ACK=I_UNDERSTAND_LOOKUP_IS_UNAUTHENTICATED`
exactly (`LOOKUP_AUTH_DISABLED=true in DEV also needs
LOOKUP_AUTH_DISABLED_ACK=...`), so one stray flag in a variables file cannot
open a deployed service. The acknowledgement alone disables nothing. In a
deployed environment (`DEV`/`TST`) with authentication off, start-up logs a
warning, and every page carries a banner (§12.4).

§2.9 `LOOKUP_SEED_DIR` unset (the deployed default) means **no seed**: nothing
is imported and, without a writable store, no tables are served (§7.1).

---

## §3 Tables and stores

§3.1 A **table** has a name, a description (at most 500 characters), one or
more **key columns**, one or more **value columns**, a **default value
column**, a per-key-column **normalisation rule**, an optional `ttl_seconds`
and a `preload` flag. A table definition is valid only if:

1. the name matches `^[a-z0-9][a-z0-9_-]{0,63}$`;
2. every column name matches `^[A-Za-z_][A-Za-z0-9_]{0,63}$` (column names
   appear in `key.<column>` query parameters, so they are kept URL-safe);
3. column names are unique across key and value columns;
4. the default value column is a value column;
5. a normalisation rule names only key columns;
6. `ttl_seconds`, when present, is from 1 to 604 800 (seven days).

Every violation is reported, as `<field>: <reason>`, never with the offending
value.

§3.2 A **row** is a canonical key (a list of strings, one per key column) and
its values, **kept in the order of the value columns** so a response and the
page both list them in the order the definition does.

§3.3 A **store** holds definitions and rows. It MUST offer: open, close,
list tables, get a table, point-read a row by canonical key, load every row of
a table, query a page of rows ordered by key text with an optional
case-insensitive key substring, replace a table (definition and rows
together), and update a definition alone. It reports a `source` name and
whether it is writable. A read-only store refuses both writes with
`read_only` (§8). This workspace has two: the **file store** (`source =
"file"`, read-only, §7) and an **in-memory store** (`source = "memory"`,
writable), which exists so the write paths can be exercised and is not
selectable by configuration. The Python's third, Cosmos-backed store
(`source = "cosmos"`) is not ported (§1.4); if it is, its document layout
(`lookup-rows`, partition key `/table_name`, id from §4.5; `lookup-config`,
partition key `/pk`, id `table::<name>`) and its transactional-batch rules
(at most 100 operations and 1 500 000 bytes per batch; upsert before delete)
are the Python's.

---

## §4 Keys

§4.1 A **canonical key** is the tuple of normalised parts in `key_columns`
order. It is built in exactly one place, so normalisation cannot drift between
loading a row and looking it up.

§4.2 **Normalisation** of one part: trim surrounding whitespace unless the
rule's `trim` is false (default true); then upper-case, lower-case or leave
the case, per the rule's `case` (`preserve`, the default, `upper` or
`lower`). A rule accepts no other field.

§4.3 A caller supplies the key **either positionally** (`k=` repeated, in
`key_columns` order) **or by name** (`key.<column>=`), never both. Each of
these is `invalid_key` (`422`): no key at all; both styles; the wrong number
of positional parts; an unknown or a missing part name; a name supplied more
than once; a part empty after normalisation.

§4.4 A lookup that finds no row is `key_not_found` (`404`) and its message
names the table and **not the key**.

§4.5 The Cosmos row id for a key is the parts joined by `::` with every
character in `% : / \ ? #` and every control character (U+0000–U+001F,
U+007F) percent-encoded (UTF-8, upper-case hex), so the join is reversible.
If the result exceeds 255 bytes the id is `h:` followed by the lower-case
hex SHA-256 of the key serialised as a JSON array **with `, ` between
elements and non-ASCII characters left unescaped** (the Python's
`json.dumps(..., ensure_ascii=False)`), so a hashed id is the same one the
Python writes. The display form of a key is its parts joined by ` :: `.

---

## §5 Resolution and caching

§5.1 At startup every table definition is loaded into memory, so a lookup
never reads a definition. `/health/ready` passes only after that, and every
API route and page except `/health/live` answers `503 not_ready` until then.

§5.2 A table with `preload` is held whole in memory as an immutable snapshot
and a lookup never reads the store. Any other table is served
cache, then store point read, then cache.

§5.3 The TTL is the table's `ttl_seconds`, else `LOOKUP_DEFAULT_TTL_SECONDS`.
A cache holds at most `LOOKUP_CACHE_MAX_ENTRIES` rows, evicting expired
entries first and then the least recently used. It counts hits and misses.
The hit and miss counts and the recency tick are `u64` and **hold at
`u64::MAX`** rather than wrap or fail (declared fallback, `rust-checked-arithmetic`
RCA-DECL-4: a statistic that cannot go higher is still correct as a lower bound,
and reaching it takes 2^64 lookups). A TTL whose seconds overflow `i64` is out
of range and rejected as any TTL over seven days is (§2.6); an upload limit
whose byte count overflows is a startup failure, and `max_upload_bytes()` is
`None` (uploads refused), never "unlimited".
Only found rows are cached (§1.1, Phase 4). `cached` in a response is `false`
only when the row was read from the store for that request; a preloaded
table's answer is always `true`.

§5.4 After an upload or a settings change the table's cache and snapshot are
rebuilt on the replica that handled it, and the new definition replaces the
old. Other replicas do not see the change (§1.1).

§5.5 A store failure on a lookup is `backend_unavailable` (`503`) and carries
the error's type, never its text.

§5.6 Uploads and settings changes on one replica run one at a time.

---

## §6 Importing CSV

§6.1 **Parsing** follows the Python `csv` module's defaults: comma
delimiter, `"` quoting with `""` as an escaped quote, line breaks inside a
quoted field, `\n`, `\r\n` or `\r` terminators, and a quote that is not at the
start of a field is literal. The file is UTF-8 and a leading BOM is
tolerated; anything else is `invalid_upload` ("not valid UTF-8", with a byte
offset). Header cells are trimmed. Blank lines, and lines whose cells are all
blank, are skipped. A **line number** in any message is the physical line on
which the record **ends**.

§6.2 The header is rejected, with a line-1 error, when the file is empty or
its first line is blank ("file is empty; a header row is required"), when it
holds an empty column name, or when it holds duplicate names. A record with a
different number of cells from the header is `line N: expected H columns, got
C`.

§6.3 A **mapping** says which CSV column feeds each key and value column and
which transforms to apply. The default maps each column to the CSV column of
the same name. Transforms are `trim`, `upper` and `lower`, applied in order.
Key parts get the mapping's transforms and then the table's normalisation
(§4.2). The header must contain every mapped column and nothing else:
`columns [...] are not in the header` and `header columns [...] are neither
key nor value columns` (an unmapped column is refused, not dropped, so no
one is surprised that data they expected to be served is not). The mapping's
targets must equal the table's columns.

§6.4 **All problems are found and reported together, with line numbers and
never with values.** A record is rejected for a missing field, an empty key
part (`empty key part '<column>'`) or a duplicate key after normalisation
(`duplicate key (first seen on line N)`). At most 50 are listed; a final line
says `... further errors suppressed after the first 50`. If any exists,
nothing is changed.

§6.5 A replace writes the new rows before deleting stale ones, so a key
present before and after never disappears. It is not atomic (§1.1).

---

## §7 Seed data

§7.0 **No seed is built in.** Upstream removed `lookup_service/seed/` from the
image (`2a220e3`): a deployed environment starts with no tables and is loaded
through the GUI or API. The two test tables that used to be the seed now live
in `crates/lookup-service/tests/fixtures/seed/` and are fixtures only.

§7.1 Each `<LOOKUP_SEED_DIR>/<name>.csv` is a table named after its file.
Without a manifest entry the first column is the key and the rest are values,
the first value column is the default, keys are trimmed and case is
preserved. `tables.json` (`{"tables": {"<name>": {...}}}`) may set
`description`, `key_columns`, `value_columns`, `default_value_column`,
`key_normalisation`, `ttl_seconds` and `preload`, and nothing else.

§7.2 **Any seed problem stops the service starting**: an unreadable or
mis-shaped manifest; a manifest entry with no CSV; an unknown manifest field;
`key_columns` or `value_columns` that is not a list of strings; a file over
50 MiB or not valid UTF-8; an invalid definition; any import problem of §6.
The message names the file and, for a row, the line, never a value.

§7.3 With a writable store, a seed table that does not yet exist in the store
is imported at startup, with uploader `seed`; one that exists is **never**
overwritten, so an upload survives a redeploy. A missing seed directory is
skipped with a warning when the store is writable, and is a startup failure
when the file store needs it.

§7.4 The test fixtures are `health_board_mapping` (`msh3` to `health_board`;
preloaded; it MUST equal `HEALTH_BOARD_MAPPING` in the ChemoCare transformer's
`pid_mapper`) and `ward_map` (a composite `sending_facility`/`ward_code` key,
upper-cased; illustrative data only). They are not served by default (§7.0).

§7.5 **`local/lookup-seed/`** is what local Compose mounts at `/seed` and sets
`LOOKUP_SEED_DIR` to. It holds one table, `pims_to_mpi_codes` (key columns
`code_type`, `pims_code`; value column and default `mpi_code`; keys
normalised `lower` and `upper`; preloaded), the PIMS to eMPI translation of
`09-transformers.md` §5.10. A **blank** `mpi_code` is a row, not a gap: it
means "leave the field unset". A bad file there stops the local stack
starting (§7.2), so a test loads it.

---

## §8 HTTP API

All bodies are JSON. An error is `{"error": "<code>", "detail": "...",
"table": "..."}` with `table` omitted-as-null when there is none, and
`"errors": [...]` present only for upload and settings validation.

| Method | Path | Behaviour |
| --- | --- | --- |
| `GET` | `/api/v1/lookup/{table}` | `?k=…` (repeatable) or `?key.<column>=…`. `{table, key, value, values, source, cached}`; `value` is the default value column |
| `GET` | `/api/v1/tables` | Every table: columns, row count, caching mode, cache stats, last upload |
| `GET` | `/api/v1/tables/{table}` | One table, plus its normalisation rules |
| `GET` | `/api/v1/tables/{table}/rows` | `search` (at most 200 characters), `offset` (≥ 0), `limit` (1–200, default 50); `{table, total, offset, limit, search, rows}` |
| `POST` | `/api/v1/tables/{table}/uploads` | Multipart: `file`, optional `definition` and `mapping` (JSON). `201 {table, row_count, upserted, removed, duration_ms, created}` |
| `PATCH` | `/api/v1/tables/{table}` | `ttl_seconds` (1–604 800 or `null`) and/or `preload`; only the fields present change; nothing else is accepted |
| `GET` | `/health/live` | `{"status":"ok"}`, no dependency checks |
| `GET` | `/health/ready` | `{status:"ready", tables, source}`, else `503 {"status":"not_ready"}` |

| Situation | Status | `error` |
| --- | --- | --- |
| Unknown table | 404 | `table_not_found` |
| No row for the key | 404 | `key_not_found` |
| Bad key (§4.3) | 422 | `invalid_key` |
| Bad CSV, rows, definition or mapping; nothing changed | 422 | `invalid_upload` |
| Bad settings | 422 | `invalid_settings` |
| Malformed query or body (limit out of range, unknown PATCH field, missing `file`) | 422 | `invalid_request` |
| File over `LOOKUP_MAX_UPLOAD_MB` | 413 | `upload_too_large` |
| No, invalid or expired credential (§12) | 401 | `unauthorized` |
| Authenticated without the role (§12.1) | 403 | `forbidden` |
| CSRF check failed (§12.5) | 403 | `csrf_failed` |
| Write to the read-only store | 409 | `read_only` |
| Tables not loaded yet | 503 | `not_ready` |
| Store failure | 503 | `backend_unavailable` |

§8.1 A write is checked in this order: the caller (`401`, `403`, and for the
CSRF-protected routes `403 csrf_failed`, §12), the request is well formed
(`422`), the table exists where the route needs it (`404`), the store is
writable (`409`), then the body is read (`413`) and validated (`422`).
Authentication comes first on **every** route but the health checks and
`/static/`, ahead of `not_ready`, as FastAPI solves a route's dependencies
before anything else.

§8.2 `definition` for an existing table replaces its definition together with
its data; without it the existing definition is kept. A new table without one
is `invalid_upload`. A table name that does not match §3.1 is
`invalid_upload`.

§8.3 A settings change on a table whose merged definition is invalid is
`invalid_settings` with `errors` and changes nothing.

§8.4 **Deviation.** The Python lets FastAPI answer a malformed request in its
own `{"detail": [...]}` shape. This port answers in the service's one error
shape with `invalid_request` (the table above). Callers of the documented
errors are unaffected.

---

## §9 Pages

§9.1 The GUI is `GET /` (table overview), `GET /tables/{table}` (definition,
cache statistics, a lookup tester that is a plain GET form, searchable rows
50 to a page, and a cache-settings form), `GET`/`POST /upload` and `POST
/tables/{table}/settings`. A successful form post is a `303` to the table
page (`?uploaded=<rows>` or `?saved=1`); a failed one re-renders the form with
the submitted values, the error and the error's status. An unknown table
renders a `404` page. Every page sets `Content-Security-Policy: default-src
'self'; frame-ancestors 'none'; form-action 'self'`, `X-Content-Type-Options:
nosniff` and `Referrer-Policy: no-referrer`, and every interpolated value is
HTML-escaped.

§9.2 **Deviations.** The Python's interactive API documentation (`/docs`,
`/openapi.json`) is not served. The Inter font files and Bootstrap Icons are
not shipped; the page uses the system font stack and text labels. The
stylesheet, both scripts and the logo are the Python's, served from
`/static/`.

§9.3 Every page carries a CSRF token in a hidden `csrf_token` field of its
forms (§12.5). A reader sees the upload page with its form disabled and the
reason, `Changing tables needs the Lookup.Editor role`, rather than a `403`
after filling it in. The overview, table and upload pages are readable by any
role of §12.1; posting needs an editor.

§9.4 A TTL entered in the forms is a whole number and a unit
(`seconds`, `minutes`, `hours`, `days`); a blank value means the service
default. It is converted to whole seconds and rejected, as `invalid_settings`,
if it is not a whole number, is below 1, exceeds seven days or has another
unit. The stored form is always seconds. A TTL is shown in the largest unit
that divides it exactly (`7200` as `2 hours`, singular at 1).

---

## §10 Deployment

§10.1 The image is the common `docker/Dockerfile` with `BIN=lookup-service`.
It carries no seed (§7.0); `LOOKUP_SEED_DIR`, when set, names a directory the
container can read (local Compose bind-mounts `local/lookup-seed`, §7.5).

§10.2 The Python's Compose profile `lookup` also starts the Cosmos emulator.
This port's Compose service does not depend on one, for the reason in §1.4.
The service also belongs to the `pims-to-mpi` profile, because the PIMS
transformer resolves its codes through it (`09-transformers.md` §5.10); its
env file sets `LOOKUP_AUTH_DISABLED=true` (`LOCAL`) and `LOOKUP_SEED_DIR=/seed`.

§10.3 Readiness for an orchestrator is `GET /health/ready`.

§10.4 There is **no ADO build definition** for this binary, because the
Python has none (its deployment is that project's Phase 2). `cargo xtask
verify-pipelines` lists `lookup-service` in `NOT_SERVICES` for that reason
(`18-deployment.md` §6.6); the entry is removed when a pipeline is added.

§10.5 The Compose service (`lookup` profile, `local/lookup-service.env`)
publishes `127.0.0.1:8090`, as the Python's does, and its health check is the
TCP probe of `18-deployment.md` §3.1 on the web port.

§10.6 **Behind the ingress.** In Container Apps the ingress terminates TLS, so
the service sees `http` and the browser's scheme arrives only in
`X-Forwarded-Proto`. Upstream's `cd833d6` (#430) made uvicorn trust it
(`proxy_headers=True`, `forwarded_allow_ips` from `FORWARDED_ALLOW_IPS`,
default `*`), because generated asset URLs were `http://` and the browser
blocked the CSS and scripts as mixed content. **Nothing here changes**: every
page links its assets by an absolute **path** (`/static/lookup.css?v=...`),
which the browser resolves against the scheme it used, and no handler builds
an absolute URL from the request. A test pins that no page contains
`http://` (§11.3). `FORWARDED_ALLOW_IPS` is therefore not read.

---

## §11 Tests

§11.1 The Python's `unittest` suites are ported one for one except where
noted: configuration, keys, TTL conversion, CSV import and the mapping
engine, the TTL cache, the resolver, the file store, seed import, the API and
the pages. The tests of the Cosmos store and its batching are not ported
(§1.4). The assertion that the API documentation is gated by environment
(§2.1) is replaced by one that it is never served (§9.2).

§11.3 Tests of §12: configuration (every row of §2.7 and §2.8), the token
verifier against a key whose private half the test holds (a valid token;
expired; wrong audience, issuer, algorithm and signature; unknown key id;
missing claims; the v1 issuer; an audience list; clock skew), the key cache
(a hit, a refresh on an unknown id, the refresh interval, a failed fetch
answering `503`), Easy Auth parsing, the role matrix per route, CSRF, the
challenge header, the page banner and token, and the absence of `http://`
from every page (§10.6).

§11.2 A PII-marker test asserts that a distinctive key and value supplied in
a request appear in no error body and no rendered error (`spec/index.md`
§0.8.4).

---

## §12 Authentication, roles and CSRF

Reference implementation: `lookup_service/lookup_service/security.py`
(`2a220e3`, #426). `crates/lookup-service/src/security.rs`, `jwks.rs`.

§12.1 **Roles**, each including the ones below it: `Lookup.Reader` (read
tables and look keys up), `Lookup.Editor` (also upload and change settings),
`Lookup.Admin`.

| Route | Mechanism | Needs |
| --- | --- | --- |
| `GET /api/v1/lookup/{table}` | bearer token (§12.2) | any role |
| `GET /api/v1/tables`, `/tables/{table}`, `/tables/{table}/rows` | Easy Auth (§12.3) | any role |
| `POST /api/v1/tables/{table}/uploads`, `PATCH /api/v1/tables/{table}` | Easy Auth, plus CSRF unless a bearer header is sent (§12.5) | Editor or Admin |
| `GET /`, `/tables/{table}`, `/upload` | Easy Auth | any role |
| `POST /upload`, `/tables/{table}/settings` | Easy Auth plus CSRF | Editor or Admin |
| `/health/*`, `/static/*` | none | none |

§12.2 **The bearer token** is an Entra access token, taken from
`Authorization: Bearer`. A missing or blank one is `401` `A bearer token is
required`. It is verified: `RS256` only (any other `alg`, `none` above all, is
refused); the signature against the key its `kid` names, from the tenant's
`https://login.microsoftonline.com/{tenant}/discovery/v2.0/keys`, cached for
300 s and refreshed on an unknown `kid` **at most once per 30 s** (deviation:
upstream refreshes on every unknown `kid`, which lets anyone able to send a
token make the service fetch from Microsoft per request); `exp`, `iss` and
`aud` required; `exp` and `nbf` with 60 s of skew; `iss` either
`https://login.microsoftonline.com/{tenant}/v2.0` or
`https://sts.windows.net/{tenant}/` (a v1 token, the app registration
default); `aud` equal to `LOOKUP_AUTH_AUDIENCE`, or containing it when a
list. The roles are the `roles` claim; the name is the first present of
`preferred_username`, `azp`, `appid`, `oid`, `sub` (at most 200 characters),
else `unknown`. Every refusal is `401` with `WWW-Authenticate: Bearer`:
`Token has expired` for an expired one, `Invalid token` for the rest. If the
key set cannot be fetched the answer is `503 backend_unavailable` (the caller
did nothing wrong). A valid token without a reader role is `403`.

§12.3 **Easy Auth.** Container Apps Easy Auth overwrites
`X-MS-CLIENT-PRINCIPAL` (base64 JSON with a `claims` list) and
`X-MS-CLIENT-PRINCIPAL-NAME`. They are trusted **only with authentication
enabled** (§2.7). Absent is `401 Authentication required`; over 32 KiB, not
base64, not JSON or without `claims` is `401 Invalid authentication
principal`. Roles are the claims of type `roles`, the WS-Federation role URI,
or the type named by `role_typ`. The name is the name header, else the first
of `preferred_username`, `name` and the WS-Federation name claim, else
`unknown`. A principal without the route's role is `403`: `Requires one of
the roles: ...` (sorted).

§12.4 **Authentication off** (§2.8): every caller is `local-dev` in `LOCAL`
and `unauthenticated` elsewhere, both `Lookup.Admin`; neither header nor
token is read. The name is recorded against uploads, so it must say plainly
that nobody signed in. Every page of a `DEV`/`TST` deployment carries the
banner `Authentication is disabled in <ENV>: anyone who can reach this
service can view and change tables. Use test data only.`

§12.5 **CSRF** is the double-submit pattern. A page issues a 43-character
URL-safe token in a cookie, `lookup_csrf` in `LOCAL` and `__Host-lookup_csrf`
elsewhere (`HttpOnly`, `SameSite=Lax`, `Path=/`, and `Secure` outside
`LOCAL`; `__Host-` stops a sibling subdomain planting a value). A cookie that
is not 32 to 128 URL-safe characters is replaced. A state-changing request
must send the same value in `X-CSRF-Token` or the `csrf_token` field, compared
in constant time; otherwise `403 csrf_failed` (`CSRF token missing or
invalid; reload the page and try again`). A request carrying an
`Authorization: Bearer` header is exempt on the admin API, because a browser
never attaches one on its own.

§12.6 The principal's name is logged with an upload (`by`) and a settings
change, and stored as the uploader.

§12.7 **Never logged**: a token, a key, a claim value, a header's contents.
A rejected token logs the kind of failure only.
