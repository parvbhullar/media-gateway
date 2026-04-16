---
phase: 02-trunk-groups-schema-core-crud
plan: 02
completed_at: 2026-04-16
status: complete
closes_requirements:
  - TRK-02
  - TRK-03
  - TRK-04
commits:
  - hash: 3a873bc
    message: "feat(02-02): implement create_trunk with validation and transactional insert"
  - hash: e936287
    message: "feat(02-02): implement update_trunk and delete_trunk with engagement check"
  - hash: da69131
    message: "test(02-02): write-path integration tests for /api/v1/trunks"
key-files:
  modified:
    - src/handler/api_v1/trunks.rs
    - tests/api_v1_trunks.rs
    - Cargo.toml
duration_seconds: 2845
tasks_completed: 3
tasks_total: 3
---

# Phase 2 Plan 02-02: Write CRUD + Gateway Validation + Engagement Delete-Block

Full write surface (POST/PUT/DELETE) for `/api/v1/trunks` with gateway-existence validation, parallel feature-gate, transactional atomic writes, and engagement-tracked delete scanning DIDs and routing records.

## What Was Built

### Task 1: create_trunk with validation (3a873bc)

Replaced the Plan 02-01 501 stub with a full handler. Added:

| Component | Purpose |
|-----------|---------|
| `CreateTrunkRequest` / `CreateTrunkMember` | DTOs with `deny_unknown_fields` |
| `validate_trunk_group_name` | 1-64 chars, alphanumeric + `_` or `-` |
| `parse_direction` | String to `SipTrunkDirection` with 400 on invalid |
| `parse_distribution_mode` | String to `TrunkGroupDistributionMode` with 400 on invalid |
| `validate_distribution_mode` | Rejects `parallel` when `parallel-trunk-dial` feature is disabled |
| `validate_gateway_refs` | IN query against `sip_trunk`, collects missing names |
| `assert_no_gateway_name_collision` | Prevents trunk group name from colliding with gateway namespace |
| `create_trunk` handler | Full transactional insert (group + members) with rollback on failure |

Validation sequence follows CONTEXT.md exactly: name -> direction -> distribution_mode -> gateway_refs -> name_collision -> duplicate_check -> tx insert.

### Task 2: update_trunk + delete_trunk with engagement check (e936287)

| Handler | Behavior |
|---------|----------|
| `update_trunk` | Loads existing, validates mode/direction/gateway_refs, tx: scalar patch + full member replacement |
| `delete_trunk` | Loads existing, runs engagement check, tx: delete members then group |
| `engagement_check_trunk_group` | Step 1: DID scan (indexed `trunk_group_name`). Step 2: Route scan (best-effort JSON parse of `target_trunks` -- Array and Object shapes). Returns 409 on hit. |

The engagement check includes `TODO(phase-6)` marker for replacing the JSON scan with an indexed FK check when RTE-01 lands.

### Task 3: Write-path integration tests (da69131)

23 tests across 5 categories:

| Category | Count | Tests |
|----------|-------|-------|
| Auth 401s | 4 | list, get, create, delete without token |
| Read happy paths | 5 | empty list, seeded list, get by name, 404, member position order |
| TRK-02 write happy paths | 6 | create 201, credentials round-trip, member position order, update replaces members, update patches scalars, delete 204 |
| TRK-03 validation 400s | 6 | empty members, unknown gateway, invalid name, gateway name collision, parallel without feature, update unknown gateway |
| TRK-04 engagement 409s | 3 | DID reference blocks delete, route reference blocks delete, unrelated route does not block |

The two Plan 02-01 501-stub tests were removed.

## Handler Summary

`src/handler/api_v1/trunks.rs`: 732 lines total

- 5 route handlers: `list_trunks`, `get_trunk`, `create_trunk`, `update_trunk`, `delete_trunk`
- 5 validation helpers: `validate_trunk_group_name`, `parse_direction`, `parse_distribution_mode`, `validate_distribution_mode`, `validate_gateway_refs`
- 2 check helpers: `assert_no_gateway_name_collision`, `engagement_check_trunk_group`
- 3 transactional boundaries (one per write handler)

## Verification Results

| Check | Result |
|-------|--------|
| `cargo build -p rustpbx --lib` | Clean |
| `grep -c not_implemented trunks.rs` | 0 |
| `grep -c .begin() trunks.rs` | 3 |
| `grep -c TODO(phase-6) trunks.rs` | 1 |
| `grep -c deny_unknown_fields trunks.rs` | 3 |
| `cargo test --test api_v1_trunks` | 23/23 passing |
| Full regression (10 test files) | 101/101 passing, 0 failures |

## Engagement Scan Strategy

Phase 2 uses a two-step engagement check before delete:

1. **DID scan (indexed):** Single query on `rustpbx_dids.trunk_group_name` column added by Plan 02-01. Efficient indexed lookup.
2. **Route scan (best-effort JSON):** Loads all `rustpbx_routes` rows and iterates over `target_trunks` JSON field, checking both `Value::Array` (any element matches) and `Value::Object` (any value matches) shapes. This is O(routes) but acceptable for Phase 2 volumes.

**Phase 6 follow-up:** When RTE-01 adds a first-class `trunk_group_id` FK column to `rustpbx_routes`, the JSON scan is replaced with an indexed equality check. Tracked via `TODO(phase-6)` comment in `engagement_check_trunk_group`.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Removed unused imports**
- **Found during:** Task 2
- **Issue:** `ModelTrait` and `Column as RouteColumn` imports were unused after implementation
- **Fix:** Removed the two unused imports
- **Files modified:** `src/handler/api_v1/trunks.rs`
- **Commit:** e936287

### Note on Task 3 Commit

The Task 3 commit (da69131) inadvertently included 14 pre-staged documentation files under `docs/07-roadmap/` that were staged from a prior working session. These are unrelated to this plan and do not affect correctness.

## Known Stubs

None. All three Plan 02-01 stubs (create_trunk, update_trunk, delete_trunk) have been replaced with full implementations.

## Self-Check: PASSED

- [x] src/handler/api_v1/trunks.rs exists (732 lines)
- [x] tests/api_v1_trunks.rs exists (23 tests)
- [x] Commit 3a873bc verified in git log
- [x] Commit e936287 verified in git log
- [x] Commit da69131 verified in git log

## EXECUTION COMPLETE

**SUMMARY:** `.planning/phases/02-trunk-groups-schema-core-crud/02-02-SUMMARY.md`
