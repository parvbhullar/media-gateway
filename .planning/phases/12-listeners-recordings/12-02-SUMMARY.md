---
phase: 12-listeners-recordings
plan: "02"
subsystem: api_v1/recordings
tags: [recordings, cdr, rest-api, sea-orm, streaming, integration-tests]
dependency_graph:
  requires: [12-01]
  provides: [REC-01, REC-02, REC-03, REC-04, REC-07]
  affects: [src/handler/api_v1/recordings.rs, tests/api_v1_recordings_core.rs]
tech_stack:
  added: []
  patterns: [sea-orm-paginator, tokio-file-stream, manual-302-response, per-row-storage-classification]
key_files:
  created: [tests/api_v1_recordings_core.rs]
  modified: [src/handler/api_v1/recordings.rs]
decisions:
  - "Built manual 302 response instead of axum Redirect (axum 0.8 has no 302 constructor: to()=303, temporary()=307)"
  - "recording_storage derived per-row from URL shape, not global CdrStorage::is_local()"
  - "recording_size_bytes absent from RecordingView with TODO(phase-13) comment"
  - "Expr::value(None::<String>) used for SeaORM NULL-clear (works without alternative)"
metrics:
  duration: "~10 minutes"
  completed: "2026-05-04"
  tasks: 2
  files: 2
---

# Phase 12 Plan 02: Recordings Core Handlers Summary

**One-liner:** Full recordings CRUD surface (list/get/download/delete) over the CDR table with per-row storage classification, 302 streaming redirect for remote URLs, and 8 green integration tests.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Replace stub recordings.rs with full handler implementation | ec04b00 | src/handler/api_v1/recordings.rs |
| 2 | Integration tests for recordings core (auth, list, get, download, delete) | a05ded2 | tests/api_v1_recordings_core.rs, src/handler/api_v1/recordings.rs (302 fix) |

## Implementation Notes

### SeaORM NULL-clear expression

Used `Expr::value(None::<String>)` — compiles and works correctly with SQLite:

```rust
CdrEntity::update_many()
    .col_expr(CdrColumn::RecordingUrl, Expr::value(None::<String>))
    .col_expr(CdrColumn::UpdatedAt, Expr::value(chrono::Utc::now()))
    .filter(CdrColumn::Id.eq(id))
    .exec(db)
    .await
```

No alternative form was needed.

### Test fixture helper names reused from tests/api_v1_cdrs.rs

- `test_state_empty()` — from `mod common` (common/mod.rs)
- `test_state_with_api_key(name)` — from `mod common`
- `body_json(resp)` — inlined (same pattern as cdrs.rs)
- `bearer(token)` — inlined (same pattern as cdrs.rs)

Seed helpers were **inlined in this test file** (not added to common module):
- `seed_cdr_with_recording(db, url) -> i64` — uses `CdrAm` ActiveModel with `recording_url: Set(Some(...))`
- `seed_cdr_without_recording(db) -> i64` — same with `recording_url: Set(None)`
- `cleanup_cdr(db, id)` — calls `Entity::delete_by_id(id)`

Rationale: these helpers are specific to recordings tests and have no value in the shared common module.

### recording_size_bytes absent from RecordingView

The column does not exist on `rustpbx_call_records` in v2.0. `RecordingView` has:
- `recording_duration_secs: Option<i32>` — column exists, included
- No `recording_size_bytes` field — comment present: `// TODO(phase-13): add recording_size_bytes when column is added to rustpbx_call_records`

### D-09 field names — no deviations

All three renamed fields match the plan exactly:
- `trunk` ← `sip_gateway` column
- `caller` ← `from_number` column
- `callee` ← `to_number` column

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] axum 0.8 has no 302 Found redirect constructor**

- **Found during:** Task 2 (integration test `recordings_download_remote_redirects` failed with 303, then 307)
- **Issue:** `Redirect::to()` emits 303 See Other; `Redirect::temporary()` emits 307 Temporary Redirect. Neither is 302 Found. The plan specified "302 Found" (D-12).
- **Fix:** Built a manual 302 response using `Response::new(Body::empty())` + `*resp.status_mut() = StatusCode::FOUND` + `resp.headers_mut().insert(header::LOCATION, location)`.
- **Files modified:** `src/handler/api_v1/recordings.rs`
- **Commit:** a05ded2 (included in Task 2 commit alongside test file)

## Verification Results

- `cargo build --bin rustpbx` — exits 0, zero warnings
- `cargo test --lib -- recordings::tests` — 3/3 unit tests pass (storage classification + MIME mapping)
- `cargo test --test api_v1_recordings_core` — 8/8 integration tests pass

## Self-Check: PASSED

- src/handler/api_v1/recordings.rs — exists, 345 lines
- tests/api_v1_recordings_core.rs — exists
- Commit ec04b00 — Task 1 feat commit
- Commit a05ded2 — Task 2 test commit (includes 302 fix)
