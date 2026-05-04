---
phase: 12-listeners-recordings
plan: "04"
subsystem: migrations-documentation
tags: [documentation, migrations, mig-02, audit]
dependency_graph:
  requires: []
  provides: [docs/MIGRATIONS.md]
  affects: []
tech_stack:
  added: []
  patterns: [migration-audit-table, forward-only-policy, d-26-d-27-d-28]
key_files:
  created:
    - docs/MIGRATIONS.md
  modified: []
decisions:
  - "routing_tables down() is drop_table (reversible=yes), contrary to plan pre-audit grouping it with forward-only; actual file is source of truth per D-28"
  - "*.md gitignore pattern required git add -f for docs/MIGRATIONS.md; no .gitignore changes made"
metrics:
  duration: "~10 minutes"
  completed: "2026-05-04"
  tasks: 1
  files_created: 1
  files_modified: 0
---

# Phase 12 Plan 04: Migration Audit (MIG-02) Summary

**One-liner:** Complete 44-migration audit table in docs/MIGRATIONS.md classifying each as reversible (27) or forward-only (17) with rollback rationale, closing MIG-02.

## Tasks Completed

| # | Task | Commit | Files |
|---|------|--------|-------|
| 1 | Audit all migrations and produce docs/MIGRATIONS.md | 492d86c | docs/MIGRATIONS.md |

## Final Row Count

- **Migrator::migrations() entries:** 44 (entries 1-44 in src/models/migration.rs lines 7-77)
- **docs/MIGRATIONS.md data rows:** 44
- **Match:** Yes — every registered migration has exactly one table row

## Classification Summary

| Classification | Count |
|----------------|-------|
| Reversible (yes) | 27 |
| Forward-only (no) | 17 |
| Total | 44 |

## Deviations from Plan

### Auto-fixed Issues

None — no code changes were made.

### Discrepancy: routing_tables down() vs planner pre-audit

**Found during:** Task 1 (spot-check read of src/models/routing_tables.rs)

**Planner pre-audit said:** "webhooks, translations, manipulations, security_rules, security_blocks, routing_tables → down() is Ok(()) no-op with explicit 'Forward-only per Phase N D-XX convention'"

**Actual file (source of truth per D-28):** `routing_tables.rs` down() calls:
```rust
manager.drop_table(Table::drop().table(Entity).to_owned()).await
```

This is a fully reversible `drop_table` — NOT a no-op.

**Impact on MIGRATIONS.md:** Row 39 (`routing_tables`) is classified as `reversible=yes` with `down_summary="drop_table supersip_routing_tables"`. The plan template row incorrectly showed it as forward-only.

**Impact on counts:** 27 reversible (not 26 as in plan template), 17 forward-only (not 18).

**No code change required** — down() is correctly implemented. Documentation reflects actual behavior.

### Git .gitignore Deviation

The project `.gitignore` contains a `*.md` pattern (line 34) that blocked `git add docs/MIGRATIONS.md`. Used `git add -f` to force-track this intentional documentation file. No changes to `.gitignore` were made to avoid affecting other ignored patterns.

## Verified down() Implementations

All migrations verified by reading source files before writing table rows:

| Migration | down() Implementation | Verified |
|-----------|----------------------|---------|
| presence | drop_table presence_states | yes (file read) |
| queue (addon) | drop_table rustpbx_queues | yes (file read) |
| rbac | drop_table user_roles, role_permissions, roles (FK-safe order) | yes (file read) |
| add_leg_timeline_column | Ok(()) no-op | yes (file read) |
| drop_credentials_column | Ok(()) with comment re: forward-only | yes (file read) |
| routing_tables | drop_table supersip_routing_tables | yes (file read) — DIFFERS from pre-audit |
| webhooks | Ok(()) with Phase 6 D-05 comment | yes (file read) |
| translations | Ok(()) with Phase 6 D-05 comment | yes (file read) |
| manipulations | Ok(()) with Phase 6 D-05 / Phase 8 comment | yes (file read) |
| security_rules | Ok(()) with Phase 6 D-05 / Phase 8 comment | yes (file read) |
| security_blocks | Ok(()) with Phase 6 D-05 / Phase 8 comment | yes (file read) |

## Broken down() Methods Found (D-28 exception)

None. No down() methods contain panics, compile errors, or unconditional Err returns.
All no-op Ok(()) returns are valid forward-only declarations.

## Rust Source Changes

None. This plan is documentation-only. No Rust files were modified.

## Known Stubs

None. docs/MIGRATIONS.md is a static audit document with no data sources to wire.

## Self-Check: PASSED

- [x] docs/MIGRATIONS.md exists: confirmed
- [x] Commit 492d86c exists: confirmed
- [x] Row count = 44: verified via grep -c
- [x] All acceptance criteria met: test -f, grep counts, header row, Forward-only section, Maintenance Instructions
