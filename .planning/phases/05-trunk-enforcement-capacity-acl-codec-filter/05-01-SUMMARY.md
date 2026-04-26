---
phase: 05-trunk-enforcement-capacity-acl-codec-filter
plan: 01
subsystem: trunk-enforcement-storage
tags: [schema, sea-orm, migration, trunk-group, capacity, acl]
requires:
  - rustpbx_trunk_groups (Phase 2)
  - drop_credentials_column pattern (Phase 3 D-02)
provides:
  - supersip_trunk_capacity table (TSUB-04 storage)
  - supersip_trunk_acl_entries table (TSUB-05 storage)
  - drop_acl_column forward-only migration
  - trunk_capacity::router stub (Plan 05-02 fills handlers)
  - trunk_acl::router stub (Plan 05-03 fills handlers)
affects:
  - src/models/trunk_group.rs (acl field + json_null column removed)
  - src/handler/api_v1/trunks.rs (acl stripped from CRUD wire types)
  - src/models/migration.rs (3 new migrations registered)
  - src/handler/api_v1/mod.rs (2 stub modules merged)
tech-stack:
  added: []
  patterns:
    - sub-resource entity pattern (from Phase 3 trunk_credentials/trunk_origination_uris)
    - forward-only drop migration with has_column guard (Phase 3 D-02 precedent)
    - Wave-1 router stubs to enable parallel Wave-2 plan execution
key-files:
  created:
    - src/models/trunk_capacity.rs
    - src/models/trunk_acl_entries.rs
    - src/models/drop_acl_column.rs
    - src/handler/api_v1/trunk_capacity.rs
    - src/handler/api_v1/trunk_acl.rs
    - .planning/phases/05-trunk-enforcement-capacity-acl-codec-filter/05-01-SUMMARY.md
  modified:
    - src/models/mod.rs
    - src/models/migration.rs
    - src/models/trunk_group.rs
    - src/handler/api_v1/trunks.rs
    - src/handler/api_v1/mod.rs
decisions:
  - D-01 enforced at schema: UNIQUE FK on supersip_trunk_capacity.trunk_group_id
  - D-04 enforced at schema: max_calls / max_cps NULL-able (NULL = unlimited)
  - D-10 enforced at schema: ACL promoted from JSON column to multi-row table with UNIQUE (trunk_group_id, rule)
  - D-11 enforced at schema: rustpbx_trunk_groups.acl dropped LAST in migration order
  - D-13 wire format validation deferred to Plan 05-03 handler (NOT in model)
metrics:
  completed: 2026-04-25
  tasks_completed: 5
  files_changed: 11
  commits: 5
---

# Phase 5 Plan 01: Trunk Enforcement Schema (Capacity + ACL) Summary

Added two `supersip_`-prefixed sub-resource tables (`supersip_trunk_capacity`,
`supersip_trunk_acl_entries`) and a forward-only drop of the legacy
`rustpbx_trunk_groups.acl` JSON column, plus empty router stubs in `api_v1/mod.rs`
so Wave-2 plans (05-02, 05-03) can land in parallel without colliding on `mod.rs`.

## What Shipped

### Storage layer
- `supersip_trunk_capacity` — `id`, `trunk_group_id` (UNIQUE FK CASCADE), `max_calls?`, `max_cps?`, timestamps. Index `idx_supersip_trunk_capacity_group_id` UNIQUE.
- `supersip_trunk_acl_entries` — `id`, `trunk_group_id` (FK CASCADE), `rule` char(255), `position` (default 0), `created_at`. Indexes: UNIQUE `(trunk_group_id, rule)` and non-unique `(trunk_group_id, position)`.
- `drop_acl_column` — `has_column`-guarded forward-only ALTER. Down is no-op (mirrors `drop_credentials_column`).

### Model surface
- Removed `pub acl: Option<Json>` from `trunk_group::Model`.
- Removed `.col(json_null(Column::Acl))` from `trunk_group::Migration::up()` CREATE TABLE — fresh DBs never see the column.
- `Column::Acl` enum variant is gone (SeaORM derives Column from Model).

### Migration registration
Appended after `drop_credentials_column` in FK-safe order:

1. `trunk_capacity::Migration`
2. `trunk_acl_entries::Migration`
3. `drop_acl_column::Migration`

### Handler stubs
Empty `pub fn router() -> Router<AppState> { Router::new() }` in:
- `src/handler/api_v1/trunk_capacity.rs`
- `src/handler/api_v1/trunk_acl.rs`

Both declared `pub mod` and `.merge(...)`-ed in `src/handler/api_v1/mod.rs`. The `pub fn router()` signature is the contract Wave-2 plans must preserve.

## Decisions Made
- Adhered to D-01, D-04, D-10, D-11, D-13 — see frontmatter `decisions:` field.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Stripped `acl` from `src/handler/api_v1/trunks.rs` CRUD wire types**
- **Found during:** Task 3 (after removing `pub acl: Option<Json>` from `trunk_group::Model`).
- **Issue:** `cargo check -p rustpbx --lib` failed with `E0609: no field 'acl' on type trunk_group::Model` (line 92) and `E0609: no field 'acl' on type trunk_group::ActiveModel` (line 576), plus `E0560` on `acl: Set(req.acl)` in the create handler.
- **Fix:** Removed `acl` from `TrunkView`, `CreateTrunkRequest`, `UpdateTrunkRequest`, and the create/update active-model construction. Updated comments to point readers at the new sub-resource (`/api/v1/trunks/{name}/acl`, Plan 05-03).
- **Files modified:** `src/handler/api_v1/trunks.rs`
- **Commit:** 7c04640
- **Rationale:** The plan's `<read_first>` for Task 3 cites `03-01-SUMMARY.md` ("explains why Model field AND CREATE TABLE entry must both go") — same pattern applies to the legacy CRUD handler that referenced the now-gone field. Without this fix Task 3's `cargo check` acceptance criterion fails.

No other deviations.

## Verification

- `cargo check -p rustpbx --lib` — clean.
- `cargo test -p rustpbx --lib --no-run` — links cleanly.
- Migration order assertion (`awk` one-liner from Task 4 verify block): `order ok`.
- `grep -r 'Column::Acl' src/` — only one match, in a doc-comment in `drop_acl_column.rs`. No code references.
- `grep -r 'pub acl: Option<Json>' src/` — zero matches.
- `grep 'pub mod trunk_capacity\|pub mod trunk_acl' src/handler/api_v1/mod.rs` — both present.
- `grep 'trunk_capacity::router()\|trunk_acl::router()' src/handler/api_v1/mod.rs` — both present.

## Commits

| Task | Commit  | Subject                                                          |
| ---- | ------- | ---------------------------------------------------------------- |
| 1    | 89aea28 | feat(05-01): add supersip_trunk_capacity entity + migration       |
| 2    | ea654b8 | feat(05-01): add supersip_trunk_acl_entries entity + migration    |
| 3    | 7c04640 | feat(05-01): drop legacy rustpbx_trunk_groups.acl column (D-11)   |
| 4    | 1229bd9 | feat(05-01): register Phase 5 migrations in FK-safe order        |
| 5    | 23611b9 | feat(05-01): add stub trunk_capacity + trunk_acl routers          |

## Hand-off to Wave 2

- Plan 05-02 owns `src/handler/api_v1/trunk_capacity.rs` body — must keep `pub fn router() -> Router<AppState>` signature; do NOT touch `mod.rs`.
- Plan 05-03 owns `src/handler/api_v1/trunk_acl.rs` body — same constraint. ACL wire-format validation (`^(allow|deny) (all|<CIDR>|<IP>)$`, D-13) lives there.
- Wave 3 plan 05-04 wires proxy enforcement against the new tables.

## Self-Check: PASSED

- `src/models/trunk_capacity.rs` — FOUND
- `src/models/trunk_acl_entries.rs` — FOUND
- `src/models/drop_acl_column.rs` — FOUND
- `src/handler/api_v1/trunk_capacity.rs` — FOUND
- `src/handler/api_v1/trunk_acl.rs` — FOUND
- Commits 89aea28, ea654b8, 7c04640, 1229bd9, 23611b9 — all FOUND in `git log`.
