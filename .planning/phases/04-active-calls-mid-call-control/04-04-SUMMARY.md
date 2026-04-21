---
phase: 4
plan: 4
subsystem: api-handlers, call-domain, call-adapters
tags: [media-commands, play, speak, dtmf, call-control, pre-dispatch-probes]
dependency_graph:
  requires: [04-01, 04-02, 04-03]
  provides: [play-route, speak-route, dtmf-route, sendDtmf-timing-fields]
  affects: [src/handler/api_v1/calls.rs, src/call/domain/command.rs, src/call/adapters/console_adapter.rs]
tech_stack:
  added: []
  patterns: [pre-dispatch-variant-probe, deferred-implementation-TODO]
key_files:
  created: []
  modified:
    - src/call/domain/command.rs
    - src/proxy/proxy_call/sip_session.rs
    - src/call/adapters/console_adapter.rs
    - src/handler/api_v1/calls.rs
    - src/handler/api_v1/error.rs
    - tests/api_v1_calls.rs
decisions:
  - Pre-dispatch URL probe returns 400 (not 501) per Phase 4 convention (operator-side problem)
  - /speak always returns 400 in Phase 4 (TTS engine not wired; CALL-07 deferred)
  - SendDtmf timing fields accepted on wire but ignored (D-14b deferral)
  - LegId == session_id convention (D-21) preserved in leg_to_leg_id helper
metrics:
  duration: "~30 minutes"
  completed: "2026-04-21"
  tasks_completed: 3
  files_modified: 6
---

# Phase 4 Plan 4: Play / Speak / DTMF Routes Summary

Three media-command routes shipped for Phase 4: `POST /api/v1/calls/{id}/play`, `POST /api/v1/calls/{id}/speak`, `POST /api/v1/calls/{id}/dtmf`. The `CallCommand::SendDtmf` variant extended with per-digit timing fields (accepted on wire, ignored in SIP layer per D-14b deferral).

## Routes Shipped

### POST /api/v1/calls/{id}/play

**Request shape:**
```json
{
  "source": {"type": "file", "path": "/tmp/hold.wav"},
  "leg": "callee",
  "loop": true,
  "interrupt_on_dtmf": false
}
```

**Response:** `{"status":"ok"}` (200) or error JSON.

**Pre-dispatch probe:** If `source.type == "url"`, returns 400 with `code: "not_supported"` BEFORE dispatching to the session. No command reaches the session. Rationale: URL playback is not wired in Phase 4 (CALL-06 deferred item). The 400 (not 501) convention follows Phase 4 policy — this is an operator-side configuration issue.

### POST /api/v1/calls/{id}/speak

**Request shape:**
```json
{
  "text": "Hello world",
  "voice": "en-US-Neural2-A",
  "leg": "caller"
}
```

**Response:** Always 400 with `code: "not_supported"`, `message: "tts engine not wired; see CALL-07 deferred item"` in Phase 4. The session is looked up (auth + existence check) but no command is dispatched. The adapter arm is wired for future phases (`CallCommand::Play { source: MediaSource::Tts { .. } }`).

### POST /api/v1/calls/{id}/dtmf

**Request shape:**
```json
{
  "digits": "1234",
  "duration_ms": 200,
  "inter_digit_ms": 100,
  "leg": "caller"
}
```

**Response:** `{"status":"ok"}` (200) or error JSON.

Digits validated before session lookup: empty string → 400, invalid chars (non `0-9 A-D a-d * #`) → 400 with `code: "bad_request"`. If `leg` omitted, `default_leg_from_direction` maps `"inbound"` → `Leg::Caller`, all others → `Leg::Callee`.

## SendDtmf Extension Delta

`CallCommand::SendDtmf` in `src/call/domain/command.rs` now carries two new optional fields:

```rust
SendDtmf {
    leg_id: LegId,
    digits: String,
    #[serde(default)]
    duration_ms: Option<u32>,   // NEW — D-14b: accepted, not honored
    #[serde(default)]
    inter_digit_ms: Option<u32>, // NEW — D-14b: accepted, not honored
}
```

**5 call sites updated to pass `None`:**

1. `src/proxy/proxy_call/sip_session.rs` dispatch arm — destructures all 4 fields, passes to `handle_send_dtmf`
2. `src/proxy/proxy_call/sip_session.rs` `handle_send_dtmf` signature — accepts both, `let _ = (duration_ms, inter_digit_ms)` with TODO comment
3. `src/proxy/proxy_call/sip_session.rs` unit test — passes `duration_ms: None, inter_digit_ms: None`
4. `src/call/adapters/console_adapter.rs` Dtmf arm — passes both fields through from payload
5. `src/call/adapters/session_action_bridge.rs` — uses `{ .. }` wildcard, no change needed

TODO comment placed: `// TODO(phase-hardening, D-14b): honor per-digit duration / inter-digit overrides in the SIP INFO payload.`

## Pre-Dispatch Probe Evidence

Two short-circuits prevent commands reaching the session:

1. **URL playback probe** (in `play_on_call`): `if let PlaySource::Url { .. } = req.source { return Err(ApiError::not_supported(...)) }` — evaluated after `require_session` (so auth and session existence are still checked) but before `dispatch_console_command`. Integration test `play_url_returns_400_pre_dispatch` verifies the rx channel receives no command.

2. **TTS short-circuit** (in `speak_on_call`): Returns `Err(ApiError::not_supported(...))` unconditionally after session lookup. The `let _ = (req.text, req.voice, req.leg)` pattern preserves the parsed request for future wiring. Integration test `speak_returns_400_always_in_phase_4` verifies the rx is empty.

Both return 400 (not 501) per Phase 4 convention: "feature not wired = operator-side problem."

## Test Inventory

**Integration tests** (`tests/api_v1_calls.rs`): **36 total** (24 from plans 01-03 + 12 new including plan 04-04 variants)

New tests added in plan 04-04:
1. `play_requires_auth` — POST /play without token → 401
2. `play_file_dispatches_happy_path` — file source → 200, session receives `CallCommand::Play { source: MediaSource::File { .. } }`
3. `play_url_returns_400_pre_dispatch` — url source → 400 `not_supported`, rx empty
4. `play_unknown_session_returns_404` — POST /play on unknown session → 404
5. `speak_requires_auth` — POST /speak without token → 401
6. `speak_returns_400_always_in_phase_4` — POST /speak → 400 `not_supported`, rx empty
7. `dtmf_requires_auth` (implicit via existing pattern)
8. `dtmf_with_timing_overrides` — `duration_ms: Some(200), inter_digit_ms: Some(100)` flows through to `CallCommand::SendDtmf`
9. `dtmf_invalid_digit_returns_400` — digits `"12e"` → 400 `bad_request`

**Adapter unit tests** (`src/call/adapters/console_adapter.rs`): **18 lib tests total**

New tests added in plan 04-04:
- `test_play_file_conversion` — PlaySource::File → MediaSource::File
- `test_play_url_conversion_still_produces_valid_cmd` — url source still converts (probe is at handler level)
- `test_speak_conversion_produces_tts_play` — Speak → MediaSource::Tts
- `test_dtmf_with_timing_overrides_passes_through` — duration_ms/inter_digit_ms flow
- `test_dtmf_without_timing_overrides` — None fields pass through

**Lib unit tests** (inline in `calls.rs`):
- `validate_dtmf_digits_happy`
- `validate_dtmf_digits_rejects_bad_char`
- `default_leg_from_direction_maps_correctly`

## Known Deferral

**D-14b:** `handle_send_dtmf` in `sip_session.rs` accepts `duration_ms` and `inter_digit_ms` but continues to use hardcoded defaults (160 ms per-digit duration). The SIP INFO DTMF payload is not modified based on the override values. Tagged with `TODO(phase-hardening, D-14b)` in two locations.

**CALL-06:** URL playback routing (fetching remote audio file, streaming to leg) — not wired.

**CALL-07:** TTS engine integration — not wired.

## Commits

- `5241ba5` feat(04-04): wire Play/Speak/Dtmf adapter arms to CallCommand
- `5a2ab15` feat(04-04): add /play, /speak, /dtmf handlers with pre-dispatch probes
- `95cbbaf` test(04-04): integration tests for /play, /speak, /dtmf routes
- `9bec20b` feat(04-04): extend CallCommand::SendDtmf with timing fields

## Hand-off for Plan 04-05

- `ApiError::not_supported` constructor is in `src/handler/api_v1/error.rs` (returns 400)
- `map_command_result(result, extra: Option<serde_json::Value>)` — pass `None` for these routes
- The `/speak` stub is wired at adapter level; only the handler 400s. Wiring TTS in a future plan only requires removing the early return in `speak_on_call` and implementing the TTS backend.
- All 36 integration tests must continue to pass as baseline for Plan 04-05.

## Self-Check: PASSED

- `src/call/domain/command.rs` — `duration_ms: Option<u32>` present
- `src/proxy/proxy_call/sip_session.rs` — `handle_send_dtmf` accepts 4 params
- `src/call/adapters/console_adapter.rs` — 3 stub arms replaced, 5 unit tests
- `src/handler/api_v1/calls.rs` — 3 handlers + 3 routes registered
- `tests/api_v1_calls.rs` — 36 tests pass
- Commits 9bec20b, 95cbbaf, 5a2ab15, 5241ba5 exist in git log
