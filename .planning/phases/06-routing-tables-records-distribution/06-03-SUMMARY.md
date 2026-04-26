---
phase: 06-routing-tables-records-distribution
plan: 03
subsystem: api-v1.routing_records
tags: [routing, records, embedded-json, crud, api-v1, phase6, IT-01, RTE-02]
requirements_completed: [RTE-02]
dependency_graph:
  requires:
    - 06-01 (entity supersip_routing_tables, routing_records.rs stub, mod.rs wiring)
  provides:
    - "pub validate_routing_record (consumed by 06-02 & 06-04)"
    - "RoutingRecord / RoutingMatch / RoutingTarget wire types"
    - "/api/v1/routing/tables/{name}/records[/{record_id}] CRUD endpoints"
  affects:
    - "06-02 imports validate_routing_record for initial-records validation"
    - "06-04 imports validate_routing_record as matcher safety net + reuses RoutingMatch/RoutingTarget"
tech_stack:
  added: []
  patterns:
    - "Embedded-document adapter: discrete REST resources backed by a JSON array column (read-modify-write per request)"
    - "Tagged-enum serde with deny_unknown_fields for typed match/target dispatch (T-06-03-08)"
    - "Single source-of-truth validator exported pub for cross-plan reuse"
key_files:
  created:
    - tests/api_v1_routing_records.rs (836 lines, 25 #[tokio::test])
  modified:
    - src/handler/api_v1/routing_records.rs (replaced 06-01 stub with full impl, 533 lines)
decisions:
  - "Sparse positions on DELETE (no renumber) — gaps acceptable per plan"
  - "POST insert-at-index shifts positions of records with position >= idx by +1"
  - "PUT preserves record_id and position (full-replace of payload fields only)"
  - "SSRF denylist enforced at write time (06-04 will repeat at runtime to defend DNS-rebind)"
metrics:
  duration_min: 14
  completed_date: "2026-04-26"
  tasks_completed: 2
  tests_passed: 25
  files_changed: 2
---

# Phase 6 Plan 03: Routing Records CRUD Summary

Element-level CRUD over the `supersip_routing_tables.records` JSON column with stable UUID v4 `record_id` URLs, write-time validation of all 5 match types and 4 target types, and a `pub` validator reusable by sibling plans.

## What Shipped

- **Wire types** (`RoutingRecord`, `RoutingMatch`, `RoutingTarget`, `CompareOp`, `CompareValue`) per CONTEXT D-03, D-08..D-12, D-24 — tagged-enum serde shape with `deny_unknown_fields` on request bodies.
- **5 handlers** (`list_records`, `create_record`, `get_record`, `update_record`, `delete_record`) using a read-modify-write pattern (`load_table` → `parse_records` → mutate `Vec<RoutingRecord>` → `save_records` via SeaORM `ActiveModel.update`).
- **`pub fn validate_routing_record`** — single source of truth enforcing:
  - Regex pattern length cap (4096) + compile check via `regex::Regex::new`
  - HttpQuery URL scheme limited to `http`/`https`; host denylist (`localhost`, 127/8, 10/8, 172.16/12, 192.168/16, 169.254/16, ::1, fc00::/7, fe80::/10) via `is_loopback_or_private_host`
  - HttpQuery `timeout_ms` ≤ 5000 (default 2000)
  - Compare op/value shape correlation (`In` ⇒ `Range`, others ⇒ `Single`)
  - Reject target `code` in 400..=699
- **At-most-one-default invariant** enforced on POST and PUT (D-18, T-06-03-07).
- **Hard cap** 1000 records/table (T-06-03-04).
- **25-test IT-01 scaffold** at `tests/api_v1_routing_records.rs` covering all 5 match types valid/invalid, 4 target types, position semantics (append + insert-and-shift + sparse delete + PUT preserves), default uniqueness, SSRF (localhost/private/non-http), regex DoS (oversized pattern), HttpQuery timeout cap, unknown match/target type tags.

## Endpoints

| Method | Path | Behavior |
| ------ | ---- | -------- |
| GET    | `/api/v1/routing/tables/{name}/records`              | List records, ordered by `position` ASC |
| POST   | `/api/v1/routing/tables/{name}/records`              | Server-gen UUIDv4 `record_id`; `position` None=append, Some(i)=insert+shift |
| GET    | `/api/v1/routing/tables/{name}/records/{record_id}`  | Fetch one |
| PUT    | `/api/v1/routing/tables/{name}/records/{record_id}`  | Replace match/target/is_default/is_active; `record_id` + `position` preserved |
| DELETE | `/api/v1/routing/tables/{name}/records/{record_id}`  | Remove (no position renumber) |

## File Ownership Invariant

| File | Status | Note |
| ---- | ------ | ---- |
| `src/handler/api_v1/routing_records.rs` | modified | Owned by 06-03 |
| `tests/api_v1_routing_records.rs` | created | Owned by 06-03 |
| `src/handler/api_v1/mod.rs` | UNTOUCHED | `git diff` empty (Plan 06-01 invariant) |
| `src/models/migration.rs` | UNTOUCHED | `git diff` empty (Plan 06-01 invariant) |

`src/handler/api_v1/routing_tables.rs` was modified by the parallel Plan 06-02 (its file ownership) — out of scope here.

## Threat Mitigations Verified

| Threat ID | Test |
| --------- | ---- |
| T-06-03-01 (regex DoS) | `post_regex_pattern_too_long_returns_400`, `post_regex_record_validates_at_write_time` |
| T-06-03-02 (SSRF) | `post_http_query_url_localhost_returns_400`, `post_http_query_url_private_ip_returns_400`, `post_http_query_url_non_http_scheme_returns_400` |
| T-06-03-03 (HTTP timeout DoS) | `post_http_query_timeout_above_5000_returns_400` |
| T-06-03-04 (JSON column size) | 1000-record cap enforced; not exercised by test (would require ≥1000 inserts) |
| T-06-03-07 (default uniqueness) | `post_two_defaults_returns_400`, `put_default_when_other_default_exists_returns_400` |
| T-06-03-08 (unknown variant) | `post_unknown_match_type_returns_400`, `post_unknown_target_kind_returns_400` |

## Deviations from Plan

None. Plan executed exactly as written. No auto-fix rules triggered. Sub-validators are inlined into `validate_match`/`validate_target` rather than as separately-named `pub` functions — single `pub fn validate_routing_record` is the documented re-export per the plan's `<interfaces>` block.

## Verification

```
cargo check -p rustpbx --lib            # Finished `dev` profile, 0 errors
cargo test  -p rustpbx --test api_v1_routing_records   # 25 passed; 0 failed
git diff src/handler/api_v1/mod.rs      # empty
git diff src/models/migration.rs        # empty
grep -c "ApiError::not_implemented" src/handler/api_v1/routing_records.rs   # 0 (stubs replaced)
grep "pub fn validate_routing_record"   # found
grep "Uuid::new_v4"                     # found
grep "is_loopback_or_private_host"      # found
```

A pre-existing flake `proxy::proxy_call::session_timer::tests::test_remote_refresh_update` was observed in `cargo test -p rustpbx --lib` under load but passes when run in isolation — unrelated to this plan, out of scope per executor's scope-boundary rule.

## Commits

| Stage | Hash | Message |
| ----- | ---- | ------- |
| RED   | `43e52b1` | test(06-03): add IT-01 RED scaffold for routing records CRUD |
| GREEN | `31b8295` | feat(06-03): implement routing records CRUD with validator (GREEN) |

## Self-Check: PASSED

- File `tests/api_v1_routing_records.rs` exists (FOUND).
- File `src/handler/api_v1/routing_records.rs` exists with `pub fn validate_routing_record` and `Uuid::new_v4` (FOUND).
- Commits `43e52b1` and `31b8295` reachable via `git log --oneline` (FOUND).
- 25/25 tests GREEN, lib check clean.
- Forbidden files (`mod.rs`, `migration.rs`) untouched.
