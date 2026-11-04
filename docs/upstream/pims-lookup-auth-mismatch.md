# Upstream report — the PIMS transformer cannot authenticate to the lookup service

**Status: not filed.** Verified against `integration-hub-beta` at `438cda2`
(2026-11-02) on 2026-11-04 by reading the source; the two components were not
run against each other with auth on.

Tracked in [`tasks.md`](../../tasks.md) under "Upstream sync — October 2026".

## What the Python does

- The PIMS reference-data client sends its credential as an `X-API-Key`
  header, from `REFERENCE_DATA_API_KEY`
  (`transformers/hl7_pims_transformer/hl7_pims_transformer/clients/reference_data_client.py`,
  lines 189 and 226–227).
- The lookup service, since #426, accepts an Entra bearer token in
  `Authorization`, or the Easy Auth principal header behind the platform
  (`lookup_service/lookup_service/security.py`; the lookup route is guarded by
  `HTTPBearer` and `require_api_reader`, `api/lookup.py` line 25). It has no
  `X-API-Key` path at all.

## Consequence

With `LOOKUP_AUTH_DISABLED` unset, which is the only permitted setting
outside DEV and TST, every lookup from the PIMS transformer is refused with
`401`. The PIMS transformer treats a failed lookup as a failed message, so
the flow would stop. Locally nobody sees this, because the local env sets
`LOOKUP_AUTH_DISABLED="true"` (`local/env/lookup/lookup-service.env`).

The lookup service is marked "Phase 1, not production ready", so this may be
known and scheduled. It is recorded here because the Rust port reproduces the
Python exactly and so inherits the gap.

## What the Rust does

Matches the Python: `hl7-pims-transformer` sends `X-API-Key`
(`spec/09-transformers.md` §5.10), and `lookup-service` accepts bearer and
Easy Auth only (`spec/20-lookup-service.md` §12). No deviation was made,
because the fix is a decision about how the transformer obtains a token
(client credentials against Entra, or a platform-injected Easy Auth
identity), and that belongs to the Hub team.

## Decision needed from the Hub team

| Option | Effect |
| --- | --- |
| The transformer acquires an Entra token and sends `Authorization: Bearer` | Matches the lookup service as built; needs an app registration and a secret or managed identity for the transformer |
| The lookup service gains an `X-API-Key` path for service callers | Matches the transformer as built; a new credential type to rotate and a new thing to audit |
| Leave it until Phase 2 | Nothing breaks while auth is off in DEV and TST |

| Decision | |
| --- | --- |
| Chosen option | |
| Decided by, on | |
