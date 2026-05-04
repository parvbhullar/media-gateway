---
phase: 12-listeners-recordings
plan: "03"
subsystem: api_v1/recordings
tags: [recordings, export, bulk-delete, zip, sea-orm, integration-tests, async-zip]
dependency_graph:
  requires: [12-02]
  provides: [REC-05, REC-06]
  affects: [src/handler/api_v1/recordings.rs, tests/api_v1_recordings_export_bulk.rs]
tech_stack:
  added: [async_zip=0.0.17 (tokio+deflate feature, added in 12-01)]
  patterns: [zip-vec-accumulation, dry-run-confirm-guardrail, update_many-null-clear, option-json-extractor]
key_files:
  created: [tests/api_v1_recordings_export_bulk.rs]
  modified: [src/handler/api_v1/recordings.rs]
decisions:
  - "async_zip 0.0.17 with_tokio() takes ownership (not mutable ref); Cursor<Vec<u8>> used as backing store because it implements tokio::io::AsyncWrite; close() returns Compat<Cursor<Vec<u8>>> requiring two .into_inner() calls to recover Vec<u8>"
  - "Vec<u8> accumulation chosen over streaming unfold (bounded by 10k cap; simpler error handling)"
  - "ZIP entry naming uses underscore separator per D-17: {cdr_id}_{call_id}.{ext}"
  - "Option<Json<ExportBody>> extractor rejects empty body when Content-Type: application/json is set; tests send no Content-Type for empty-body POSTs"
  - "/export and /bulk routes registered before /{id} in router() per axum literal-segment requirement"
metrics:
  duration: "~20 minutes"
  completed: "2026-05-04"
  tasks: 2
  files: 2
---

# Phase 12 Plan 03: Recordings Export and Bulk Delete Summary

**One-liner:** ZIP export (REC-05) with 10k cap and MANIFEST.json, plus bulk delete (REC-06) with dry-run ?confirm=true guardrail, backed by 5 green integration tests.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Add handle_export and handle_bulk_delete to recordings.rs | 90ce4c5 | src/handler/api_v1/recordings.rs |
| 2 | Integration tests for export and bulk delete | 6ba21f6 | tests/api_v1_recordings_export_bulk.rs |

## Implementation Notes

### async_zip 0.0.17 API — actual usage

Resolved version: `0.0.17` (exact match to Cargo.toml spec).

The plan suggested `ZipFileWriter::with_tokio(&mut buf)` with a `Vec<u8>`. The actual API:

```rust
// with_tokio() takes ownership of a T: tokio::io::AsyncWrite + Unpin.
// Vec<u8> does NOT implement tokio::io::AsyncWrite.
// std::io::Cursor<Vec<u8>> DOES implement tokio::io::AsyncWrite.
let cursor = std::io::Cursor::new(Vec::<u8>::new());
let mut zip = ZipFileWriter::with_tokio(cursor);  // takes ownership

// ... write entries ...

// close() consumes zip and returns Result<Compat<Cursor<Vec<u8>>>>.
// The Compat wrapper comes from tokio_util::compat::TokioAsyncWriteCompatExt.
let compat_out = zip.close().await?;
let buf: Vec<u8> = compat_out.into_inner().into_inner();
// First .into_inner(): Compat<Cursor<Vec<u8>>> -> Cursor<Vec<u8>>
// Second .into_inner(): Cursor<Vec<u8>> -> Vec<u8>
```

Import path: `async_zip::base::write::ZipFileWriter` (not `async_zip::tokio::write::ZipFileWriter` which is just a type alias).

### Vec<u8> accumulation approach

Vec<u8>/Cursor accumulation was used (not streaming unfold). Rationale:
- Bounded by 10k hard cap (D-19) — peak memory is bounded
- Simpler error handling vs. streaming
- Plan explicitly permits this approach ("Vec<u8> accumulation is acceptable for v2.0")

### ZIP entry naming

No deviations from D-17. Entry name format: `{cdr_id}_{call_id}.{ext}` with underscore separator.

### MANIFEST.json always present

Every export response (even 0-row exports) includes a `MANIFEST.json` entry:
```json
{ "exported": [], "skipped_remote": [] }
```

### body_bytes helper

Added inline in `tests/api_v1_recordings_export_bulk.rs` (not in `tests/common/mod.rs`):
```rust
async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024)
        .await
        .expect("read body bytes")
        .to_vec()
}
```
Rationale: specific to the export test; no value in the shared common module.

### Test helper names reused from 12-02

- `test_state_empty()` — from `tests/common/mod.rs`
- `test_state_with_api_key(name)` — from `tests/common/mod.rs`
- `body_json(resp)` — inlined (same pattern as core test)
- `bearer(token)` — inlined (same pattern as core test)
- `seed_cdr_with_recording(db, url)` — inlined (same implementation as core test)
- `cleanup_cdr(db, id)` — inlined (same implementation as core test)

### Route registration order confirmed

```rust
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/recordings", get(list_recordings))
        .route("/recordings/export", post(handle_export))       // literal before {id}
        .route("/recordings/bulk",   delete(handle_bulk_delete)) // literal before {id}
        .route("/recordings/{id}", get(get_recording).delete(delete_recording))
        .route("/recordings/{id}/download", get(handle_download))
}
```

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] async_zip API differs from plan's suggested pattern**

- **Found during:** Task 1 (compile error)
- **Issue:** Plan specified `ZipFileWriter::with_tokio(&mut buf)` with `Vec<u8>`. Actual API takes ownership; `Vec<u8>` does not implement `tokio::io::AsyncWrite`. Also `close()` returns `Result<Compat<Cursor<Vec<u8>>>>`, not `Result<Cursor<Vec<u8>>>`.
- **Fix:** Used `std::io::Cursor<Vec<u8>>` as backing store; called `.into_inner().into_inner()` twice to recover `Vec<u8>` from `Compat<Cursor<Vec<u8>>>`.
- **Files modified:** `src/handler/api_v1/recordings.rs`
- **Commit:** 90ce4c5

**2. [Rule 1 - Bug] Content-Type: application/json with empty body causes axum 400**

- **Found during:** Task 2 (export_empty_set_returns_zip returned 400)
- **Issue:** axum's `Option<Json<ExportBody>>` extractor rejects empty body when `Content-Type: application/json` is set (JSON parse error on empty string). The plan's test template sent Content-Type unnecessarily.
- **Fix:** Removed `Content-Type` header from empty-body POST requests in tests. `Option<Json<...>>` correctly resolves to `None` when Content-Type is absent.
- **Files modified:** `tests/api_v1_recordings_export_bulk.rs`
- **Commit:** 6ba21f6 (inline fix alongside test file)

**3. [Rule 1 - Bug] Unused import `ActiveModelTrait` added by plan's import block**

- **Found during:** Task 1 (compiler warning → error would have been emitted on `--deny warnings`)
- **Fix:** Removed `ActiveModelTrait` from imports.
- **Files modified:** `src/handler/api_v1/recordings.rs`
- **Commit:** 90ce4c5

## Verification Results

- `cargo build --bin rustpbx` — exits 0, zero warnings
- `cargo test --lib -- recordings::tests` — 5/5 pass (3 from 12-02 + 2 new cap tests)
- `cargo test --test api_v1_recordings_export_bulk` — 5/5 pass

## Threat Surface Scan

No new network endpoints or auth paths beyond those specified in the plan's threat model. The `/recordings/export` and `/recordings/bulk` routes are inside the existing Bearer sub-router (T-12-03-01 mitigated). No new trust boundaries introduced.

## Known Stubs

None. Both handlers are fully wired: export reads from DB and produces a valid ZIP; bulk delete queries, files best-effort removal, and updates the DB. No placeholder data.

## Self-Check: PASSED

- src/handler/api_v1/recordings.rs — exists, contains handle_export and handle_bulk_delete
- tests/api_v1_recordings_export_bulk.rs — exists, 295 lines, 5 test functions
- Commit 90ce4c5 — Task 1 feat commit
- Commit 6ba21f6 — Task 2 test commit
