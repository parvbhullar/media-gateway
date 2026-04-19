---
phase: 4
slug: active-calls-mid-call-control
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-04-19
---

# Phase 4 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution. Draft — planner fills the Per-Task Verification Map as plans land.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | `cargo test` (Rust built-in) |
| **Config file** | `Cargo.toml` (workspace member `rustpbx`) |
| **Quick run command** | `cargo test -p rustpbx --test api_v1_calls` |
| **Full suite command** | `cargo test -p rustpbx` |
| **Estimated runtime** | Quick ~30–60s; Full ~3–5 min (existing baseline ~183 tests + new Phase 4 tests) |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p rustpbx --test api_v1_calls` (scoped to Phase 4 test file)
- **After every plan wave:** Run `cargo test -p rustpbx` (full suite to catch regressions in Phase 1/2/3)
- **Before `/gsd-verify-work`:** Full suite must be green
- **Max feedback latency:** 60s after a single-file test run

---

## Per-Task Verification Map

*To be filled by gsd-planner when individual plans are produced. Entries follow this shape:*

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 04-01-01 | 04-01 | 1 | CALL-01 | — | list endpoint requires Bearer auth (401 without token) | integration | `cargo test -p rustpbx --test api_v1_calls -- calls_require_auth` | ❌ W0 | ⬜ pending |
| 04-01-02 | 04-01 | 1 | CALL-01 | — | GET /api/v1/calls returns PaginatedResponse over registry snapshot | integration | `cargo test -p rustpbx --test api_v1_calls -- list_active_calls_paginated` | ❌ W0 | ⬜ pending |
| 04-01-03 | 04-01 | 1 | CALL-02 | — | GET /api/v1/calls/{id} returns rich ActiveCallView with SessionSnapshot | integration | `cargo test -p rustpbx --test api_v1_calls -- get_active_call_by_id_returns_rich_view` | ❌ W0 | ⬜ pending |
| 04-02-01 | 04-02 | 2 | CALL-03 | — | POST /hangup dispatches through dispatch_console_command | integration | `cargo test -p rustpbx --test api_v1_calls -- hangup_dispatches_via_registry` | ❌ W0 | ⬜ pending |
| 04-02-02 | 04-02 | 2 | CALL-05 | — | POST /mute resolves leg→track_id using SipSession::CALLER_TRACK_ID / CALLEE_TRACK_ID constants | integration | `cargo test -p rustpbx --test api_v1_calls -- mute_resolves_leg_to_track_id` | ❌ W0 | ⬜ pending |
| 04-03-01 | 04-03 | 3 | CALL-04 | — | POST /transfer {type:"blind"} dispatches CallCommand::Transfer{attended:false} | integration | `cargo test -p rustpbx --test api_v1_calls -- blind_transfer_dispatches` | ❌ W0 | ⬜ pending |
| 04-03-02 | 04-03 | 3 | CALL-04 | — | POST /transfer {type:"attended"} returns consult_leg_id in body | integration | `cargo test -p rustpbx --test api_v1_calls -- attended_transfer_returns_consult_leg_id` | ❌ W0 | ⬜ pending |
| 04-04-01 | 04-04 | 4 | CALL-06 | — | POST /play with {source:{url:...}} returns 400 before dispatch (pre-variant probe) | integration | `cargo test -p rustpbx --test api_v1_calls -- play_url_returns_400_pre_dispatch` | ❌ W0 | ⬜ pending |
| 04-04-02 | 04-04 | 4 | CALL-08 | — | POST /dtmf with custom duration_ms passes through extended SendDtmf struct | integration | `cargo test -p rustpbx --test api_v1_calls -- dtmf_with_timing_overrides` | ❌ W0 | ⬜ pending |
| 04-05-01 | 04-05 | 5 | CALL-09 | — | POST /record returns auto-generated path in response body + drops transcribe marker | integration | `cargo test -p rustpbx --test api_v1_calls -- record_auto_path_and_marker` | ❌ W0 | ⬜ pending |

*Planner will expand this table as concrete tasks per plan land. Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [x] `tests/api_v1_calls.rs` — dedicated IT-01 test file (NEW — created in Plan 04-01)
- [x] `tests/common/mod.rs` — reuse existing `test_state_with_api_key` / `test_state_empty` helpers (no changes needed)
- [x] Test fixture: `SipSession::with_handle(id)` returning `(handle, _cmd_rx)` — already exists in `src/proxy/active_call_registry.rs:208-214` (cited in CONTEXT canonical refs)

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Real audio plays on a live SIP call via POST /play | CALL-06 | Requires a real SIP endpoint and audio output to verify playback quality | Place a call via carrier trunk, POST `/api/v1/calls/{id}/play {"source":{"file":"/var/lib/supersip/audio/test.wav"}}`, listen on callee side to confirm audio |
| TTS generates audible speech on live call | CALL-07 | Requires live TTS engine + audio output | Place a call, POST `/api/v1/calls/{id}/speak {"text":"hello"}`, listen for synthesized speech |
| Recording file has valid audio after stop | CALL-09 | Requires decoding resulting wav/mp3 to confirm format correctness | Start recording, send audio, stop, open resulting file in audio player |
| Transfer actually rings a new destination | CALL-04 | Requires a second endpoint to receive the REFER | Place a call, POST `/transfer {type:"blind", target:"+14155551234"}`, verify the target phone rings |

---

## Integration Test Coverage Summary (IT-01 per-route)

| Route | auth (401) | happy | 404 (unknown id) | 400 (bad body) | 409 (dispatch fail) |
|-------|-----------|-------|------------------|----------------|---------------------|
| GET /api/v1/calls | ✓ | ✓ | — | ✓ (invalid filter) | — |
| GET /api/v1/calls/{id} | ✓ | ✓ | ✓ | — | — |
| POST /hangup | ✓ | ✓ | ✓ | ✓ | ✓ |
| POST /transfer | ✓ | ✓ | ✓ | ✓ | ✓ |
| POST /transfer/complete | ✓ | ✓ | ✓ (unknown consult_leg) | ✓ | — |
| POST /transfer/cancel | ✓ | ✓ | ✓ (unknown consult_leg) | ✓ | — |
| POST /mute | ✓ | ✓ | ✓ | ✓ (missing leg) | ✓ (no media tracks) |
| POST /unmute | ✓ | ✓ | ✓ | ✓ | ✓ |
| POST /play | ✓ | ✓ (file source) | ✓ | ✓ (url source returns 400 pre-dispatch) | ✓ |
| POST /speak | ✓ | ✓ | ✓ | ✓ | ✓ (capability denied → 400) |
| POST /dtmf | ✓ | ✓ | ✓ | ✓ (invalid digits) | ✓ |
| POST /record | ✓ | ✓ (auto path) | ✓ | ✓ (path traversal) | ✓ |

**Consolidated test count target:** ~35–45 tests across the 12 routes (some routes share a fixture so not every cell is a separate test).

---

## Regression Baseline

- **Phase 1 + 2 + 3 existing tests must stay green.** Current baseline (as of 2026-04-18): 114 Phase 1/2 tests + 69 Phase 3 tests = ~183 tests.
- **Console tests** (if any — `tests/console_call_control*`) must pass after `CallCommandPayload` relocation (D-10). This is a compile-level change; tests pick up the new import path automatically via the console adapter.
- **RWI tests** (`tests/rwi_*`) must pass — RWI uses its own `RwiCommandPayload`, not `CallCommandPayload` (per research correction on D-10), so no blast radius there.

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references (reused existing fixtures — no new Wave 0 work needed)
- [ ] No watch-mode flags (cargo test is one-shot)
- [ ] Feedback latency < 60s (scoped `--test api_v1_calls` runs fast)
- [ ] `nyquist_compliant: true` set in frontmatter (pending planner fills the Per-Task table completely)

**Approval:** pending (planner + checker pass)
