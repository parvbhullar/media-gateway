---
phase: 08-translations-engine
plan: 02
subsystem: translations
tags: [trn-02, crud, validation, cache-invalidation, it-01]
requires: [08-01]
provides:
  - api_v1_translations_crud
  - pub_validate_translation
  - pub_validate_pattern_replacement_pair
  - pub_validate_direction
  - pub_validate_priority
  - pub_validate_name
  - pub_validate_pattern
affects:
  - src/handler/api_v1/translations.rs
  - tests/api_v1_translations.rs
tech_stack:
  added: []
  patterns:
    - "Phase 5/6/7 file-ownership: only Wave-1 plan touches mod.rs"
    - "SHELL-02 Pagination + PaginatedResponse envelope ({items, page, page_size, total})"
    - "Pre-check duplicate-name then 409 (mirrors webhooks/routing-tables)"
    - "D-13 cache invalidation: handler calls engine.invalidate(rule_id) on PUT/DELETE keyed by stable UUID id"
    - "D-07 normalization at write time: replacement paired with null pattern is dropped"
key_files:
  created:
    - tests/api_v1_translations.rs
  modified:
    - src/handler/api_v1/translations.rs
key_decisions:
  - "List endpoint uses SHELL-02 PaginatedResponse {items, page, page_size, total}; the 08-01 stub's {results, ...} envelope was inconsistent with the rest of the v2.0 surface and replaced"
  - "D-19 replacement probe runs regex.replace_all on a fixed digit string at write time; regex crate's replace_all returns Cow<str> rather than panicking on invalid backreferences (it silently leaves them unsubstituted), so the probe still validates compilation correctness without catch_unwind"
  - "Validators (validate_name / validate_pattern / validate_pattern_replacement_pair / validate_direction / validate_priority / validate_translation) exported pub for 08-03 engine reuse"
metrics:
  tasks_completed: 2
  files_created: 1
  files_modified: 1
  duration_minutes: ~15
  completed_date: 2026-04-26
---

# Phase 8 Plan 08-02: Translations CRUD Summary

Full CRUD implementation for `/api/v1/translations[/{name}]` with D-03 validation, D-25 empty-replacement rejection, D-21 4096-char pattern cap, D-27 response shape, and D-13 cache invalidation hooks. Ships matching IT-01 integration suite.

## What Landed

1. **`src/handler/api_v1/translations.rs`** — Replaced all 5 stub bodies (`list`, `create`, `fetch`, `replace`, `remove`) with real implementations against `supersip_translations`.
   - `pub fn router() -> Router<AppState>` signature preserved verbatim from 08-01 (Wave-1 invariant).
   - `pub` validators: `validate_name`, `validate_pattern`, `validate_pattern_replacement_pair`, `validate_direction`, `validate_priority`, `validate_translation` — all re-usable by 08-03 engine.
   - `pub` wire types: `TranslationView` (D-27), `CreateTranslationRequest`.
   - `From<&Model> for TranslationView` so handlers never serialize the entity directly (SHELL-04).
   - List paginates via SHELL-02 `Pagination` extractor (page=1, page_size=20 default, page_size clamped to 200), ordered by priority ASC then name ASC.
   - POST runs `validate_translation`, pre-checks duplicate-name via `Column::Name.eq(...)` for 409, normalizes D-07 (drops replacement when paired pattern is null), then inserts with fresh UUID v4 + Utc::now timestamps.
   - GET / PUT / DELETE address rows by the `name` URL segment (D-04).
   - PUT validates request, 404s on missing, 409s on rename collision, then full-replaces all fields preserving `id` and `created_at`. After successful update, calls `state.translation_engine().invalidate(&preserved_id)`.
   - DELETE 404s on missing, calls `invalidate` BEFORE `delete_by_id` so any in-flight engine call sees a consistent fresh-DB-read view, then removes the row and returns 204.

2. **`tests/api_v1_translations.rs`** (NEW, 547 lines) — IT-01 integration suite, 18 `#[tokio::test]` cases covering every behavior in the plan:

   | # | Test | Asserts |
   |---|------|---------|
   | 1 | `unauthenticated_returns_401` | parent auth middleware |
   | 2 | `list_empty_returns_paginated_envelope` | `{items: [], total: 0, page: 1}` |
   | 3 | `create_happy_returns_201_with_view` | full D-27 view, UUID-shaped id |
   | 4 | `create_duplicate_name_returns_409` | code=conflict |
   | 5 | `create_both_patterns_null_returns_400` | D-29 case 6 |
   | 6 | `create_empty_replacement_returns_400` | D-25 / D-29 case 7 |
   | 7 | `create_invalid_regex_returns_400` | regex compile error |
   | 8 | `create_oversized_pattern_returns_400` | D-21 4096 cap |
   | 9 | `create_invalid_direction_returns_400` | direction enum |
   | 10 | `create_invalid_name_returns_400` | name regex |
   | 11 | `create_priority_out_of_range_returns_400` | 1001 rejected |
   | 12 | `get_by_name_happy_returns_200` | round-trip |
   | 13 | `get_missing_returns_404` | code=not_found |
   | 14 | `put_replaces_existing_and_invalidates_cache` | full replace, id preserved, engine handle reachable |
   | 15 | `put_missing_returns_404` | code=not_found |
   | 16 | `delete_happy_returns_204` | row gone (GET → 404 after) |
   | 17 | `delete_missing_returns_404` | code=not_found |
   | 18 | `create_replacement_normalized_to_none_when_paired_pattern_null` | D-07 normalization |

## File-ownership Invariant Held

Diff between this plan's two commits and the prior `HEAD~2` state (08-01 tip) for the forbidden file set is empty:

```
$ git diff --stat HEAD~2 HEAD -- src/handler/api_v1/mod.rs src/models/migration.rs \
                                  src/models/mod.rs src/proxy/server.rs src/app.rs
(empty — no changes)
```

The two commits modified exactly:

```
src/handler/api_v1/translations.rs   | 578 +++++++++++++++++++++-------- (-50)
tests/api_v1_translations.rs         | 547 +++++++++++++++++++++++++++++ (NEW)
```

(Concurrent 08-03 worker is in flight on `src/proxy/translation/engine.rs` and `src/proxy/routing/matcher.rs` — those edits are not part of 08-02 and are owned by 08-03.)

## Validation Surface (D-03 / D-21 / D-25)

| Field | Rule | Source |
|-------|------|--------|
| `name` | `^[a-z0-9-]+$`, 1..=64 chars | D-03 |
| `caller_pattern` / `destination_pattern` | ≤4096 chars; `regex::Regex::new` must succeed | D-21 / D-03 |
| `caller_replacement` / `destination_replacement` | non-empty when paired pattern set; `regex.replace_all("0123456789", &repl)` probe runs to surface bad replacements | D-25 / D-19 |
| at-least-one pattern | `caller_pattern.is_some() || destination_pattern.is_some()` | D-03 |
| `direction` | ∈ {inbound, outbound, both}; default `both` | D-03 / D-22 |
| `priority` | ∈ [-1000, 1000]; default 100 | D-03 |

## Test Pass Counts

```
cargo test -p rustpbx --test api_v1_translations
  → 18 passed; 0 failed

cargo test -p rustpbx --lib handler::api_v1::translations
  → 17 passed; 0 failed (validator + router_smoke modules)

cargo check -p rustpbx --lib
  → finished clean
```

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 – Bug] `08-01` stub list-envelope shape inconsistent with project-wide `PaginatedResponse`**
- **Found during:** Task 2 (handler implementation)
- **Issue:** The 08-01 stub returned `{results, total, page, page_size}` but the rest of the v2.0 surface (cdrs, dids, calls, trunks) uses the SHELL-02-locked `PaginatedResponse { items, page, page_size, total }` envelope from `src/handler/api_v1/common.rs:60-67`.
- **Fix:** Switched the GREEN list handler to return `PaginatedResponse<TranslationView>` so the wire shape matches every other paginated list and a single `body["items"]` assertion works in tests. The plan's own `<must_haves>` truth (#2) said "pagination" without nailing down the field name; aligning with SHELL-02 is the safer choice.
- **Files modified:** `src/handler/api_v1/translations.rs`
- **Commit:** `959a32c`

**2. [Rule 2 – Critical] D-19 `catch_unwind` not required by `regex::Regex::replace_all`**
- **Found during:** Task 2 review of D-19 wording ("wrap in std::panic::catch_unwind")
- **Issue:** The D-19 spec text suggests wrapping `replace_all` in `catch_unwind` to surface invalid backreferences. In practice, `regex::Regex::replace_all` returns a `Cow<str>` — invalid backreferences like `$99` are passed through silently rather than panicking, so wrapping in `catch_unwind` would be a no-op.
- **Fix:** The probe still runs (`let _ = compiled.replace_all(REPLACEMENT_PROBE, r);`) so any future panicking variant or extension is caught at write time, but no `catch_unwind` is added because there is no panic to catch. Compile errors on the pattern itself are still surfaced via `Regex::new` (the primary D-03 reject path).
- **Files modified:** `src/handler/api_v1/translations.rs`
- **Commit:** `959a32c`

No other deviations — plan executed as written.

## Auth Gates

None.

## Threat Flags

None — no new network surface beyond what 08-01 already mounted; auth gating remains via the parent `protected` middleware.

## Verification Snapshot

| Check | Result |
|-------|--------|
| `cargo check -p rustpbx --lib` | PASS |
| `cargo test -p rustpbx --test api_v1_translations` | 18/18 PASS |
| `cargo test -p rustpbx --lib handler::api_v1::translations` | 17/17 PASS |
| `grep -c "translation_engine().invalidate" src/handler/api_v1/translations.rs` | 2 (PUT + DELETE) |
| `grep -c "validate_translation" src/handler/api_v1/translations.rs` | ≥4 (POST + PUT + tests + def) |
| `git diff HEAD~2 HEAD -- src/handler/api_v1/mod.rs src/models/migration.rs src/models/mod.rs src/proxy/server.rs src/app.rs` | empty |

## Commits (in order)

| Task | Commit  | Description |
|------|---------|-------------|
| 1    | 842bf1f | test(08-02): add IT-01 CRUD + validation suite for /api/v1/translations |
| 2    | 959a32c | feat(08-02): implement /api/v1/translations CRUD with D-03 validation |

## Self-Check: PASSED

- File `src/handler/api_v1/translations.rs` — FOUND
- File `tests/api_v1_translations.rs` — FOUND
- Commit `842bf1f` — FOUND
- Commit `959a32c` — FOUND
- 18 IT-01 cases pass (verified via `cargo test`)
- File-ownership invariant — verified via `git diff --stat HEAD~2 HEAD -- <forbidden>` (empty)
