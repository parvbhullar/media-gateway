---
phase: 05-trunk-enforcement-capacity-acl-codec-filter
plan: 02
subsystem: api_v1
tags: [trunk-capacity, sub-resource, tsub-04, tsub-07]
requires:
  - 05-01  # supersip_trunk_capacity entity, migration, stub router
provides:
  - GET  /api/v1/trunks/{name}/capacity (D-04 wire shape)
  - PUT  /api/v1/trunks/{name}/capacity (D-05 upsert + 0-rejection)
affects:
  - src/handler/api_v1/trunk_capacity.rs (replaced stub body)
  - tests/api_v1_trunk_capacity.rs (new)
tech_stack:
  added: []
  patterns:
    - "Phase-3 sub-resource lookup_trunk_group_id helper (404 on parent miss)"
    - "Manual upsert via find_capacity_row → ActiveModel::update OR ::insert"
    - "u32 wire / Option<i32> DB cast with try_from saturating to i32::MAX"
key_files:
  created:
    - tests/api_v1_trunk_capacity.rs
  modified:
    - src/handler/api_v1/trunk_capacity.rs
decisions:
  - "D-01: missing capacity row == unlimited; UNIQUE FK on trunk_group_id"
  - "D-04: GET shape {max_calls, max_cps, current_active, current_cps_rate}; live counts placeholder 0 (Plan 05-04)"
  - "D-05: PUT body both optional; 0 rejected with 'use null for unlimited'"
  - "D-22: 9-test matrix (auth, parent-404, defaults, upsert, null, 0-rejected calls+cps, replace, PUT parent-404)"
metrics:
  tasks_completed: 2
  files_created: 1
  files_modified: 1
  tests_added: 9
  tests_passing: 9
  duration_minutes: ~6
---

# Phase 5 Plan 02: Trunk Capacity API Summary

CRUD half of TSUB-04 (capacity persistence) and the response-shape half of
TSUB-07 (live counts) for `/api/v1/trunks/{name}/capacity`, replacing the
Plan 05-01 stub with a working GET+PUT handler and a 9-case integration
suite.

## What shipped

- **`src/handler/api_v1/trunk_capacity.rs`** — full handler; replaces the
  6-line empty-router stub Plan 05-01 placed for parallel-wave file
  ownership. The `pub fn router() -> Router<AppState>` signature is
  preserved exactly so the existing `.merge(trunk_capacity::router())`
  call in `mod.rs` (Plan 05-01) continues to compile untouched.
  - `TrunkCapacityView { max_calls: Option<u32>, max_cps: Option<u32>,
    current_active: u32, current_cps_rate: u32 }` — D-04 wire shape.
  - `PutTrunkCapacityRequest { max_calls: Option<u32>, max_cps: Option<u32> }`
    with `#[serde(deny_unknown_fields)]`.
  - `validate_capacity` enforces D-05: `Some(0)` → 400 with the substring
    `use null for unlimited`. `u32` field type fends off negatives at the
    serde boundary.
  - `lookup_trunk_group_id` copied verbatim from `trunk_credentials.rs` —
    parent-missing → 404 before any capacity-table I/O.
  - `find_capacity_row` + manual update-or-insert dispatch on the
    UNIQUE-FK row. Insert path stamps `created_at`/`updated_at`; update
    path bumps only `updated_at`.
  - `u32 → i32` cast uses `i32::try_from(v).unwrap_or(i32::MAX)` so a
    pathological 2^31..2^32-1 caller cannot wrap into a negative DB
    value. (Operator-relevant max ≪ i32::MAX in practice.)
  - `current_active` / `current_cps_rate` return **0** with a
    `TODO(Plan 05-04)` pointing at the registry / token-bucket wiring.
- **`tests/api_v1_trunk_capacity.rs`** — 9 `#[tokio::test]` cases:
  1. auth gate (401 without Bearer)
  2. GET parent-missing → 404
  3. GET no-row → defaults `{null,null,0,0}` (D-04)
  4. PUT happy → 200 + GET round-trip with zeros for live counts
  5. PUT both null → 200; GET shows both null (unlimited)
  6. PUT `max_calls=0` → 400 with `use null for unlimited`
  7. PUT `max_cps=0` → 400 with `use null for unlimited`
  8. PUT replaces existing row (100/10 → 50/null upsert)
  9. PUT parent-missing → 404
  - Fixture helpers (`insert_trunk`, `insert_trunk_group`, `body_json`,
    `auth_header`) inline-copied from `tests/api_v1_trunk_credentials.rs`
    per the project convention.

## Decisions made

- **D-04 placeholder zeros.** Returning `0`/`0` for the live counts now,
  with a `TODO(Plan 05-04)` comment, lets operators integrate against
  the wire shape immediately and keeps Plan 05-04 a strict additive
  change (replace two literals with snapshot reads).
- **u32 saturation cast.** Chose `try_from(...).unwrap_or(i32::MAX)`
  rather than 400-rejecting >i32::MAX values: serde already constrains
  to u32 (no negatives), and the saturation behavior is observable but
  harmless because operator workloads don't approach 2 G concurrent
  calls per trunk. Documented inline.
- **Manual upsert vs SeaORM `on_conflict`.** Manual fetch+dispatch keeps
  the response-shape round-trip in one transaction-equivalent call and
  matches the Phase-3 sub-resource style operators already grok.

## Verification results

- `cargo test -p rustpbx --test api_v1_trunk_capacity` → **9 passed; 0 failed**
- `cargo check -p rustpbx --lib` → clean
- `git diff --name-only` for this plan does **NOT** include
  `src/handler/api_v1/mod.rs` (file-ownership invariant satisfied for
  Wave-2 parallel execution).
- All Plan 05-02 acceptance-criteria greps verified:
  - `pub fn router()` × 2 (declaration + module-doc reference)
  - `pub struct TrunkCapacityView` × 1
  - `pub struct PutTrunkCapacityRequest` × 1
  - `use null for unlimited` × 2 (one per validator branch)
  - `TODO(Plan 05-04)` × 1
  - `/trunks/{name}/capacity` × 4 (route + comments)

## Deferred Issues

1. **Pre-existing failure unrelated to this plan:**
   `tests/api_v1_trunks.rs::create_trunk_persists_acl_nofailover` is
   failing **422 vs 201** because the POST `/trunks` body now carries
   `acl` and `nofailover_sip_codes` fields that are owned by **Plan
   05-03** (running in parallel in this wave). Plan 05-02 does not
   touch `trunks.rs` nor `mod.rs`, so this is squarely 05-03's surface.
   Logged to `deferred-items.md`.
2. **Live counts wiring** — `current_active` and `current_cps_rate` are
   placeholder zeros. Plan 05-04 owns the wiring to
   `ActiveProxyCallRegistry` + `TrunkCapacityState` token bucket.
   Tracked via `TODO(Plan 05-04)` comment in source.

## Deviations from Plan

None — plan executed exactly as written, including the file-ownership
invariant (mod.rs untouched) and the acceptance-criteria gates.

## Commits

- `e56ad1b` test(05-02): add failing tests for /trunks/{name}/capacity
- `6c3009f` feat(05-02): implement /trunks/{name}/capacity GET+PUT (TSUB-04, TSUB-07)

## Self-Check: PASSED

- `tests/api_v1_trunk_capacity.rs` exists.
- `src/handler/api_v1/trunk_capacity.rs` exists and exports the three
  required symbols.
- Commits `e56ad1b` and `6c3009f` present in `git log`.
- `src/handler/api_v1/mod.rs` not in this plan's diff.
