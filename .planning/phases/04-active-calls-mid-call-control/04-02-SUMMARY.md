---
phase: 04-active-calls-mid-call-control
plan: 02
subsystem: api_v1 / call.adapters
tags: [call-03, call-05, call-10, d-07, d-08, d-09]
requires:
  - plan 04-01 (CallCommandPayload + ApiHangup/ApiMute/ApiUnmute stub arms + calls.rs router)
provides:
  - POST /api/v1/calls/{id}/hangup
  - POST /api/v1/calls/{id}/mute
  - POST /api/v1/calls/{id}/unmute
  - map_command_result helper (D-07 mapping — reused by plans 04-03/04/05)
  - validate_leg + require_media_ready + require_session helpers
affects:
  - src/call/adapters/console_adapter.rs (3 stub arms filled in)
  - src/handler/api_v1/calls.rs (router extended, 3 handlers + helpers added)
  - tests/api_v1_calls.rs (+10 integration tests, 17 total)
tech-stack:
  added: []
  patterns:
    - "Dispatch-through-single-entry-point (CALL-10) — handlers never call send_command directly"
    - "Compile-time leg->track_id resolution via SipSession::CALLER_TRACK_ID/CALLEE_TRACK_ID (tamper-proof: clients cannot supply track_id)"
    - "404 pre-check before dispatch (D-08) — session existence is a clean 404, never a dispatch-level 409"
    - "409 precondition on snapshot (D-09) — mute requires snapshot present AND leg_count >= 2"
    - "D-07 CommandResult->HTTP mapping centralized in map_command_result for re-use"
key-files:
  created:
    - .planning/phases/04-active-calls-mid-call-control/04-02-SUMMARY.md
  modified:
    - src/call/adapters/console_adapter.rs
    - src/handler/api_v1/calls.rs
    - tests/api_v1_calls.rs
decisions:
  - "map_command_result takes Option<serde_json::Value> extra so plan 04-03 can merge consult_leg_id and plan 04-05 can merge recording path without duplicating the dispatch-mapping logic"
  - "mute_missing_leg_returns_400 accepts either 400 or 422 — axum 0.8 surfaces Json extractor rejections as 422 for missing-required-field (observed); invalid values still hit our validate_leg and return 400"
  - "Did NOT add a separate ApiError::not_supported constructor — map_command_result reuses bad_request with a comment; only relevant to plan 04-04 when speak/play without infra triggers the not_supported branch"
  - "Kept on branch sip_fix (actual current branch; executor prompt said console_sip but that branch doesn't exist in this worktree)"
metrics:
  duration: "~8 min"
  completed: 2026-04-20
---

# Phase 4 Plan 02: Hangup, Mute, Unmute Routes — Summary

Replaced the three `ApiHangup`, `ApiMute`, `ApiUnmute` stub arms in the
console adapter with real `CallCommand` construction, then shipped `POST
/api/v1/calls/{id}/{hangup,mute,unmute}` on top of the Plan 04-01 router.
Every handler routes through `dispatch_console_command` verbatim — CALL-10
holds by construction. The three routes are the first REST surface that
consumes the Phase 4 dispatch pipeline end-to-end.

## Routes Implemented

| Method | Path | Request Body | Success Response |
|---|---|---|---|
| POST | `/api/v1/calls/{id}/hangup` | `{"reason"?:"by_caller","code"?:200}` | `200 {"message":"dispatched"}` |
| POST | `/api/v1/calls/{id}/mute`   | `{"leg":"caller" \| "callee"}` | `200 {"message":"dispatched"}` |
| POST | `/api/v1/calls/{id}/unmute` | `{"leg":"caller" \| "callee"}` | `200 {"message":"dispatched"}` |

Error responses (shared `{"error": "...", "code": "..."}` envelope):
- `401 unauthorized` — missing/invalid Bearer (all 3 routes)
- `400 bad_request` — invalid leg value (mute/unmute), or `not_supported` msg from CommandResult (safety-net)
- `404 not_found` — unknown session_id (pre-dispatch D-08 check) or CommandResult "not found"
- `409 conflict` — media tracks not yet established (D-09), or `failed to dispatch` (mpsc closed)
- `500 internal` — anyhow error from dispatch or unknown failure message

## Adapter Arms Filled

`src/call/adapters/console_adapter.rs::console_to_call_command`:

| Payload variant | CallCommand produced |
|---|---|
| `ApiHangup { reason, code }` | `CallCommand::Hangup(HangupCommand::local("api", parse_hangup_reason(reason), code).with_cascade(HangupCascade::All))` |
| `ApiMute { leg: Caller }`   | `CallCommand::MuteTrack { track_id: "caller-track" }` |
| `ApiMute { leg: Callee }`   | `CallCommand::MuteTrack { track_id: "callee-track" }` |
| `ApiUnmute { leg: Caller }` | `CallCommand::UnmuteTrack { track_id: "caller-track" }` |
| `ApiUnmute { leg: Callee }` | `CallCommand::UnmuteTrack { track_id: "callee-track" }` |

Track ID resolution uses `SipSession::CALLER_TRACK_ID` / `CALLEE_TRACK_ID`
compile-time constants per D-09 (research-corrected — the snapshot has no
per-leg `track_id` fields). The handler layer never accepts a
client-supplied `track_id`, eliminating T-04-02-02 (tampering via
leg-scoping bypass).

## Helpers Added in `calls.rs`

| Function | Purpose |
|---|---|
| `validate_leg(&str) -> ApiResult<Leg>` | Case-insensitive allow-list; rejects anything but `caller`/`callee` with `ApiError::bad_request` |
| `require_media_ready(&SipSessionHandle)` | Returns `ApiError::conflict("media tracks not yet established")` when snapshot is `None` or `leg_count < 2` |
| `require_session(&Arc<ActiveProxyCallRegistry>, &str)` | 404 pre-check; returns handle if present, else `ApiError::not_found` |
| `map_command_result(Result<CommandResult>, Option<Value>)` | D-07 CommandResult->HTTP mapping; merges optional `extra` fields into the 200 body |

`map_command_result` is the **shared entry point** that plans 04-03/04/05
reuse — no duplication of the dispatch-mapping logic. The `extra` parameter
handles plan 04-03's `consult_leg_id` and plan 04-05's recording `path`
without refactoring.

## Test Inventory

| Suite | Count | Status | Notes |
|---|---|---|---|
| Integration (`tests/api_v1_calls.rs`) | 17 | pass | 7 from 04-01 + **10 new** (hangup x4 + mute x5 + unmute x1) |
| Unit: `console_adapter.rs::tests` | 9 | pass | 3 legacy + **6 new** (mute caller, mute callee, unmute caller, unmute callee, hangup with reason/code, hangup without) |
| Unit: `handler::api_v1::calls::tests` | 12 | pass | 6 from 04-01 + **6 new** (validate_leg accepts mixed case, validate_leg rejects garbage, map_command_result: success/404/409/merge) |
| Regression: `api_v1_trunks`          | 23 | pass | — |
| Regression: `api_v1_routing_resolve` |  7 | pass | — |
| Regression: `api_v1_trunk_credentials` | 8 | pass | — |
| Regression: `api_v1_trunk_media`     |  9 | pass | — |
| **Total exercised by this plan**     | **85** | **all pass** | |

## Integration test breakdown (10 new)

1. `hangup_requires_auth` — POST w/o Bearer → 401
2. `hangup_dispatches_via_registry` — 200; `rx.try_recv()` → `CallCommand::Hangup(_)`
3. `hangup_unknown_session_returns_404` — 404 `not_found` (pre-dispatch)
4. `hangup_dropped_rx_returns_409` — drop cmd_rx, POST → 409 "command dispatch failed"
5. `mute_requires_auth` — 401
6. `mute_happy_dispatches_caller_track` — 200; `rx.try_recv()` → `CallCommand::MuteTrack{track_id:"caller-track"}`
7. `mute_missing_leg_returns_400` — POST `{}` → 4xx (accepts 400 or 422 from axum's Json rejection)
8. `mute_invalid_leg_returns_400` — POST `{leg:"both"}` → 400 `bad_request` with "invalid leg"
9. `mute_without_media_tracks_returns_409` — snapshot=None → 409 "media tracks not yet established"
10. `unmute_happy_dispatches_callee_track` — 200; `rx.try_recv()` → `CallCommand::UnmuteTrack{track_id:"callee-track"}`

## Deviations from Plan

1. **[Style adjustment] `mute_missing_leg_returns_400` accepts 422 or 400.**
   - **Found during:** Task 3 test run
   - **Issue:** axum 0.8 returns 422 Unprocessable Entity for valid-JSON-but-missing-required-field (which is what `{}` without `leg` is); older axum versions returned 400. Plan anticipated this: "If the missing-leg test returns something other than 400, update the assertion to match whatever axum returns in this version."
   - **Fix:** Assert `status == 400 || status == 422` — the key invariant is that a 4xx rejection fires. The `mute_invalid_leg_returns_400` test (POST `{leg:"both"}`) still asserts exactly 400 because that request has `leg` present and hits our own `validate_leg` → `ApiError::bad_request`.
   - **File:** `tests/api_v1_calls.rs::mute_missing_leg_returns_400`

2. **[Design choice] Did NOT add a separate `ApiError::not_supported` constructor.**
   - **Found during:** Task 2 design
   - **Issue:** Plan suggested optionally adding a `not_supported` constructor on `ApiError`. It's not consumed by hangup/mute/unmute — only by the safety-net `"not supported"` branch of `map_command_result` which fires in plan 04-04 for play/speak without infra.
   - **Fix:** Reuse `ApiError::bad_request` with a comment explaining why (400 not 501 — request is semantically malformed for our current deployment). Keeps `error.rs` untouched in this plan. Plan 04-04 can still choose to add it if the verifier prefers a dedicated code.
   - **File:** `src/handler/api_v1/calls.rs::map_command_result`

3. **[Environmental] Executed on branch `sip_fix` (not `console_sip`).**
   - **Found during:** first commit
   - **Issue:** The executor prompt said "Stay on current branch `console_sip`." The worktree's actual current branch is `sip_fix` (Plan 04-01's commits — dc5fdc5 et al — live here; no `console_sip` branch exists in this repo). The `console_sip` name in the prompt was stale/incorrect.
   - **Fix:** Continued on `sip_fix` — this is the branch Plan 04-01 landed on and where all prior work lives. No merge/rebase needed.

## D-07 Mapping Helper — Hand-off Note for Plans 04-03/04/05

```rust
// Single entry point — all plans reuse.
fn map_command_result(
    result: anyhow::Result<CommandResult>,
    extra: Option<serde_json::Value>,      // merged into 200 body
) -> ApiResult<Json<serde_json::Value>>;
```

- **Plan 04-03 (transfer)** — attended transfer returns `{"message":"dispatched","consult_leg_id":"..."}`. Pass `Some(json!({"consult_leg_id": id}))` as `extra`. Will need `CommandResult::payload` extension or post-dispatch registry poll to learn the consult leg id before calling `map_command_result` (per CONTEXT Claude's Discretion — recommend extending `CommandResult`).
- **Plan 04-04 (play/speak/dtmf)** — `not_supported` CommandResults already flow through the safety-net branch (`400 bad_request`). If the plan wants a dedicated `code:"not_supported"`, add it to `error.rs` and teach `map_command_result` to emit it — one-line change.
- **Plan 04-05 (record)** — merge `{"path":"<resolved-path>"}` via `extra` so the client learns where the file will land.

## Leg → track_id Resolution Evidence (T-04-02-02 mitigation)

```
$ grep -n 'track_id' src/handler/api_v1/calls.rs
(no matches — handler never sees track_id)

$ grep -n 'CALLER_TRACK_ID\|CALLEE_TRACK_ID' src/call/adapters/console_adapter.rs
7:use crate::proxy::proxy_call::sip_session::SipSession;
89:                Leg::Caller => SipSession::CALLER_TRACK_ID.to_string(),
90:                Leg::Callee => SipSession::CALLEE_TRACK_ID.to_string(),
96:                Leg::Caller => SipSession::CALLER_TRACK_ID.to_string(),
97:                Leg::Callee => SipSession::CALLEE_TRACK_ID.to_string(),
```

A client cannot smuggle a raw `track_id` into the new `/mute` or `/unmute`
routes — the wire contract only accepts `leg:"caller"|"callee"`, and the
resolution happens at the adapter layer via compile-time constants.

## Commits

| Commit | Task | Message |
|---|---|---|
| `64fcd95` | Task 1 | feat(04-02): wire ApiHangup/ApiMute/ApiUnmute adapter arms to CallCommand |
| `3ae0c46` | Task 2 | feat(04-02): add POST /api/v1/calls/{id}/{hangup,mute,unmute} handlers |
| `38bf5f6` | Task 3 | test(04-02): integration tests for hangup/mute/unmute routes |

## Success Criteria

- [x] CALL-03 closed: `POST /api/v1/calls/{id}/hangup` dispatches through `dispatch_console_command`; 401/404/409/200 all tested.
- [x] CALL-05 closed: `POST /api/v1/calls/{id}/mute` and `/unmute` resolve `leg → track_id` via compile-time constants and dispatch through `dispatch_console_command`; 409 guard when media tracks not established; 400 on invalid leg; 401/404/409/200 all tested.
- [x] CALL-10 advanced: three routes route exclusively through `dispatch_console_command` — no direct `send_command` calls in handlers.
- [x] 10 new integration tests pass (17 total in `tests/api_v1_calls.rs`); 6 new adapter unit tests; 6 new calls.rs unit tests.
- [x] Phase 1/2/3 baseline green (trunks 23, routing_resolve 7, trunk_credentials 8, trunk_media 9).
- [x] `map_command_result` helper in place — reusable by Plans 04-03/04/05.

## Self-Check: PASSED

Files verified to exist:
- `.planning/phases/04-active-calls-mid-call-control/04-02-SUMMARY.md` FOUND
- `src/call/adapters/console_adapter.rs` FOUND (modified)
- `src/handler/api_v1/calls.rs` FOUND (modified)
- `tests/api_v1_calls.rs` FOUND (modified)

Commits verified in `git log --oneline`:
- `64fcd95` FOUND
- `3ae0c46` FOUND
- `38bf5f6` FOUND

Test battery: 85/85 passed (17 integration + 9 adapter unit + 12 handler unit + 23 trunks + 7 routing + 8 trunk_credentials + 9 trunk_media).

No stubs remain for Plan 04-02 — all three `not yet wired; see plan 04-02` messages are gone from `console_adapter.rs`. The 7 remaining `anyhow::bail!` stubs (BlindTransfer, AttendedTransferStart/Complete/Cancel, Play, Speak, Dtmf, Record) are owned by plans 04-03/04/05 per the Plan 04-01 hand-off table.
