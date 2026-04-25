---
phase: 04-active-calls-mid-call-control
plan: 05
subsystem: active-calls
tags: [api-v1, recording, transcribe, phase-4-signoff]
requires:
  - 04-01-PLAN (CallCommandPayload::Record variant + adapter stub)
  - 04-04-PLAN (map_command_result extra-payload param + dispatch_console_command helper)
  - src/config.rs::Config::recorder_path
provides:
  - "POST /api/v1/calls/{id}/record handler with auto-path generation, traversal-safe explicit paths, format validation, transcribe marker side-effect"
  - "Phase 4 sign-off: 12 mid-call control routes, all CALL-01..10 closed"
affects:
  - tests/common/mod.rs (new test_state_with_recorder helper)
tech-stack:
  added: []
  patterns:
    - "Filesystem path validation: absolute + no '..' + starts_with(recorder_root)"
    - "Side-effect-before-dispatch ordering for transcribe marker (so Phase 7 observers see the marker even if dispatch fails)"
    - "Per-test isolated recorder root via std::env::temp_dir() to avoid CWD-dependent ./config/recorders fallback"
key-files:
  created: []
  modified:
    - src/call/adapters/console_adapter.rs    # Record arm filled (was the last `not yet wired` stub)
    - src/handler/api_v1/calls.rs             # +RecordRequest, +record_call, +resolve_recording_path, +validate_recording_format, +maybe_drop_transcribe_marker
    - tests/api_v1_calls.rs                   # +9 integration tests
    - tests/common/mod.rs                     # +test_state_with_recorder helper
decisions:
  - "Path validation uses string-prefix `starts_with(recorder_root)` with no canonicalize() — symlink-escape inside the recorder dir is ACCEPTED risk per T-04-05-03 (deferred to v2.1 hardening). Production deployments must keep the recorder dir free of unverified symlinks."
  - "Transcribe marker is a HINT, not a contract — best-effort std::fs::File::create; failure is logged but does NOT block 200. Phase 7 webhook consumers must treat markers idempotently."
  - "Marker is dropped BEFORE dispatch so Phase 7 observers see it even if dispatch fails (409/500). Phase 7 must tolerate stray markers from failed records."
metrics:
  duration: "single session"
  completed: "2026-04-25"
  tasks: 3
  files_modified: 4
  tests_added: "9 IT (record_*) + 7 unit (validate_recording_format / resolve_recording_path) + 2 unit (console_adapter Record conversion) = 18"
---

# Phase 4 Plan 5: Record Route + Phase 4 Sign-Off Summary

POST /api/v1/calls/{id}/record now ships with auto-path generation, traversal-safe explicit paths, wav/mp3 format validation, and a best-effort transcribe marker side-effect — closing CALL-09 and completing Phase 4 (CALL-01..10 all green, full regression intact).

## Route Shipped

`POST /api/v1/calls/{id}/record` — request body fully optional:

```jsonc
{}                                     // auto-path under recorder tree, wav, no beep, no marker
{"format": "mp3"}                      // auto-path with .mp3 extension
{"path": "/var/rec/abc.wav"}           // explicit path (must lie inside recorder tree)
{"transcribe": true}                   // also drops <path>.transcribe.marker before dispatch
{"max_duration_secs": 3600, "beep": true, "transcribe": true}
```

Response on success: `200 { "message": "dispatched", "path": "<resolved>" }`.

## Path Resolution Algorithm Evidence

Helper at `src/handler/api_v1/calls.rs::resolve_recording_path`:

| Input | Output |
|-------|--------|
| `path: None` | `<recorder_root>/<session_id>-<unix_ts>.<ext>` |
| `path: Some("/var/rec/custom.wav")` (in tree) | `/var/rec/custom.wav` |
| `path: Some("/var/rec/../../etc/passwd")` | 400 `recording path must not contain '..'` |
| `path: Some("relative.wav")` | 400 `recording path must be absolute` |
| `path: Some("/etc/passwd.wav")` | 400 `recording path must be inside recorder tree '<recorder_root>'` |
| `path: Some("")` | 400 `empty recording path` |

Format helper `validate_recording_format(Option<&str>)`:

| Input | Output |
|-------|--------|
| `None` | `Ok("wav")` (default) |
| `Some("wav")` / `Some("WAV")` / `Some("mp3")` / `Some("MP3")` | `Ok("wav" or "mp3")` (lower-cased) |
| anything else | 400 `invalid format '<x>' (expected 'wav' or 'mp3')` |

## Transcribe Marker Contract (Hand-off to Phase 7)

When `transcribe: true`, `maybe_drop_transcribe_marker` calls `std::fs::File::create("<resolved_path>.transcribe.marker")` BEFORE dispatch. Best-effort — `tracing::warn!` on IO failure but request still returns 200.

Phase 7 (Webhooks) consumers must:
1. Watch the `callrecord/` completion event stream as today.
2. For each completed recording, probe for a sibling `*.transcribe.marker` file alongside the recording path.
3. Treat markers idempotently — a marker may exist without a corresponding recording (dispatch failed after marker landed) or without a successful transcription (transcribe service down). The marker is a HINT to attempt transcription, not a guarantee.
4. Operators should garbage-collect orphan markers periodically (suggest age > 24h with no companion recording).

## Test Inventory — Final Phase 4

### Plan 04-05 contributions

| Test | Type | File |
|------|------|------|
| record_requires_auth | IT | tests/api_v1_calls.rs |
| record_auto_path_and_marker | IT | tests/api_v1_calls.rs |
| record_no_transcribe_skips_marker | IT | tests/api_v1_calls.rs |
| record_explicit_in_tree_path_works | IT | tests/api_v1_calls.rs |
| record_invalid_format_returns_400 | IT | tests/api_v1_calls.rs |
| record_path_traversal_returns_400 | IT | tests/api_v1_calls.rs |
| record_relative_path_returns_400 | IT | tests/api_v1_calls.rs |
| record_outside_tree_returns_400 | IT | tests/api_v1_calls.rs |
| record_unknown_session_returns_404 | IT | tests/api_v1_calls.rs |
| validate_recording_format_defaults_and_allows_mp3 | unit | src/handler/api_v1/calls.rs |
| validate_recording_format_rejects_flac | unit | src/handler/api_v1/calls.rs |
| resolve_recording_path_auto_generates | unit | src/handler/api_v1/calls.rs |
| resolve_recording_path_rejects_traversal | unit | src/handler/api_v1/calls.rs |
| resolve_recording_path_rejects_relative | unit | src/handler/api_v1/calls.rs |
| resolve_recording_path_rejects_outside_recorder_tree | unit | src/handler/api_v1/calls.rs |
| resolve_recording_path_accepts_in_tree | unit | src/handler/api_v1/calls.rs |
| test_record_conversion_happy | unit | src/call/adapters/console_adapter.rs |
| test_record_conversion_defaults | unit | src/call/adapters/console_adapter.rs |

### Phase 4 cumulative integration tests (`tests/api_v1_calls.rs`)

```
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.36s
```

### Full-suite regression

| Bucket | Count | Status |
|--------|-------|--------|
| `cargo test -p rustpbx --lib` | 1122 | ok |
| api_v1_auth | 2 | ok |
| api_v1_calls | 45 | ok |
| api_v1_cdrs | 13 | ok |
| api_v1_diagnostics | 12 | ok |
| api_v1_dids | 20 | ok |
| api_v1_error_shape | 1 | ok |
| api_v1_gateways | 19 | ok |
| api_v1_middleware | 3 | ok |
| api_v1_mount | 1 | ok |
| api_v1_routing_resolve | 7 | ok |
| api_v1_system | 7 | ok |
| api_v1_trunk_credentials | 8 | ok |
| api_v1_trunk_media | 9 | ok |
| api_v1_trunk_origination_uris | 9 | ok |
| api_v1_trunks | 23 | ok |

`cargo check -p rustpbx --release` → `Finished` in 2m 07s.

## Pre-existing Out-of-Scope Issue

`tests/did_index.rs` fails to compile on HEAD before Plan 04-05 began:

```
error[E0599]: no function or associated item named `from_map_for_test` found for struct `DidIndex`
  --> tests/did_index.rs:13:15
```

Verified pre-existing on the previous tip (introduced by `c1bcc54 feat(did): first-class DID/numbers configuration`). NOT caused by this plan; logged here for the next person who touches `did/index.rs`. This is the only test binary that fails to compile across the workspace.

## Phase 4 Completion Checklist

| Req | Description | Status | Evidence |
|-----|-------------|--------|----------|
| CALL-01 | List active calls | ✓ | GET /calls — Plan 04-01 |
| CALL-02 | Get single active call | ✓ | GET /calls/{id} — Plan 04-01 |
| CALL-03 | Hangup | ✓ | POST /calls/{id}/hangup — Plan 04-01 |
| CALL-04 | Mute / unmute (per-leg) | ✓ | POST /calls/{id}/mute,/unmute — Plan 04-02 |
| CALL-05 | Blind transfer | ✓ | POST /calls/{id}/transfer — Plan 04-03 |
| CALL-06 | Attended transfer (start, complete, cancel) | ✓ | POST /calls/{id}/transfer{/complete,/cancel} — Plan 04-03 |
| CALL-07 | Play / Speak (DTMF-friendly TTS) | ✓ | POST /calls/{id}/play,/speak — Plan 04-04 |
| CALL-08 | Send DTMF (RFC 4733 timing fields) | ✓ | POST /calls/{id}/dtmf — Plan 04-04 |
| CALL-09 | Start recording (auto/explicit path, format, transcribe marker) | ✓ | POST /calls/{id}/record — this plan |
| CALL-10 | All routes dispatch via single `dispatch_console_command` helper | ✓ | `grep send_command\|dispatch_call_command src/handler/api_v1/calls.rs` returns 0 hits |

12 routes registered in `router()` (verified via grep `\.route\("/calls`):

```
GET    /calls
GET    /calls/{id}
POST   /calls/{id}/hangup
POST   /calls/{id}/mute
POST   /calls/{id}/unmute
POST   /calls/{id}/transfer
POST   /calls/{id}/transfer/complete
POST   /calls/{id}/transfer/cancel
POST   /calls/{id}/play
POST   /calls/{id}/speak
POST   /calls/{id}/dtmf
POST   /calls/{id}/record
```

Zero adapter stubs remaining: `grep -c 'not yet wired' src/call/adapters/console_adapter.rs` → 0.

Schema impact: 0 schema changes, 1 additive `CallCommand::SendDtmf` struct extension (Plan 04-04), 1 additive `CommandResult.payload` extension (Plan 04-02), 1 additive `SessionSnapshot.pending_consult_leg_id` extension (Plan 04-03).

## Hand-Off to Phase 5

Phase 5 (Trunk Enforcement) is independent of Phase 4 deliverables — none of the active-call routes are on the proxy hot-path that Phase 5 will enforce. Phase 5 can begin immediately.

The single Phase 4 contract that LATER phases consume:
- **Phase 7 (Webhooks)** — must read `<resolved_path>.transcribe.marker` siblings produced by `/record` when `transcribe:true`. Contract documented above.

## Self-Check: PASSED

- `src/call/adapters/console_adapter.rs` — Record arm filled, 0 stubs remaining, 2 new unit tests present
- `src/handler/api_v1/calls.rs` — record_call + resolve_recording_path + validate_recording_format + maybe_drop_transcribe_marker present; 12 routes registered; 0 direct send_command/dispatch_call_command leaks
- `tests/api_v1_calls.rs` — 9 record_* tests present; full file passes 45/45
- `tests/common/mod.rs` — test_state_with_recorder helper present
- Commits in git log:
  - `9a01716 feat(04-05): fill Record adapter arm; map to StartRecording with RecordConfig`
  - `8861706 feat(04-05): add POST /record handler with path validation and transcribe marker`
  - `d32de69 test(04-05): integration tests for /record route (auto-path, traversal, marker, format)`
- Release build: `Finished release profile`
- Full lib + api_v1 integration test sweep: green (1122 lib + 178 api_v1 IT)
