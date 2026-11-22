# Checked-arithmetic audit (`rust-checked-arithmetic`, RCA)

Measured 2026-11-22 on branch `upstream-sync-2026-10` after `0ae2192`, with
`cargo clippy --workspace --lib --bins -- -W clippy::arithmetic_side_effects
-W clippy::as_conversions -W clippy::indexing_slicing -W clippy::string_slice
-W clippy::integer_division -W clippy::modulo_arithmetic -W clippy::unwrap_used
-W clippy::expect_used -W clippy::panic -W clippy::unreachable ...` (production
code only; tests are exempt, RCA-EXC-5). Nothing here is enforced yet: the
workspace sets none of these lints, so the gate cannot see them (RCA-ENF-1).

**`unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented` and the `cast_*`
lints report nothing** in production code. What remains is arithmetic on
offsets and counters, byte-offset string slicing, indexing, and a few `as`.

| Crate | Total | arithmetic_side_effects | string_slice | indexing_slicing | as_conversions | integer_division | unreachable |
|---|---:|---:|---:|---:|---:|---:|---:|
| `lookup-service` | 64 | 37 |  | 19 | 4 | 4 |  |
| `db-sender` | **0** (was 62) | | | | | | |
| `dashboard` | 32 | 24 | 2 |  | 6 |  |  |
| `ultra7` | 20 | 9 |  | 10 | 1 |  |  |
| `hl7-soap-server` | 17 | 8 | 9 |  |  |  |  |
| `hl7-pims-transformer` | 16 | 8 |  | 7 | 1 |  |  |
| `ihb-validation` | 13 | 6 | 5 | 2 |  |  |  |
| `http-mock-receiver` | 11 | 4 | 7 |  |  |  |  |
| `ihb-bus` | 8 | 7 |  | 1 |  |  |  |
| `hl7-server` | 7 |  | 4 | 3 |  |  |  |
| `soap-sender` | 7 | 2 | 5 |  |  |  |  |
| `ihb-fields` | 6 | 4 |  | 2 |  |  |  |
| `fhir-sender` | 6 | 4 | 2 |  |  |  |  |
| `xml-fhir-proms-transformer` | 5 | 2 |  | 3 |  |  |  |
| `integration-hub-tester` | 5 | 3 |  | 2 |  |  |  |
| `message-store-service` | 3 | 3 |  |  |  |  |  |
| `message-replay-job` | 3 | 3 |  |  |  |  |  |
| `buswatch` | 3 | 3 |  |  |  |  |  |
| `ihb-sender` | 3 | 2 |  |  | 1 |  |  |
| `rest-server` | 3 | 1 | 2 |  |  |  |  |
| `ihb-sql` | 2 |  |  |  |  | 1 | 1 |
| `ihb-identity` | 1 |  |  |  | 1 |  |  |
| `ihb-telemetry` | 1 | 1 |  |  |  |  |  |
| `hl7-mock-receiver` | 1 |  |  | 1 |  |  |  |
| `ihb-transformer` | 1 | 1 |  |  |  |  |  |
| `hl7-rest-server` | 1 |  | 1 |  |  |  |  |
| `hl7-sender` | 1 |  |  | 1 |  |  |  |
| `hl7-lims-wrrs-transformer` | 1 |  | 1 |  |  |  |  |
| **all** | **303** | |||||| 

**In progress:** `lookup-service` (`cache`, `config`, `file_store`, `import`, `pages`,
`ttl`, `web`, `admin` done; `form.rs`, `jwks.rs`, `security.rs` left, 44 findings).

**Done:** `db-sender` (denied at the crate root; `sanitize.rs` rewritten on remainder
slices, `msh.rs` salvage dropped, spec/10 §7.4 and §7.5).

## Found by grep, not by Clippy (RCA-ENF-5)

`saturating_*`, `wrapping_*`, `overflowing_*` and `clamp` are forbidden outright
(RCA-INT-4, RCA-EXC-4) but are not lints, so the table above does not count them.
`rg -w --type rust '(saturating|wrapping|overflowing)_\w+|\.clamp\('` on production
code (2026-11-22):

| Where | What | Needs |
|---|---|---|
| `hl7-pims-transformer/src/reference_data.rs:504` | `2_u32.saturating_pow(retry - 1)` | declared back-off ceiling, `checked_pow` + `unwrap_or` |
| `ultra7/src/iteration.rs:65-66` | `saturating_mul`, `saturating_add`, `try_from(..).unwrap_or(0)` | error: an overflowing iteration is invalid |
| `hl7-rest-server/src/http.rs:120` | `limit.saturating_mul(6).saturating_add(1024)` | checked, refuse on overflow |
| `dashboard/src/contacts.rs:454` | `(page.max(1) - 1).saturating_mul(per_page)` | **decision**: `page=0` is invalid input (RCA-IN-2) |
| `dashboard/src/azure/token.rs:195` | `Instant::now() + expires_in.saturating_sub(..)` | `checked_*`, declared fallback |
| `dashboard/src/web.rs:444` | `.clamp(1, MAX_THROUGHPUT_HOURS)` | **decision**: an out-of-range query parameter is an error |
| `ihb-sender/src/main_loop.rs:85`, `throttle.rs:158,166` | `saturating_sub`, `allowable.clamp(1, ..)` | declared floor, or error |
| `dashboard/src/azure/email.rs:164` | `backoff * 2_u32.saturating_pow(attempt)` | declared ceiling |
| `hl7-server/src/listener.rs:199` | `len().saturating_sub(buffered())` | `checked_sub`, the invariant is the point |
| `lookup-service/src/security.rs:753` | `len().saturating_sub(1)` | `lookup-service` pass, below |

## Decisions needed (rejection currently becomes a substitute, RCA-IN-2 / RCA-IN-7)

Changing these alters behaviour, so they need a spec decision first and are
**not** changed by the arithmetic pass:

- `lookup-service/src/pages.rs`: `extras.page.max(1)` turns page `0` into page 1.
- `dashboard/src/contacts.rs`, `dashboard/src/web.rs`: as in the table above.

## Plan (RCA-ENF-3: one module at a time, then deny at the crate root)

1. Convert a crate until `-W` reports nothing; then add the `deny` block (as in
   `crates/db-sender/src/trrr.rs`) at its root so it cannot regress.
2. Decide each case by the rule, not by habit: a failed operation on valid input
   recovers only through a *declared* fallback (RCA-NONE-3); malformed input is an
   error (RCA-IN-2). Where upstream's behaviour is a fallback for malformed input,
   raise it in `spec/` (RCA-DECL-3/-6) before porting it. The first one found:
   `count_segments` (spec/10 §7.9).
3. When every crate is clean, move the lints into `[workspace.lints.clippy]` and
   add the `clippy.toml` bans (RCA-ENF-1, -2) so `cargo xtask gate` enforces them.

Suggested order, hardest scanners first because they set the pattern:
`db-sender` (`sanitize.rs`, `msh.rs`: byte-offset text scanners, to be rewritten
on `str::get`, `split_once`, `char_indices`), `lookup-service`, `dashboard`,
`ultra7`, then the rest by count.

## Findings

`file:line lint`, one per line.

```
crates/buswatch/src/live.rs:296 arithmetic_side_effects
crates/buswatch/src/live.rs:333 arithmetic_side_effects
crates/buswatch/src/render.rs:99 arithmetic_side_effects
crates/dashboard/src/alarm/mod.rs:358 arithmetic_side_effects
crates/dashboard/src/alarm/mod.rs:386 as_conversions
crates/dashboard/src/alarm/pause.rs:450 arithmetic_side_effects
crates/dashboard/src/alarm/pause.rs:531 arithmetic_side_effects
crates/dashboard/src/alarm/pause.rs:565 arithmetic_side_effects
crates/dashboard/src/alarm/pause.rs:734 arithmetic_side_effects
crates/dashboard/src/alarm/pause.rs:737 arithmetic_side_effects
crates/dashboard/src/alarm/pause.rs:782 arithmetic_side_effects
crates/dashboard/src/alarm/pause.rs:867 arithmetic_side_effects
crates/dashboard/src/alarm/rules.rs:129 arithmetic_side_effects
crates/dashboard/src/azure/email.rs:164 arithmetic_side_effects
crates/dashboard/src/azure/email.rs:167 arithmetic_side_effects
crates/dashboard/src/azure/live.rs:249 as_conversions
crates/dashboard/src/azure/live.rs:398 as_conversions
crates/dashboard/src/azure/token.rs:195 arithmetic_side_effects
crates/dashboard/src/contacts.rs:454 arithmetic_side_effects
crates/dashboard/src/contacts.rs:650 arithmetic_side_effects
crates/dashboard/src/contacts.rs:664 arithmetic_side_effects
crates/dashboard/src/contacts.rs:719 arithmetic_side_effects
crates/dashboard/src/contacts.rs:815 arithmetic_side_effects
crates/dashboard/src/contacts.rs:817 arithmetic_side_effects
crates/dashboard/src/flow_sources.rs:299 arithmetic_side_effects
crates/dashboard/src/flow_sources.rs:339 arithmetic_side_effects
crates/dashboard/src/language.rs:125 string_slice
crates/dashboard/src/language.rs:133 string_slice
crates/dashboard/src/network_test.rs:297 as_conversions
crates/dashboard/src/network_test.rs:300 as_conversions
crates/dashboard/src/network_test.rs:313 as_conversions
crates/dashboard/src/network_test.rs:352 arithmetic_side_effects
crates/dashboard/src/pages/contacts.rs:153 arithmetic_side_effects
crates/dashboard/src/period.rs:111 arithmetic_side_effects
crates/dashboard/src/topics.rs:143 arithmetic_side_effects
crates/fhir-sender/src/client.rs:283 arithmetic_side_effects
crates/fhir-sender/src/client.rs:303 arithmetic_side_effects
crates/fhir-sender/src/client.rs:316 arithmetic_side_effects
crates/fhir-sender/src/client.rs:329 arithmetic_side_effects
crates/fhir-sender/src/client.rs:490 string_slice
crates/fhir-sender/src/client.rs:494 string_slice
crates/hl7-lims-wrrs-transformer/src/classifier.rs:93 string_slice
crates/hl7-mock-receiver/src/server.rs:117 indexing_slicing
crates/hl7-pims-transformer/src/lib.rs:424 arithmetic_side_effects
crates/hl7-pims-transformer/src/reference_data.rs:356 arithmetic_side_effects
crates/hl7-pims-transformer/src/reference_data.rs:420 indexing_slicing
crates/hl7-pims-transformer/src/reference_data.rs:421 as_conversions
crates/hl7-pims-transformer/src/reference_data.rs:445 indexing_slicing
crates/hl7-pims-transformer/src/reference_data.rs:456 arithmetic_side_effects
crates/hl7-pims-transformer/src/reference_data.rs:456 indexing_slicing
crates/hl7-pims-transformer/src/reference_data.rs:470 indexing_slicing
crates/hl7-pims-transformer/src/reference_data.rs:474 arithmetic_side_effects
crates/hl7-pims-transformer/src/reference_data.rs:474 indexing_slicing
crates/hl7-pims-transformer/src/reference_data.rs:478 arithmetic_side_effects
crates/hl7-pims-transformer/src/reference_data.rs:481 indexing_slicing
crates/hl7-pims-transformer/src/reference_data.rs:482 arithmetic_side_effects
crates/hl7-pims-transformer/src/reference_data.rs:482 indexing_slicing
crates/hl7-pims-transformer/src/reference_data.rs:504 arithmetic_side_effects
crates/hl7-pims-transformer/src/reference_data.rs:538 arithmetic_side_effects
crates/hl7-rest-server/src/ack.rs:124 string_slice
crates/hl7-sender/src/client.rs:220 indexing_slicing
crates/hl7-server/src/ack.rs:241 string_slice
crates/hl7-server/src/listener.rs:171 indexing_slicing
crates/hl7-server/src/listener.rs:189 indexing_slicing
crates/hl7-server/src/listener.rs:346 indexing_slicing
crates/hl7-server/src/validate.rs:222 string_slice
crates/hl7-server/src/validate.rs:223 string_slice
crates/hl7-server/src/validate.rs:224 string_slice
crates/hl7-soap-server/src/payload.rs:68 string_slice
crates/hl7-soap-server/src/payload.rs:72 string_slice
crates/hl7-soap-server/src/payload.rs:83 arithmetic_side_effects
crates/hl7-soap-server/src/payload.rs:83 string_slice
crates/hl7-soap-server/src/payload.rs:89 arithmetic_side_effects
crates/hl7-soap-server/src/payload.rs:93 arithmetic_side_effects
crates/hl7-soap-server/src/payload.rs:94 arithmetic_side_effects
crates/hl7-soap-server/src/payload.rs:103 string_slice
crates/hl7-soap-server/src/payload.rs:113 string_slice
crates/hl7-soap-server/src/payload.rs:114 arithmetic_side_effects
crates/hl7-soap-server/src/payload.rs:115 arithmetic_side_effects
crates/hl7-soap-server/src/payload.rs:119 string_slice
crates/hl7-soap-server/src/payload.rs:128 string_slice
crates/hl7-soap-server/src/payload.rs:131 arithmetic_side_effects
crates/hl7-soap-server/src/payload.rs:139 string_slice
crates/hl7-soap-server/src/payload.rs:156 arithmetic_side_effects
crates/hl7-soap-server/src/payload.rs:156 string_slice
crates/http-mock-receiver/src/fhir.rs:234 arithmetic_side_effects
crates/http-mock-receiver/src/soap.rs:128 arithmetic_side_effects
crates/http-mock-receiver/src/soap.rs:128 string_slice
crates/http-mock-receiver/src/soap.rs:130 string_slice
crates/http-mock-receiver/src/soap.rs:150 string_slice
crates/http-mock-receiver/src/soap.rs:151 arithmetic_side_effects
crates/http-mock-receiver/src/soap.rs:151 string_slice
crates/http-mock-receiver/src/soap.rs:162 string_slice
crates/http-mock-receiver/src/soap.rs:166 arithmetic_side_effects
crates/http-mock-receiver/src/soap.rs:166 string_slice
crates/http-mock-receiver/src/soap.rs:168 string_slice
crates/ihb-bus/src/fake.rs:106 arithmetic_side_effects
crates/ihb-bus/src/fake.rs:227 arithmetic_side_effects
crates/ihb-bus/src/fake.rs:328 indexing_slicing
crates/ihb-bus/src/live.rs:526 arithmetic_side_effects
crates/ihb-bus/src/live.rs:545 arithmetic_side_effects
crates/ihb-bus/src/live.rs:550 arithmetic_side_effects
crates/ihb-bus/src/live.rs:692 arithmetic_side_effects
crates/ihb-bus/src/renew.rs:161 arithmetic_side_effects
crates/ihb-fields/src/lib.rs:345 arithmetic_side_effects
crates/ihb-fields/src/lib.rs:345 indexing_slicing
crates/ihb-fields/src/lib.rs:555 arithmetic_side_effects
crates/ihb-fields/src/lib.rs:556 arithmetic_side_effects
crates/ihb-fields/src/lib.rs:699 arithmetic_side_effects
crates/ihb-fields/src/lib.rs:699 indexing_slicing
crates/ihb-identity/src/lib.rs:205 as_conversions
crates/ihb-sender/src/throttle.rs:105 arithmetic_side_effects
crates/ihb-sender/src/throttle.rs:164 arithmetic_side_effects
crates/ihb-sender/src/throttle.rs:164 as_conversions
crates/ihb-sql/src/chunk.rs:15 integer_division
crates/ihb-sql/src/connection.rs:157 unreachable
crates/ihb-telemetry/src/clock.rs:160 arithmetic_side_effects
crates/ihb-transformer/src/runtime.rs:262 arithmetic_side_effects
crates/ihb-validation/src/convert.rs:204 string_slice
crates/ihb-validation/src/convert.rs:205 string_slice
crates/ihb-validation/src/convert.rs:212 string_slice
crates/ihb-validation/src/convert.rs:213 arithmetic_side_effects
crates/ihb-validation/src/convert.rs:213 string_slice
crates/ihb-validation/src/convert.rs:225 string_slice
crates/ihb-validation/src/flow.rs:136 indexing_slicing
crates/ihb-validation/src/grammar.rs:305 arithmetic_side_effects
crates/ihb-validation/src/grammar.rs:306 arithmetic_side_effects
crates/ihb-validation/src/grammar.rs:325 arithmetic_side_effects
crates/ihb-validation/src/repair.rs:171 arithmetic_side_effects
crates/ihb-validation/src/repair.rs:194 indexing_slicing
crates/ihb-validation/src/repair.rs:200 arithmetic_side_effects
crates/integration-hub-tester/src/cli.rs:109 indexing_slicing
crates/integration-hub-tester/src/cli.rs:136 indexing_slicing
crates/integration-hub-tester/src/plugins/senders.rs:50 arithmetic_side_effects
crates/integration-hub-tester/src/plugins/senders.rs:103 arithmetic_side_effects
crates/integration-hub-tester/src/plugins/senders.rs:104 arithmetic_side_effects
crates/lookup-service/src/cache.rs:102 arithmetic_side_effects
crates/lookup-service/src/cache.rs:107 arithmetic_side_effects
crates/lookup-service/src/cache.rs:112 arithmetic_side_effects
crates/lookup-service/src/cache.rs:116 arithmetic_side_effects
crates/lookup-service/src/cache.rs:129 arithmetic_side_effects
crates/lookup-service/src/config.rs:158 arithmetic_side_effects
crates/lookup-service/src/file_store.rs:95 integer_division
crates/lookup-service/src/file_store.rs:237 as_conversions
crates/lookup-service/src/file_store.rs:240 integer_division
crates/lookup-service/src/file_store.rs:332 indexing_slicing
crates/lookup-service/src/form.rs:20 indexing_slicing
crates/lookup-service/src/form.rs:22 arithmetic_side_effects
crates/lookup-service/src/form.rs:23 arithmetic_side_effects
crates/lookup-service/src/form.rs:23 indexing_slicing
crates/lookup-service/src/form.rs:24 arithmetic_side_effects
crates/lookup-service/src/form.rs:24 indexing_slicing
crates/lookup-service/src/form.rs:27 arithmetic_side_effects
crates/lookup-service/src/form.rs:27 indexing_slicing
crates/lookup-service/src/form.rs:28 arithmetic_side_effects
crates/lookup-service/src/form.rs:28 indexing_slicing
crates/lookup-service/src/form.rs:29 arithmetic_side_effects
crates/lookup-service/src/form.rs:30 arithmetic_side_effects
crates/lookup-service/src/form.rs:34 arithmetic_side_effects
crates/lookup-service/src/form.rs:42 arithmetic_side_effects
crates/lookup-service/src/form.rs:43 arithmetic_side_effects
crates/lookup-service/src/form.rs:44 arithmetic_side_effects
crates/lookup-service/src/form.rs:105 arithmetic_side_effects
crates/lookup-service/src/form.rs:108 arithmetic_side_effects
crates/lookup-service/src/form.rs:108 indexing_slicing
crates/lookup-service/src/form.rs:120 arithmetic_side_effects
crates/lookup-service/src/form.rs:123 indexing_slicing
crates/lookup-service/src/form.rs:131 arithmetic_side_effects
crates/lookup-service/src/form.rs:133 indexing_slicing
crates/lookup-service/src/form.rs:136 arithmetic_side_effects
crates/lookup-service/src/form.rs:138 indexing_slicing
crates/lookup-service/src/form.rs:140 arithmetic_side_effects
crates/lookup-service/src/form.rs:142 indexing_slicing
crates/lookup-service/src/form.rs:151 arithmetic_side_effects
crates/lookup-service/src/import.rs:82 arithmetic_side_effects
crates/lookup-service/src/import.rs:103 arithmetic_side_effects
crates/lookup-service/src/import.rs:120 arithmetic_side_effects
crates/lookup-service/src/jwks.rs:192 indexing_slicing
crates/lookup-service/src/jwks.rs:212 indexing_slicing
crates/lookup-service/src/jwks.rs:222 arithmetic_side_effects
crates/lookup-service/src/jwks.rs:222 indexing_slicing
crates/lookup-service/src/jwks.rs:233 indexing_slicing
crates/lookup-service/src/jwks.rs:237 arithmetic_side_effects
crates/lookup-service/src/jwks.rs:237 indexing_slicing
crates/lookup-service/src/jwks.rs:241 arithmetic_side_effects
crates/lookup-service/src/jwks.rs:244 indexing_slicing
crates/lookup-service/src/jwks.rs:245 arithmetic_side_effects
crates/lookup-service/src/jwks.rs:245 indexing_slicing
crates/lookup-service/src/pages.rs:463 arithmetic_side_effects
crates/lookup-service/src/pages.rs:580 arithmetic_side_effects
crates/lookup-service/src/pages.rs:581 arithmetic_side_effects
crates/lookup-service/src/security.rs:334 as_conversions
crates/lookup-service/src/security.rs:336 as_conversions
crates/lookup-service/src/security.rs:754 indexing_slicing
crates/lookup-service/src/security.rs:755 arithmetic_side_effects
crates/lookup-service/src/ttl.rs:41 integer_division
crates/lookup-service/src/ttl.rs:57 arithmetic_side_effects
crates/lookup-service/src/ttl.rs:57 integer_division
crates/lookup-service/src/web.rs:69 as_conversions
crates/lookup-service/src/web.rs:449 arithmetic_side_effects
crates/message-replay-job/src/job.rs:223 arithmetic_side_effects
crates/message-replay-job/src/main.rs:174 arithmetic_side_effects
crates/message-replay-job/src/sql.rs:85 arithmetic_side_effects
crates/message-store-service/src/insert.rs:21 arithmetic_side_effects
crates/message-store-service/src/insert.rs:23 arithmetic_side_effects
crates/message-store-service/src/insert.rs:96 arithmetic_side_effects
crates/rest-server/src/xml.rs:146 string_slice
crates/rest-server/src/xml.rs:148 arithmetic_side_effects
crates/rest-server/src/xml.rs:149 string_slice
crates/soap-sender/src/envelope.rs:245 string_slice
crates/soap-sender/src/envelope.rs:246 arithmetic_side_effects
crates/soap-sender/src/envelope.rs:246 string_slice
crates/soap-sender/src/envelope.rs:260 string_slice
crates/soap-sender/src/envelope.rs:264 arithmetic_side_effects
crates/soap-sender/src/envelope.rs:264 string_slice
crates/soap-sender/src/envelope.rs:266 string_slice
crates/ultra7/src/export.rs:75 arithmetic_side_effects
crates/ultra7/src/format.rs:175 arithmetic_side_effects
crates/ultra7/src/iteration.rs:39 indexing_slicing
crates/ultra7/src/iteration.rs:41 indexing_slicing
crates/ultra7/src/iteration.rs:43 indexing_slicing
crates/ultra7/src/iteration.rs:82 arithmetic_side_effects
crates/ultra7/src/iteration.rs:82 indexing_slicing
crates/ultra7/src/run.rs:84 as_conversions
crates/ultra7/src/run.rs:86 arithmetic_side_effects
crates/ultra7/src/run.rs:90 arithmetic_side_effects
crates/ultra7/src/run.rs:92 arithmetic_side_effects
crates/ultra7/src/sender/http.rs:168 indexing_slicing
crates/ultra7/src/sender/http.rs:180 arithmetic_side_effects
crates/ultra7/src/sender/http.rs:180 indexing_slicing
crates/ultra7/src/sender/http.rs:196 indexing_slicing
crates/ultra7/src/sender/http.rs:201 arithmetic_side_effects
crates/ultra7/src/sender/http.rs:201 indexing_slicing
crates/ultra7/src/sender/http.rs:208 indexing_slicing
crates/ultra7/src/sender/http.rs:209 arithmetic_side_effects
crates/ultra7/src/sender/mllp.rs:77 indexing_slicing
crates/xml-fhir-proms-transformer/src/bundle.rs:248 indexing_slicing
crates/xml-fhir-proms-transformer/src/identity.rs:80 arithmetic_side_effects
crates/xml-fhir-proms-transformer/src/identity.rs:87 arithmetic_side_effects
crates/xml-fhir-proms-transformer/src/resources.rs:143 indexing_slicing
crates/xml-fhir-proms-transformer/src/resources.rs:144 indexing_slicing
```
