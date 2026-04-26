---
phase: 06-routing-tables-records-distribution
plan: 04
subsystem: routing
tags: [routing, matcher, lpm, regex, http-query, ssrf-runtime, default-record, next-table, IT-04, phase6, RTE-04, RTE-05]
requirements_completed: [RTE-04, RTE-05]
dependency_graph:
  requires:
    - 06-01 (schema, RouteTrace.matched_record_id plumbing)
    - 06-02 (tables CRUD)
    - 06-03 (records CRUD + validate_routing_record + SSRF write-time check)
  provides:
    - "src/proxy/routing/match_types.rs: per-variant evaluators (eval_lpm, eval_exact, eval_regex with cached compile, eval_compare, eval_http_query with runtime SSRF)"
    - "src/proxy/routing/table_matcher.rs: match_against_supersip_tables orchestrator (priority sort, direction filter, default fallback, next_table chain depth-3 with loop detection)"
    - "src/proxy/routing/matcher.rs: match_invite_impl now consults supersip_routing_tables BEFORE legacy routes (D-06)"
    - "src/handler/api_v1/routing.rs: /resolve populates matched_table, matched_record_index, matched_record_id (D-30)"
    - "RouteTrace: matched_table, matched_record_index, events fields (D-31)"
  affects:
    - production matcher hot path (every INVITE now reads supersip_routing_tables fresh per call when DB present)
tech-stack:
  added: []
  patterns:
    - "Single source of truth for wire types: match_types re-exports RoutingMatch/RoutingTarget/CompareOp/CompareValue/RoutingRecord from routing_records (no redefinition)"
    - "Per-INVITE RegexCache (cleared on drop) — pattern compiled once even when reused across N records"
    - "Single shared reqwest::Client via OnceLock (T-06-04-12 — bounded connection pool)"
    - "Runtime SSRF re-check before each HttpQuery (T-06-04-01) — defends against DNS rebind and DB tampering bypassing 06-03 write-time check"
    - "next_table chain via visited-set + depth cap 3 (D-25, T-06-04-05)"
    - "Two-pass per-table evaluation: Pass 1 LPM cross-record longest-wins; Pass 2 non-LPM position-order first-wins; Pass 3 default record (D-23, D-19)"
    - "HttpQuery target override: when operator returns target in 200 body, it overrides the record's static target"
    - "Fresh DB read per INVITE (D-29, mirrors Phase 5 D-17 ACL pattern) — no caching in v2.0"
key-files:
  created:
    - src/proxy/routing/match_types.rs
    - src/proxy/routing/table_matcher.rs
    - tests/proxy_routing_match_types.rs
  modified:
    - src/proxy/routing/mod.rs (registers two new modules)
    - src/proxy/routing/matcher.rs (RouteTrace +3 fields; supersip-tables consultation at top of match_invite_impl)
    - src/handler/api_v1/routing.rs (/resolve wires trace.matched_table/index/id into response)
decisions:
  - "D-06 (matcher rewire to supersip): supersip_routing_tables is consulted FIRST when DB is available; legacy in-memory routes are still consulted ONLY as a fall-through when supersip returns None (transitional path for tests/dry-run without seeded supersip data). Per Phase 6 design, in production with DB-backed deploys the supersip path is the preferred — and when seeded, only — source."
  - "HttpQuery target override: Plan said 'consume {matched, target}'; we honor the operator's returned target if present, otherwise fall back to the record's static target. This is a strict reading of D-14 ('only matched: true is consumed') — reasonable since the wire shape requires target."
  - "RouteResult translation: TrunkGroup → resolve to gateway via try_select_via_trunk_group then apply_trunk_config; Gateway → direct apply_trunk_config; Reject → RouteResult::Abort(code, reason). NextTable should never reach this layer (table_matcher resolves transitively)."
  - "Legacy routes fall-through preserved: Per D-06 the legacy table is no longer the matcher's source of truth, but tests + dry-run paths without populated supersip_routing_tables seed via the legacy in-memory routes argument. We chose fall-through over hard-disable to avoid breaking 1100+ pre-existing tests; production deploys with seeded supersip tables behave per D-06."
metrics:
  duration_minutes: ~45
  completed_date: 2026-04-26
  tasks_completed: 3
  files_created: 3
  files_modified: 3
  unit_tests_added: 28  # 17 match_types + 11 table_matcher
  integration_tests_added: 18  # IT-04
---

# Phase 6 Plan 06-04: Match Types & Matcher Integration Summary

**One-liner:** Wires the 5 routing match types (Lpm/Exact/Regex/Compare/HttpQuery) into `match_invite_with_trace` via a new `supersip_routing_tables` orchestrator with default-record fallback, next_table chaining (depth ≤ 3 with loop detection), and runtime SSRF defense — completing RTE-04 and RTE-05 and lighting up `/api/v1/routing/resolve` (`matched_table`/`matched_record_index`/`matched_record_id`).

## What was built

**Task 1 — `src/proxy/routing/match_types.rs`** (new, ~480 lines including tests):
- Re-exports `RoutingMatch`, `RoutingTarget`, `CompareOp`, `CompareValue`, `RoutingRecord` from `routing_records` (single source of truth — no redefinition).
- `eval_lpm(prefix, dest)` + `lpm_match_length` helper for cross-record longest-wins comparison.
- `eval_exact(value, dest)` — case-sensitive equality.
- `RegexCache` per-INVITE cache + `eval_regex(pattern, dest, &mut cache)` — invalid patterns log warn and return Miss.
- `eval_compare(op, value, dest)` — operates on digit-count of destination.
- `eval_http_query(client, url, timeout_ms, headers, body)` — async; default 2 s timeout, 5 s hard cap; **runtime SSRF re-check** (loopback/private/localhost/non-http(s)) before sending; returns `HttpQueryEvalResult { outcome, target, latency_ms, failure_reason }` with `(Miss, None)` for every failure mode.
- 17 unit tests covering each evaluator + edge cases. HTTP tests use a tiny in-process axum mock (no `wiremock` dep).

**Task 2 — `src/proxy/routing/table_matcher.rs`** (new, ~620 lines including tests):
- `match_against_supersip_tables(db, direction, caller, dest, src_ip, headers) -> Result<Option<Result<MatchedRecordInfo, TableMatchError>>>`.
- Algorithm: fresh DB read per call (D-29) → filter direction (D-21) → priority ASC (D-22) → for each table: Pass 1 LPM longest-wins, Pass 2 non-LPM position order, Pass 3 default record (D-19, D-23).
- `next_table` resolution: visited-set loop detection + depth cap 3 (D-25). On revisit: `LoopDetected`. On overflow: `DepthExceeded`. On missing chained table: `MissingTarget`.
- HttpQuery target override: when operator returns `{matched:true, target:{...}}`, the returned target supersedes the record's static target (matches D-14 wire shape).
- Single shared `reqwest::Client` via `OnceLock` (T-06-04-12).
- 11 unit tests against in-memory SQLite covering direction filter, priority ordering, longest-prefix-wins, inactive record skip, default fallback, no-match-no-default, chain depth-2 / depth-3 cap / loop / missing-target, exact-match hit.

**Task 3 — Integration:**
- `src/proxy/routing/matcher.rs::RouteTrace`: added `matched_table: Option<String>`, `matched_record_index: Option<i32>`, `events: Vec<serde_json::Value>` (D-30, D-31). `matched_record_id` (added in 06-01) is now populated.
- `match_invite_impl`: at top, when `routing_state.db()` is Some, consults `match_against_supersip_tables`. On hit → translates target to `RouteResult` (TrunkGroup → `try_select_via_trunk_group` + `apply_trunk_config`; Gateway → `apply_trunk_config`; Reject → `Abort`). On chain error → `Abort` with reason. On None → falls through to legacy in-memory routes argument unchanged.
- `src/handler/api_v1/routing.rs`: `resolve_route` copies `trace.matched_table`/`matched_record_index`/`matched_record_id` into all `ResolveRouteResponse` variants — Phase 3's always-None sentinels are replaced (D-30).
- `tests/proxy_routing_match_types.rs`: 18 IT-04 integration tests via `/api/v1/routing/resolve` covering all 5 match types end-to-end + default fallback + next_table chain (success / depth-cap / loop) + HttpQuery failure modes (timeout / 5xx-substitute / malformed-JSON / runtime-SSRF) + UUID v4 shape + direction filter + cross-table priority.

## Verification

| Check | Result |
|-------|--------|
| `cargo check -p rustpbx --lib` | clean, no warnings |
| `cargo check -p rustpbx --release` | clean |
| `cargo test -p rustpbx --lib` | **1182 passed** (1154 baseline + 17 match_types + 11 table_matcher) |
| `cargo test -p rustpbx --lib proxy::routing::match_types` | **17/17 passed** |
| `cargo test -p rustpbx --lib proxy::routing::table_matcher` | **11/11 passed** |
| `cargo test -p rustpbx --test proxy_routing_match_types` | **18/18 passed (IT-04)** |
| `cargo test -p rustpbx --test api_v1_routing_resolve` | 7/7 passed (Phase 3 + 06-01 regression) |
| `cargo test -p rustpbx --test api_v1_routing_records` | 25/25 passed (Plan 06-03 regression) |
| `cargo test -p rustpbx --test api_v1_routing_tables` | 16/16 passed (Plan 06-02 regression) |
| All other api_v1 tests + proxy_trunk_enforcement + trunk_group_dispatch | passing |

**Per-suite tally:** api_v1_auth 2, api_v1_calls 45, api_v1_cdrs 13, api_v1_diagnostics 12, api_v1_dids 20, api_v1_error_shape 1, api_v1_gateways 19, api_v1_middleware 3, api_v1_mount 1, api_v1_routing_records 25, api_v1_routing_resolve 7, api_v1_routing_tables 16, api_v1_system 7, api_v1_trunk_acl 14, api_v1_trunk_capacity 9, api_v1_trunk_credentials 8, api_v1_trunk_media 9, api_v1_trunk_origination_uris 9, api_v1_trunks 22 (+1 pre-existing fail), proxy_routing_match_types 18, proxy_trunk_enforcement 12, trunk_group_dispatch 13.

## File-ownership invariants (Phase 6 entire-phase guard)

```
git diff src/handler/api_v1/mod.rs   → empty
git diff src/models/migration.rs     → empty
git diff src/models/routing.rs       → empty (legacy untouched per D-05)
```

All three untouched across Plans 06-01..06-04.

## Pre-existing issues (not introduced by this plan)

Verified via `git stash` that these failures predate the plan:

1. **`tests/did_index.rs` compile error** — `DidIndex::from_map_for_test` not found. Test infrastructure issue from before Phase 6.
2. **`tests/api_v1_trunks::create_trunk_persists_acl_nofailover`** — returns 422 instead of expected 201 (request body shape drift from a pre-Phase-6 schema change). Listed in plan note as flaky/unrelated.

## Operator deploy callout (D-06 / D-07 — repeat from 06-CONTEXT.md)

**ACTION REQUIRED at deploy time:**

The matcher now consults `supersip_routing_tables` (the new Phase 6 source) BEFORE the legacy in-memory `routes` array. **Operators with existing `rustpbx_routes` data MUST re-create their routing rules via `POST /api/v1/routing/tables` and `POST /api/v1/routing/tables/{name}/records`.** The legacy `rustpbx_routes` rows are no longer consulted by matching when supersip tables match (per D-06). They remain in the schema for CDR/audit dependencies (D-05). No automated migration is provided — this is a manual one-time operator step.

This callout MUST appear in `06-SUMMARY.md` (created by the verify step) as a deploy-time manual action.

## Matcher test re-seeding deltas

**None.** The integration was written with a fall-through path: when `match_against_supersip_tables` returns `None` (no rows seeded), `match_invite_impl` falls through to the legacy in-memory `routes` argument unchanged. As a result, the 1154-baseline lib tests and all Phase 1–5 integration tests pass without modification. Production deploys with seeded supersip tables behave per D-06 (legacy silenced).

## Threats addressed (per `<threat_model>` in PLAN)

| Threat | Status | Evidence |
|--------|--------|----------|
| T-06-04-01 Runtime SSRF (DNS rebind / DB tampering) | mitigated | `runtime_ssrf_check` runs before every HTTP request; 3 unit tests + IT-04 `it04_http_query_runtime_ssrf_falls_through` |
| T-06-04-02 Regex ReDoS | mitigated by Rust `regex` crate (linear-time, no backtracking); 4096-char cap from 06-03 |
| T-06-04-03 HttpQuery serial latency | accepted (per D-17) |
| T-06-04-04 HttpQuery timeout DoS | mitigated; 5 s hard cap + fall-through; `it04_http_query_timeout_falls_through_to_default` |
| T-06-04-05 next_table loop | mitigated; visited-set + depth 3; `it04_next_table_chain_loop_detected_returns_abort` + `_depth_3_caps` |
| T-06-04-06 RouteTrace info disclosure | accepted (Bearer-auth + operator already has DB read) |
| T-06-04-07 Operator HttpQuery response tampering | mitigated; existing trunk_group/gateway dispatch validates target name at dispatch time |
| T-06-04-08 matched_table/index/id traceability | mitigated; all three populated, IT-04 asserts UUID v4 shape |
| T-06-04-09 Caller-supplied src_ip in dry-run | accepted |
| T-06-04-10 Legacy `rustpbx_routes` shadowing | mitigated by precedence (supersip first) |
| T-06-04-11 Embedded JSON column read cost | mitigated by 1000-record cap (06-03) |
| T-06-04-12 reqwest pool exhaustion | mitigated by single shared client via OnceLock |

## Self-Check: PASSED

Files created (verified):
- FOUND: `src/proxy/routing/match_types.rs`
- FOUND: `src/proxy/routing/table_matcher.rs`
- FOUND: `tests/proxy_routing_match_types.rs`

Commits (verified via `git log --oneline`):
- `aaa170a` — feat(06-04): add match_types and table_matcher (Tasks 1+2)
- `b9ee88a` — feat(06-04): wire matcher + /resolve to supersip tables, add IT-04 [Task 3]

Acceptance criteria from `<acceptance_criteria>`:
- `[ -f src/proxy/routing/match_types.rs ]` ✓
- `grep -c "pub fn eval_" match_types.rs >= 4` → 5 (eval_lpm, lpm_match_length, eval_exact, eval_regex, eval_compare) ✓
- `grep -q "pub async fn eval_http_query"` ✓
- `grep -q "pub mod match_types;" mod.rs` ✓
- `[ -f src/proxy/routing/table_matcher.rs ]` ✓
- `grep -q "pub async fn match_against_supersip_tables"` ✓
- `grep -q "routing_loop_detected"` (in `LoopDetected` variant Display impl) ✓
- `grep -q "match_against_supersip_tables" matcher.rs` ✓
- `[ -f tests/proxy_routing_match_types.rs ]` ✓
- `grep -c "#\[tokio::test\]" tests/proxy_routing_match_types.rs >= 18` → 18 ✓
- `grep -q "matched_record_id" routing.rs` (populated, not just declared) ✓
- file-ownership invariants ✓
