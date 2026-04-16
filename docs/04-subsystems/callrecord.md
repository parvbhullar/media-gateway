# Call Record

## What it does

The callrecord module generates Call Detail Records (CDRs) when calls
complete. It serializes call metadata (caller, callee, timestamps,
status, recording URLs, leg timeline), uploads recordings and CDR JSON
to configurable backends (local disk, S3, HTTP webhook), and runs
post-processing hooks (database persistence, SipFlow upload).

## Key types & entry points

- **`CallRecordHook`** (trait) — async hook called after a CDR is saved; used for database writes, SipFlow uploads, transcript processing. `src/callrecord/mod.rs`
- **`CallRecordFormatter`** (trait) — controls CDR JSON formatting, file naming, transcript paths, and media paths. `src/callrecord/mod.rs`
- **`CallRecord`** — the complete CDR: call_id, timestamps (start/ring/answer/end), caller, callee, status code, hangup reason, recorder media list, leg timeline, and nested `CallDetails`. `src/callrecord/mod.rs`
- **`CallDetails`** — enriched call metadata: direction, status, from/to numbers, agent, queue, department, trunk, recording URL, transcript status, tags, rewrite info. `src/callrecord/mod.rs`
- **`CallRecordManager`** — manages the CDR processing pipeline: receives `CallRecord` via channel, saves via configurable saver function, then runs all hooks. `src/callrecord/mod.rs`
- **`CallRecordManagerBuilder`** — builder with cancel token, config, saver function, formatter, hooks, and optional pending DB for retry. `src/callrecord/mod.rs`
- **`DefaultCallRecordFormatter`** — default implementation that writes CDRs under `{root}/{YYYYMMDD}/{call_id}.json`. `src/callrecord/mod.rs`
- **`CallRecordStats`** — backlog, processed, failed counts and average processing time. `src/callrecord/mod.rs`
- **`LegTimeline`** — ordered list of leg events (added, bridged, transferred, removed) for multi-leg call tracking. `src/callrecord/mod.rs`

## Sub-modules

- `sipflow.rs` — SipFlow integration (SipFlowBuilder, SipFlow session management)
- `sipflow_upload.rs` — SipFlow data upload hook
- `storage.rs` — CDR storage helpers
- `tests.rs` — Unit tests

## Configuration

Config section `[callrecord]` with variants:

- **Local:** `root` directory for CDR files
- **S3:** `vendor`, `bucket`, `region`, `access_key`, `secret_key`, `endpoint`, `with_media`, `keep_media_copy`
- **HTTP:** `url`, `headers`, `with_media`, `keep_media_copy`

## Public API surface

CDRs are queried through the handler layer (API endpoints). The module
itself does not register HTTP routes but provides the `CallRecordSender`
channel for the proxy/call layers to submit completed call records.

## See also

- [sipflow.md](sipflow.md) — SIP flow capture referenced by CDRs
- [upload-retry.md](upload-retry.md) — Retry scheduler for failed S3 uploads
- [storage.md](storage.md) — Object storage abstraction used for S3 uploads
- [handler.md](handler.md) — API endpoints for CDR queries

---
**Status:** ✅ Shipped
**Source:** `src/callrecord/`
**Related phases:** [Phase 7](../07-roadmap/phase-07-callrecord.md), [Phase 11](../07-roadmap/phase-11-cdr.md), [Phase 12](../07-roadmap/phase-12-cdr.md)
**Last reviewed:** 2026-04-16
