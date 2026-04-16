---
phase: 03-trunk-sub-resources-l1-routing-resolve
plan: 01
completed_at: 2026-04-17
status: complete
closes_requirements:
  - TSUB-01
  - TSUB-02
  - TSUB-03
  - RTE-03
commits:
  - hash: 5e1af7a
    message: "feat(03-01): Task 1 -- schema migrations for supersip_trunk_credentials + origination_uris + media_config column + drop credentials column"
  - hash: dc7ff2c
    message: "feat(03-01): Task 2 -- sub-router 501 stubs + mod.rs wiring + Phase 2 test split"
key-files:
  created:
    - src/models/trunk_credentials.rs
    - src/models/trunk_origination_uris.rs
    - src/models/add_media_config_column.rs
    - src/models/drop_credentials_column.rs
    - src/handler/api_v1/trunk_credentials.rs
    - src/handler/api_v1/trunk_origination_uris.rs
    - src/handler/api_v1/trunk_media.rs
    - src/handler/api_v1/routing.rs
  modified:
    - src/models/trunk_group.rs
    - src/models/mod.rs
    - src/models/migration.rs
    - src/handler/api_v1/trunks.rs
    - src/handler/api_v1/mod.rs
    - tests/api_v1_trunks.rs
duration_seconds: 1850
tasks_completed: 3
tasks_total: 3
dependency_graph:
  requires:
    - Phase 2 schema (rustpbx_trunk_groups, rustpbx_trunk_group_members)
    - Phase 2 api_v1 protected router
    - ApiError::not_implemented (Phase 1)
  provides:
    - supersip_trunk_credentials table (TSUB-01 storage)
    - supersip_trunk_origination_uris table (TSUB-02 storage)
    - rustpbx_trunk_groups.media_config column (TSUB-03 storage)
    - 4 mounted 501-stub sub-routers for Plans 03-02..03-05 to fill in
  affects:
    - rustpbx_trunk_groups schema (added media_config, dropped credentials)
    - trunks.rs write handlers (no longer accept/return credentials)
tech-stack:
  patterns:
    - SeaORM DeriveEntityModel + belongs_to relation for sub-resource entities
    - manager.has_column + Table::alter::drop_column for forward-only column drop
    - supersip_ prefix on new tables (D-00 LOCKED)
    - .merge() Router composition for sub-router mounting
key-decisions:
  - "D-02 LOCKED: credentials column drop runs LAST in Migrator order to let in-flight reads succeed during deploy"
  - "D-00 LOCKED: new tables use supersip_ prefix; cross-prefix FKs to rustpbx_trunk_groups.id are intentional until v2.1 bulk rename"
  - "media_config column added to fresh-DB CREATE TABLE so new installs skip the alter migration; alter remains as idempotent upgrade path"
---

# Phase 3 Plan 03-01: Schema Foundation + 4 Stub Sub-Routers Summary

Schema-first Wave 1 of Phase 3: locks all DB migrations (2 new tables + 1 added column + 1 dropped column) and all 4 sub-router mount points in one cohesive plan so that Plans 03-02..03-05 can fill in handler bodies in parallel without touching `mod.rs` or `migration.rs`.

## What Was Built

### Task 1: Schema Additions + Removals (commit 5e1af7a)

**New tables (supersip_ prefix per D-00):**

| Table | Columns | Indexes |
|-------|---------|---------|
| `supersip_trunk_credentials` | id (i64 PK auto), trunk_group_id (i64 FK CASCADE), realm (String 255), auth_username (String 255), auth_password (String 255), created_at | UNIQUE (trunk_group_id, realm), non-unique (trunk_group_id) |
| `supersip_trunk_origination_uris` | id, trunk_group_id (FK CASCADE), uri (String 500), position (i32 default 0), created_at | UNIQUE (trunk_group_id, uri), non-unique (trunk_group_id, position) |

FK names: `fk_supersip_trunk_credentials_group_id`, `fk_supersip_trunk_origination_uris_group_id`. Both reference `rustpbx_trunk_groups.id` — cross-prefix FKs are intentional (D-00) until the v2.1 bulk-rename milestone.

**Schema mutations on `rustpbx_trunk_groups`:**

| Change | Migration file | Guard |
|--------|----------------|-------|
| ADD column `media_config` JSON nullable (TSUB-03, D-09) | `add_media_config_column.rs` | `has_column` idempotent |
| DROP column `credentials` JSON (D-02 LOCKED, destructive but safe) | `drop_credentials_column.rs` | `has_column` idempotent; runs LAST in Migrator order |

Also applied: the `credentials` field was removed from `trunk_group::Model`, and `media_config: Option<Json>` was added. The `Credentials` column was also removed from the `trunk_group::Migration` CREATE TABLE block (fresh DBs never create it); `media_config` was added there so fresh DBs receive the final-state schema without needing to run the additive alter.

**Migration registration order (load-bearing):**

```rust
// Phase 3 Plan 03-01 — TSUB-01..03 sub-resource schema.
// Order is load-bearing:
//   1. Create new sub-resource tables (FK to existing rustpbx_trunk_groups.id)
//   2. Add media_config column (additive, idempotent)
//   3. Drop credentials column LAST so any in-flight reads succeed during deploy
Box::new(super::trunk_credentials::Migration),
Box::new(super::trunk_origination_uris::Migration),
Box::new(super::add_media_config_column::Migration),
Box::new(super::drop_credentials_column::Migration),
```

`awk` assertion `tc<tou && tou<amc && amc<dcc` returns `order ok`.

**Handler updates (`src/handler/api_v1/trunks.rs`):**

- `TrunkView`: removed `credentials` field
- `view_from()`: removed the `credentials: group.credentials` line
- `CreateTrunkRequest` and `UpdateTrunkRequest` DTOs: removed `credentials` fields
- `create_trunk`: removed `credentials: Set(req.credentials)` from ActiveModel; added `media_config: Set(None)` (a Plan 03-04 PUT sets it later)
- `update_trunk`: removed the `if let Some(v) = req.credentials {...}` block

### Task 2: Sub-Router Stubs + Wiring + Phase 2 Test Split (commit dc7ff2c)

**4 new stub sub-routers**, each `.merge()`-ed into the `protected` `/api/v1` Router (Bearer-auth layered):

| File | Routes | Stub status | Downstream plan |
|------|--------|-------------|-----------------|
| `src/handler/api_v1/trunk_credentials.rs` | `GET/POST /trunks/{name}/credentials`, `DELETE /trunks/{name}/credentials/{realm}` | all 501 | 03-02 |
| `src/handler/api_v1/trunk_origination_uris.rs` | `GET/POST /trunks/{name}/origination_uris`, `DELETE /trunks/{name}/origination_uris/{uri}` | all 501 | 03-03 |
| `src/handler/api_v1/trunk_media.rs` | `GET /trunks/{name}/media`, `PUT /trunks/{name}/media` | all 501 | 03-04 |
| `src/handler/api_v1/routing.rs` | `POST /routing/resolve` | 501 | 03-05 |

Each handler returns `ApiError::not_implemented("<resource> — Plan 03-0X")`. Route paths are FROZEN — downstream plans replace only handler bodies.

**`src/handler/api_v1/mod.rs` — 4 new `pub mod` declarations + 4 new `.merge()` calls:**

```rust
let protected: Router<AppState> = Router::new()
    .merge(gateways::router())
    .merge(dids::router())
    .merge(cdrs::router())
    .merge(diagnostics::router())
    .merge(system::router())
    .merge(trunks::router())
    .merge(trunk_credentials::router())        // Phase 3 — TSUB-01
    .merge(trunk_origination_uris::router())   // Phase 3 — TSUB-02
    .merge(trunk_media::router())              // Phase 3 — TSUB-03
    .merge(routing::router())                  // Phase 3 — RTE-03
    ;
```

Placeholder comments `// Plan 2: .merge(routing::router())` and `// Plan 3: .merge(security::router())` removed.

**Phase 2 test split (D-02 LOCKED):**

| Before | After |
|--------|-------|
| `create_trunk_persists_credentials_acl_nofailover` | `create_trunk_persists_acl_nofailover` |
| POST body included `"credentials": creds` | POST body no longer carries `credentials` |
| GET round-trip asserted `body["credentials"]["auth_username"] == "user1"` | assertion removed; ACL + nofailover checks preserved |

A top-of-file comment block documents the split and points to `tests/api_v1_trunk_credentials.rs` (to be created by Plan 03-02) for the rebuilt credentials-sub-resource happy-path test.

### Task 3: Phase 2 Regression Gate (verification only, no commit)

Task 3 is a pure verification pass — no source changes. Results:

| Test binary | Passed | Failed |
|-------------|-------:|-------:|
| `api_v1_auth` | 2 | 0 |
| `api_v1_cdrs` | 13 | 0 |
| `api_v1_dids` | 12 | 0 |
| `api_v1_diagnostics` | 20 | 0 |
| `api_v1_error_shape` | 1 | 0 |
| `api_v1_gateways` | 19 | 0 |
| `api_v1_middleware` | 3 | 0 |
| `api_v1_mount` | 1 | 0 |
| `api_v1_system` | 7 | 0 |
| `api_v1_trunks` | 23 | 0 |
| `trunk_group_dispatch` | 13 | 0 |
| **Total** | **114** | **0** |

Phase 2 baseline of 114 tests is preserved exactly. Test count is unchanged because the `create_trunk_persists_acl_nofailover` rename is a rename-in-place (D-02 split), not a removal — Plan 03-02 will rebuild the credentials coverage in a new file.

## Verification Results

| Check | Result |
|-------|--------|
| `cargo check -p rustpbx --lib` | Clean, zero errors |
| Migration registration order assertion | `order ok` |
| Phase 2 baseline tests (11 binaries) | 114/114 passing, 0 failed |
| `api_v1_trunks` suite specifically | 23/23 passing |
| MIG-01 grep: no `rustpbx_sip_trunks` in new migration files | 0 matches across all 4 new files |
| Lingering `trunk_group::Column::Credentials` in `src/` | 0 matches |
| Lingering `*.credentials` trunk_group reads in `src/` | 0 matches |

## MIG-01 Evidence

```
$ for f in src/models/trunk_credentials.rs src/models/trunk_origination_uris.rs \
           src/models/add_media_config_column.rs src/models/drop_credentials_column.rs; do
      echo "=== $f ==="
      grep -c 'rustpbx_sip_trunks' "$f" || echo 0
  done
=== src/models/trunk_credentials.rs ===
0
=== src/models/trunk_origination_uris.rs ===
0
=== src/models/add_media_config_column.rs ===
0
=== src/models/drop_credentials_column.rs ===
0
```

Zero `ALTER TABLE rustpbx_sip_trunks` in any new migration file. The only ALTER TABLE statements target `rustpbx_trunk_groups` (add_media_config_column, drop_credentials_column) and all guards use `has_column` for idempotency.

## Phase 2 Test Split Note

The Phase 2 test `create_trunk_persists_credentials_acl_nofailover` was renamed to `create_trunk_persists_acl_nofailover`. The credentials half (POST body field + GET round-trip assertion) was removed because:

1. The `credentials` column no longer exists on `rustpbx_trunk_groups` (D-02 drop).
2. `POST /api/v1/trunks` no longer accepts a `credentials` field (DTO removed).
3. Credentials are now multi-row, managed via `/api/v1/trunks/{name}/credentials` (sub-resource).

Plan 03-02 will create `tests/api_v1_trunk_credentials.rs` with a happy-path test that POSTs `{realm, username, password}` to the sub-resource endpoint, restoring full credentials coverage at the sub-resource layer.

## Known Stubs

All four new handler files return 501 by design. These are NOT bugs — they are route-mounts that Plans 03-02..03-05 will fill in:

| File | Stub count | Target plan |
|------|-----------:|-------------|
| `src/handler/api_v1/trunk_credentials.rs` | 3 | 03-02 |
| `src/handler/api_v1/trunk_origination_uris.rs` | 3 | 03-03 |
| `src/handler/api_v1/trunk_media.rs` | 2 | 03-04 |
| `src/handler/api_v1/routing.rs` | 1 | 03-05 |

Each stub's `ApiError::not_implemented` message references the downstream plan so the hand-off is traceable from curl output.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Dropped `Credentials` from trunk_group::Migration CREATE TABLE**
- **Found during:** Task 1 cargo check
- **Issue:** The plan's action 1.5 proposed keeping `json_null(Column::Credentials)` in the CREATE TABLE block and relying on `drop_credentials_column.rs` to remove it for fresh DBs. However, SeaORM derives the `Column` enum from the `Model` struct fields — removing `pub credentials: Option<Json>` from the Model removes the `Credentials` variant from `Column`, so `Column::Credentials` becomes a compile error if left in the migration.
- **Fix:** Dropped both the Model field AND the CREATE TABLE `json_null(Column::Credentials)` entry. `drop_credentials_column.rs` becomes a no-op on fresh DBs (the `has_column` guard sees no such column), and does its work only on Phase-2-era DBs being upgraded. This is the correct semantic and keeps the test matrix green.
- **Files modified:** `src/models/trunk_group.rs`
- **Commit:** 5e1af7a (Task 1)

### Pre-existing Out-of-Scope Issue (documented, NOT fixed per scope rules)

- `tests/did_index.rs` references `DidIndex::from_map_for_test(map)` which does not exist on `DidIndex`. This is a pre-existing compilation error in the Phase 2 test suite and is unrelated to Plan 03-01. Verified by `git stash && cargo check -p rustpbx --tests` producing the same error at base commit `5e1af7a`'s parent. Logged for Phase 2 maintenance; Plan 03-01 test runs use scoped `--test <name>` invocations that bypass this file.

## Hand-off Notes for Wave 2 (Plans 03-02..03-05)

1. **Sub-router route paths are FROZEN.** Plans 03-02..03-05 replace only the handler function bodies. Do not touch `src/handler/api_v1/mod.rs` or `src/models/migration.rs` in Wave 2 — zero file overlap with other Wave-2 plans is the whole point of this front-loaded plan.

2. **Schema is READY.** `supersip_trunk_credentials` and `supersip_trunk_origination_uris` tables exist after `Migrator::up()`. `rustpbx_trunk_groups.media_config` is an `Option<Json>` ready to read/write. `rustpbx_trunk_groups.credentials` no longer exists.

3. **Entity ActiveModels follow `..Default::default()` pattern** (confirmed in Plan 02-01 hand-off). Sub-resource writes should use:
   ```rust
   trunk_credentials::ActiveModel {
       trunk_group_id: Set(group.id),
       realm: Set(req.realm),
       auth_username: Set(req.username),
       auth_password: Set(req.password),
       created_at: Set(Utc::now()),
       ..Default::default()
   }
   ```

4. **FK CASCADE is configured** — deleting a trunk group automatically deletes its credentials and origination_uris rows. No manual cleanup needed in `delete_trunk`.

5. **UNIQUE indexes enforce keying** — POST to sub-resource endpoints that violates `(trunk_group_id, realm)` or `(trunk_group_id, uri)` raises a SeaORM DB error; translate to `ApiError::conflict(...)` (409) per D-04 / D-07.

6. **Parent-trunk-missing (404)** — every sub-resource handler must first resolve the `{name}` path segment via `TrunkGroupEntity::find().filter(Column::Name.eq(name))`, 404 on miss, before any sub-resource operation. Copy the pattern from `get_trunk` in `trunks.rs:393`.

7. **Plan 03-02 TSUB-01 test (`tests/api_v1_trunk_credentials.rs`)** should include a credentials round-trip test that replaces the coverage lost in the Phase 2 split — POST `{realm, username, password}` then GET and assert the realm/username in the list response.

8. **Plan 03-05 (/routing/resolve)** routing.rs only has ONE route (POST /routing/resolve). It reuses `RoutingState::new_with_db(server.database.clone())` per D-17 and `match_invite_with_trace` per D-13. Fixture pattern: copy `tests/trunk_group_dispatch.rs::matcher_level_trunk_group_dispatch`.

## Self-Check: PASSED

- `src/models/trunk_credentials.rs`: FOUND
- `src/models/trunk_origination_uris.rs`: FOUND
- `src/models/add_media_config_column.rs`: FOUND
- `src/models/drop_credentials_column.rs`: FOUND
- `src/handler/api_v1/trunk_credentials.rs`: FOUND
- `src/handler/api_v1/trunk_origination_uris.rs`: FOUND
- `src/handler/api_v1/trunk_media.rs`: FOUND
- `src/handler/api_v1/routing.rs`: FOUND
- commit `5e1af7a`: FOUND
- commit `dc7ff2c`: FOUND

## EXECUTION COMPLETE

SUMMARY: `/Users/parvbhullar/Drives/Vault/Projects/Unpod/super-voice/media-gateway/.planning/phases/03-trunk-sub-resources-l1-routing-resolve/03-01-SUMMARY.md`
