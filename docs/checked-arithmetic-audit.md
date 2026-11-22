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
| `lookup-service` | **0** (was 64) | | | | | | |
| `db-sender` | **0** (was 62) | | | | | | |
| `dashboard` | **0** (was 32) | | | | | | |
| `ultra7` | **0** (was 20) | | | | | | |
| `hl7-soap-server` | **0** (was 17) | | | | | | |
| `hl7-pims-transformer` | **0** (was 16) | | | | | | |
| `ihb-validation` | **0** (was 13) | | | | | | |
| `http-mock-receiver` | **0** (was 11) | | | | | | |
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

**Done:** `http-mock-receiver` (`soap::extract_payload` on remainders; identical to the offset version
on 400,000 generated texts), `ihb-validation` (`drop_empty_elements` compared with the offset version on 400,000
generated texts; `insertion_point` counts from the end instead of `+ 1`), `hl7-pims-transformer` (a network-supplied chunk size was added to unchecked, as in
`lookup-service`; the retry back-off now ends retrying instead of saturating), `hl7-soap-server` (`payload::extract` on `get`/`checked_*`; compared with the
offset version on 800,000 generated texts), `ultra7` (its stale-range and lenient-chunk fallbacks are kept and listed under
decisions below), `dashboard` (§7.4 of its spec; 32 findings and the saturating/clamp uses
except the three decisions listed there), `lookup-service` (denied at the crate root; `jwks.rs` had a network-reachable
overflow in the chunk-size arithmetic, fixed with tests) and `db-sender` (denied at the crate root; `sanitize.rs` rewritten on remainder
slices, `msh.rs` salvage dropped, spec/10 §7.4 and §7.5).

## Found by grep, not by Clippy (RCA-ENF-5)

`saturating_*`, `wrapping_*`, `overflowing_*` and `clamp` are forbidden outright
(RCA-INT-4, RCA-EXC-4) but are not lints, so the table above does not count them.
`rg -w --type rust '(saturating|wrapping|overflowing)_\w+|\.clamp\('` on production
code (2026-11-22):

| Where | What | Needs |
|---|---|---|
| `hl7-pims-transformer/src/reference_data.rs:504` | `2_u32.saturating_pow(retry - 1)` | done: `backoff_before` is `Option`, retrying ends |
| `ultra7/src/iteration.rs:65-66` | `saturating_mul`, `saturating_add`, `try_from(..).unwrap_or(0)` | done: checked, original unchanged on overflow |
| `hl7-rest-server/src/http.rs:120` | `limit.saturating_mul(6).saturating_add(1024)` | checked, refuse on overflow |
| `dashboard/src/contacts.rs:454` | `(page.max(1) - 1).saturating_mul(per_page)` | **decision**: `page=0` is invalid input (RCA-IN-2) |
| `dashboard/src/azure/token.rs:195` | `Instant::now() + expires_in.saturating_sub(..)` | `checked_*`, declared fallback |
| `dashboard/src/web.rs:444` | `.clamp(1, MAX_THROUGHPUT_HOURS)` | **decision**: an out-of-range query parameter is an error |
| `ihb-sender/src/main_loop.rs:85`, `throttle.rs:158,166` | `saturating_sub`, `allowable.clamp(1, ..)` | declared floor, or error |
| `dashboard/src/azure/email.rs:164` | `backoff * 2_u32.saturating_pow(attempt)` | declared ceiling |
| `hl7-server/src/listener.rs:199` | `len().saturating_sub(buffered())` | `checked_sub`, the invariant is the point |
| `lookup-service/src/security.rs:753` | `len().saturating_sub(1)` | done: `der_integer` rewritten |

## Decisions needed (rejection currently becomes a substitute, RCA-IN-2 / RCA-IN-7)

Changing these alters behaviour, so they need a spec decision first and are
**not** changed by the arithmetic pass:

- `ultra7/src/iteration.rs`, `ultra7/src/sender/http.rs`: an out-of-range iteration range
  returns the content unchanged, and a damaged chunked reply is read leniently
  (both on purpose for a tool that sends and reads broken messages; spec/ultra7 §4.2).
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
crates/fhir-sender/src/client.rs:283 arithmetic_side_effects
crates/fhir-sender/src/client.rs:303 arithmetic_side_effects
crates/fhir-sender/src/client.rs:316 arithmetic_side_effects
crates/fhir-sender/src/client.rs:329 arithmetic_side_effects
crates/fhir-sender/src/client.rs:490 string_slice
crates/fhir-sender/src/client.rs:494 string_slice
crates/hl7-lims-wrrs-transformer/src/classifier.rs:93 string_slice
crates/hl7-mock-receiver/src/server.rs:117 indexing_slicing
crates/hl7-rest-server/src/ack.rs:124 string_slice
crates/hl7-sender/src/client.rs:220 indexing_slicing
crates/hl7-server/src/ack.rs:241 string_slice
crates/hl7-server/src/listener.rs:171 indexing_slicing
crates/hl7-server/src/listener.rs:189 indexing_slicing
crates/hl7-server/src/listener.rs:346 indexing_slicing
crates/hl7-server/src/validate.rs:222 string_slice
crates/hl7-server/src/validate.rs:223 string_slice
crates/hl7-server/src/validate.rs:224 string_slice
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
crates/integration-hub-tester/src/cli.rs:109 indexing_slicing
crates/integration-hub-tester/src/cli.rs:136 indexing_slicing
crates/integration-hub-tester/src/plugins/senders.rs:50 arithmetic_side_effects
crates/integration-hub-tester/src/plugins/senders.rs:103 arithmetic_side_effects
crates/integration-hub-tester/src/plugins/senders.rs:104 arithmetic_side_effects
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
crates/xml-fhir-proms-transformer/src/bundle.rs:248 indexing_slicing
crates/xml-fhir-proms-transformer/src/identity.rs:80 arithmetic_side_effects
crates/xml-fhir-proms-transformer/src/identity.rs:87 arithmetic_side_effects
crates/xml-fhir-proms-transformer/src/resources.rs:143 indexing_slicing
crates/xml-fhir-proms-transformer/src/resources.rs:144 indexing_slicing
```
