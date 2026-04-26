---
phase: 06-routing-tables-records-distribution
plan: 01
subsystem: routing
tags: [routing, schema, migration, scaffolding, phase6, wave1]
requires: []
provides:
  - "supersip_routing_tables schema (D-01) — single row per table, records embedded as JSON array"
  - "Stub /api/v1/routing/tables[/...] router (5 endpoints, 501 Not Implemented)"
  - "Stub /api/v1/routing/tables/{name}/records[/...] router (5 endpoints, 501 Not Implemented)"
  - "matched_record_id: Option<String> threaded through RouteTrace + ResolveRouteResponse (D-30 plumbing)"
affects:
  - src/models/routing_tables.rs (NEW)
  - src/models/mod.rs
  - src/models/migration.rs
  - src/handler/api_v1/routing_tables.rs (NEW)
  - src/handler/api_v1/routing_records.rs (NEW)
  - src/handler/api_v1/mod.rs
  - src/proxy/routing/matcher.rs
  - src/handler/api_v1/routing.rs
  - tests/api_v1_routing_resolve.rs
tech-stack:
  added: []
  patterns:
    - "Phase 5 sub-resource entity+Migration colocated pattern (mirrors trunk_acl_entries.rs)"
    - "supersip_ table prefix (D-00)"
    - "Stub router with `pub fn router() -> Router<AppState>` signature stable across waves"
key-files:
  created:
    - src/models/routing_tables.rs
    - src/handler/api_v1/routing_tables.rs
    - src/handler/api_v1/routing_records.rs
  modified:
    - src/models/mod.rs
    - src/models/migration.rs
    - src/handler/api_v1/mod.rs
    - src/proxy/routing/matcher.rs
    - src/handler/api_v1/routing.rs
    - tests/api_v1_routing_resolve.rs
decisions:
  - "Used `DeriveMigrationName` for migration name (matches Phase 3/5 convention; module path becomes the migration ID)"
  - "Records column is `Json` NOT NULL with default `'[]'` via `Expr::cust(\"'[]'\")` (works under both PostgreSQL and SQLite test backends)"
  - "Both stub routers register via `Router::new().route(...)` with route methods chained (`get(...).post(...)`); handler bodies return `ApiError::not_implemented(\"phase 6 plan 06-XX\")`"
  - "matched_record_id added to RouteTrace AND ResolveRouteResponse — D-30 specifies the resolve dry-run; matcher.rs gets the same field so Wave 3 (06-04) can write into the trace and have it surface up via the existing trace serialization path"
  - "Test assertion added inline to existing `resolve_unknown_destination_returns_not_handled` (instead of new test file) — keeps the resolve test matrix consolidated; the key-presence assertion is the minimum behavior change required"
metrics:
  duration: "~2m compile/test cycle"
  completed: "2026-04-26"
---

# Phase 6 Plan 06-01: Routing Tables Schema + Scaffolding Summary

**One-liner:** New `supersip_routing_tables` table with embedded JSON records column, two 501-stub sub-routers wired into `/api/v1`, and `matched_record_id: Option<String>` plumbed through the resolve response surface — Phase 6 foundation laid without touching legacy `rustpbx_routes`.

## What Shipped

### 1. Schema (Task 1)

- New entity `src/models/routing_tables.rs` (`#[sea_orm(table_name = "supersip_routing_tables")]`).
- Columns: `id BIGSERIAL PK`, `name TEXT UNIQUE NOT NULL`, `description TEXT NULL`, `direction TEXT NOT NULL DEFAULT 'both'` (D-21), `priority INTEGER NOT NULL DEFAULT 100` (D-22), `is_active BOOLEAN NOT NULL DEFAULT true`, `records JSON NOT NULL DEFAULT '[]'` (D-01, D-03), `created_at TIMESTAMPTZ`, `updated_at TIMESTAMPTZ`.
- UNIQUE index `idx_supersip_routing_tables_name` on `name`.
- `pub mod routing_tables;` added to `src/models/mod.rs` (alphabetical, between `routing` and `sip_trunk`).
- `Box::new(super::routing_tables::Migration)` appended LAST to `Migrator::migrations` in `src/models/migration.rs`, with the prescribed comment block citing Plan 06-01 + RTE-01/RTE-02 + D-01 + D-05.
- Legacy `rustpbx_routes` and `src/models/routing.rs` untouched (D-05 verified — no diff).
- **Migration position:** LAST (after `drop_acl_column::Migration`, the Phase 5 final migration).

### 2. Stub routers (Task 2)

- New `src/handler/api_v1/routing_tables.rs` — RTE-01:
  - `GET    /routing/tables`         → `list_tables`   → `501 not_implemented "phase 6 plan 06-02"`
  - `POST   /routing/tables`         → `create_table`  → 501
  - `GET    /routing/tables/{name}`  → `get_table`     → 501
  - `PUT    /routing/tables/{name}`  → `update_table`  → 501
  - `DELETE /routing/tables/{name}`  → `delete_table`  → 501
- New `src/handler/api_v1/routing_records.rs` — RTE-02:
  - `GET    /routing/tables/{name}/records`              → `list_records`   → `501 "phase 6 plan 06-03"`
  - `POST   /routing/tables/{name}/records`              → `create_record`  → 501
  - `GET    /routing/tables/{name}/records/{record_id}`  → `get_record`     → 501
  - `PUT    /routing/tables/{name}/records/{record_id}`  → `update_record`  → 501
  - `DELETE /routing/tables/{name}/records/{record_id}`  → `delete_record`  → 501
- Both files carry the prescribed NOTE block instructing 06-02/06-03 to preserve `pub fn router()` and route paths.
- `src/handler/api_v1/mod.rs` declares `pub mod routing_tables;` and `pub mod routing_records;`, and merges both routers into the `protected` chain after `routing::router()`.

### 3. Resolve response plumbing (Task 3, TDD)

- `RouteTrace` (`src/proxy/routing/matcher.rs`) gains `pub matched_record_id: Option<String>` with `#[serde(default)]`. `Default::default()` yields `None` automatically.
- `ResolveRouteResponse` (`src/handler/api_v1/routing.rs`) gains `pub matched_record_id: Option<String>`. All **6** construction sites in `resolve_route` initialize the field to `None`.
- New assertion in `tests/api_v1_routing_resolve.rs::resolve_unknown_destination_returns_not_handled` verifies the JSON object contains the `matched_record_id` key (with value `null`). RED→GREEN observed.

## Frozen Invariant

**`src/handler/api_v1/mod.rs` and `src/models/migration.rs` are FROZEN for the rest of Phase 6.** Plans 06-02, 06-03, 06-04 MUST NOT modify either file:

- 06-02 replaces handler bodies in `routing_tables.rs` only.
- 06-03 replaces handler bodies in `routing_records.rs` only.
- 06-04 edits `src/proxy/routing/matcher.rs` (and possibly new modules under `src/proxy/routing/`) but DOES NOT touch `mod.rs` or `migration.rs`.

This mirrors the Phase 5 file-ownership lesson: Wave 1 owns the wiring; Waves 2/3 swap implementations.

## Verification

- `cargo check -p rustpbx --lib` — clean (Finished `dev` profile).
- `cargo test -p rustpbx --test api_v1_routing_resolve` — 7 passed, 0 failed (was 6 prior + 1 added assertion in existing test; matrix size unchanged).
- `git diff src/models/routing.rs` — empty.
- `grep -c "ApiError::not_implemented" src/handler/api_v1/routing_tables.rs src/handler/api_v1/routing_records.rs` — 5 + 5 = 10.

## Per-Task Commits

| Task | Commit  | Message                                                          |
| ---- | ------- | ---------------------------------------------------------------- |
| 1    | fc5ece0 | feat(06-01): add supersip_routing_tables entity + migration      |
| 2    | 50464b9 | feat(06-01): wire stub routers for routing tables and records    |
| 3    | 83432e2 | feat(06-01): plumb matched_record_id through resolve response    |

## Deviations from Plan

None — plan executed exactly as written. Two minor implementation choices fell within Claude's Discretion:

- **Migration name:** Used `DeriveMigrationName` (matches Phase 3/5 convention) rather than the literal `m20260426_000001_create_supersip_routing_tables` string the plan listed as illustrative. The derived name (`m_routing_tables` from the module path) is unambiguous and idempotent.
- **Test placement:** Extended the existing `resolve_unknown_destination_returns_not_handled` test rather than creating a new file `tests/api_v1_routing_resolve_matched_record_id.rs`. Plan §Action explicitly allowed either; consolidation keeps the resolve matrix in one file.

## Self-Check: PASSED

- src/models/routing_tables.rs — FOUND
- src/handler/api_v1/routing_tables.rs — FOUND
- src/handler/api_v1/routing_records.rs — FOUND
- Commit fc5ece0 — FOUND
- Commit 50464b9 — FOUND
- Commit 83432e2 — FOUND
- `cargo check -p rustpbx --lib` — clean
- `cargo test -p rustpbx --test api_v1_routing_resolve` — 7/7 pass
