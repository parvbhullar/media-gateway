---
phase: 04-active-calls-mid-call-control
plan: 01
subsystem: api_v1 / call.runtime
tags: [call-01, call-02, call-10, shell-04, d-10, d-11, d-11b]
requires:
  - phase 01 (api_v1 shell, Pagination, PaginatedResponse, ApiError)
  - phase 02 (integration test harness)
  - phase 03 (sub-router merge pattern; IT-01 matrix convention)
provides:
  - GET /api/v1/calls (paginated + filtered)
  - GET /api/v1/calls/{id} (rich ActiveCallView + SessionSnapshot)
  - crate::call::runtime::command_payload module (CallCommandPayload, Leg, PlaySource, ApiPlayOptions)
  - 8 new CallCommandPayload variants stubbed for plans 04-02..04-05
affects:
  - src/console/handlers/call_control.rs (re-exports CallCommandPayload; inline enum deleted)
  - src/call/adapters/console_adapter.rs (11 new match arms stubbed anyhow::bail!)
  - src/call/runtime/command_dispatch.rs (import path moved)
  - src/proxy/proxy_call.rs (sip_session module visibility widened: pub(crate) → pub)
tech-stack:
  added: []
  patterns:
    - "View + inlined pagination query pattern (matches dids.rs / cdrs.rs; serde_urlencoded cannot flatten)"
    - "Uniform ApiError envelope for filter parse failures"
    - "Registry snapshot drop on TOCTOU (never 500 on missing handle)"
key-files:
  created:
    - src/call/runtime/command_payload.rs
    - src/handler/api_v1/calls.rs
    - tests/api_v1_calls.rs
    - .planning/phases/04-active-calls-mid-call-control/04-01-SUMMARY.md
  modified:
    - src/call/runtime/mod.rs
    - src/console/handlers/call_control.rs
    - src/call/adapters/console_adapter.rs
    - src/call/runtime/command_dispatch.rs
    - src/handler/api_v1/mod.rs
    - src/proxy/proxy_call.rs
decisions:
  - "Inline pagination into CallListQuery (not a separate Query<Pagination> extractor) to match dids.rs/cdrs.rs since serde_urlencoded cannot flatten typed fields"
  - "SessionState::Active (not 'Answered' — plan text had wrong variant name) used in snapshot test fixture"
  - "Widen src/proxy/proxy_call::sip_session from pub(crate) to pub so integration tests can construct SipSession::with_handle + SessionSnapshot literals without fragile deep re-exports"
  - "Fix plan test expected timestamp: 2026-04-19T12:00:00Z → 1776600000 (not 1776513600)"
  - "PlaySource + ApiPlayOptions derive Serialize (required because CallCommandPayload derives Serialize and uses them by value)"
metrics:
  duration: "~25 min"
  completed: 2026-04-19
---

# Phase 4 Plan 01: Active Calls List/Get + CallCommandPayload Relocation — Summary

Relocated `CallCommandPayload` from `src/console/handlers/call_control.rs` to the neutral `src/call/runtime/command_payload.rs` module, extended from 5 to 13 variants (5 legacy console + 8 new API-facing per D-11), and shipped `GET /api/v1/calls` + `GET /api/v1/calls/{id}` closing CALL-01 and CALL-02. Console continues to work via a re-export shim; the 8 new adapter arms are `anyhow::bail!` stubs that plans 04-02..04-05 replace one-by-one.

## Files Created / Modified

| File | Type | Purpose |
|---|---|---|
| `src/call/runtime/command_payload.rs` | CREATE | Home of `CallCommandPayload` (13 variants) + `Leg` + `PlaySource` + `ApiPlayOptions` |
| `src/call/runtime/mod.rs` | MODIFY | `pub mod command_payload` + re-export `*` |
| `src/console/handlers/call_control.rs` | MODIFY | Delete inline enum; `pub use crate::call::runtime::command_payload::CallCommandPayload;` |
| `src/call/adapters/console_adapter.rs` | MODIFY | Import path update; add 11 `anyhow::bail!` stub arms (one per new variant + `ApiHangup`) |
| `src/call/runtime/command_dispatch.rs` | MODIFY | Import path update |
| `src/handler/api_v1/calls.rs` | CREATE | `ActiveCallView`, `CallListQuery`, GET /calls, GET /calls/{id}; 6 helper unit tests |
| `src/handler/api_v1/mod.rs` | MODIFY | `pub mod calls` + `.merge(calls::router())` |
| `src/proxy/proxy_call.rs` | MODIFY | `sip_session` module `pub(crate)` → `pub` (for test fixtures) |
| `tests/api_v1_calls.rs` | CREATE | 7 integration tests (IT-01 list/get slice) |

## CallCommandPayload Variant Inventory (13)

**Legacy (5) — console preserved verbatim:**
- `Hangup { reason, code, initiator }`
- `Accept { callee, sdp }` (with `#[serde(alias = "accept")]`)
- `Transfer { target }` (console's single-arg blind transfer)
- `Mute { track_id }` (console's raw track mute)
- `Unmute { track_id }`

**New API (8 + 1 bonus ApiHangup = 9):**
- `BlindTransfer { target, leg: Option<Leg> }` (plan 04-03)
- `AttendedTransferStart { target, leg: Option<Leg> }` (plan 04-03)
- `AttendedTransferComplete { consult_leg }` (plan 04-03)
- `AttendedTransferCancel { consult_leg }` (plan 04-03)
- `ApiMute { leg: Leg }` (plan 04-02)
- `ApiUnmute { leg: Leg }` (plan 04-02)
- `Play { source: PlaySource, leg, options }` (plan 04-04)
- `Speak { text, voice, leg }` (plan 04-04)
- `Dtmf { digits, duration_ms, inter_digit_ms, leg }` (plan 04-04)
- `Record { path, format, beep, max_duration_secs, transcribe }` (plan 04-05)
- `ApiHangup { reason, code }` (plan 04-02 — no `initiator`; adapter defaults to `"api"`)

`Leg = {Caller, Callee}` lowercase-serialized per D-11b.
`PlaySource = {File{path}, Url{url}}` tagged under `"type"` per D-12.
`ApiPlayOptions { loop_playback, interrupt_on_dtmf }` — `loop` is a Rust keyword, so renamed via `#[serde(rename = "loop")]`.

## Wire Types Shipped

```rust
// src/handler/api_v1/calls.rs
pub struct ActiveCallView {
    session_id: String,
    caller: Option<String>,
    callee: Option<String>,
    direction: String,
    started_at: DateTime<Utc>,
    answered_at: Option<DateTime<Utc>>,
    status: String,                    // "ringing" | "talking"
    snapshot: Option<SessionSnapshot>, // dropped on TOCTOU, skip_serializing_if=None
}

pub struct CallListQuery {
    page: Option<u64>,
    page_size: Option<u64>,
    status: Option<String>,     // "ringing" | "talking" (case-insensitive) else 400
    direction: Option<String>,  // "inbound" | "outbound" (case-insensitive) else 400
    caller: Option<String>,     // case-insensitive substring
    callee: Option<String>,     // case-insensitive substring
    since: Option<String>,      // RFC-3339; else 400 with uniform ApiError envelope
}
```

## Filter Semantics (Locked, D-04)

- `status` — finite allow-list; mixed-case OK; else 400 `bad_request`
- `direction` — finite allow-list; mixed-case OK; else 400
- `caller` / `callee` — case-insensitive substring on `Option<String>`, unset fields never match
- `since` — `DateTime::parse_from_rfc3339` + tz-convert to UTC; else 400
- `started_at >= since` (inclusive lower bound)
- Default sort: `started_at desc` (inherited from `list_recent` internal sort)
- Pagination: `page` defaults 1; `page_size` defaults 20, clamped to 200 via `Pagination::limit`

## Test Inventory

| Suite | Count | Status | File |
|---|---|---|---|
| Integration IT-01 list/get | 7 | pass | `tests/api_v1_calls.rs` |
| Unit: `calls.rs` helpers | 6 | pass | `src/handler/api_v1/calls.rs::tests` |
| Unit: `command_payload.rs` serde | 5 | pass | `src/call/runtime/command_payload.rs::tests` |
| Unit: `console_adapter.rs` legacy | 3 | pass | `src/call/adapters/console_adapter.rs::tests` |
| Regression: `api_v1_trunks` | 23 | pass | — |
| Regression: `api_v1_routing_resolve` | 7 | pass | — |
| Regression: `api_v1_trunk_credentials` | 8 | pass | — |
| **Total** | **59** | **all pass** | — |

## Deviations from Plan

### Rule 1 — Bug fixes applied

1. **[Rule 1 - Bug] Plan's `parse_since_rfc3339_happy` expected timestamp was wrong.**
   - **Found:** Task 2 unit-test run
   - **Issue:** Plan asserted `dt.timestamp() == 1776513600` for `"2026-04-19T12:00:00Z"`; actual value is `1776600000`.
   - **Fix:** Corrected expected to `1776600000` and added a comment explaining the derivation.
   - **File:** `src/handler/api_v1/calls.rs::tests::parse_since_rfc3339_happy`
   - **Commit:** `a734c6e`

2. **[Rule 1 - Bug] Plan used nonexistent `SessionState::Answered` variant.**
   - **Found:** Task 3 compile
   - **Issue:** `SessionState` (src/call/domain/state.rs) has `Initializing | Ringing | EarlyMedia | Active | Held | Transferring | AppRunning | Ending | Ended` — no `Answered`.
   - **Fix:** Used `SessionState::Active` (semantically equivalent — bridged/active session) in the snapshot test fixture.
   - **File:** `tests/api_v1_calls.rs::get_active_call_by_id_returns_rich_view`
   - **Commit:** `7726385`

### Rule 2 — Missing critical correctness

3. **[Rule 2 - Correctness] Added `Serialize` derive to `PlaySource` and `ApiPlayOptions`.**
   - **Issue:** `CallCommandPayload` derives both `Deserialize` and `Serialize`; by-value fields of a `Serialize` enum must also implement `Serialize`. Plan's type snippet only listed `Deserialize`.
   - **Fix:** Added `Serialize` to both derive lists.
   - **File:** `src/call/runtime/command_payload.rs`
   - **Commit:** `a261113`

### Rule 3 — Blocking fixes

4. **[Rule 3 - Blocker] Made `command_payload` module `pub` (not private `mod`).**
   - **Issue:** Plan's step 1.2 listed `mod command_payload;`. But `src/call/adapters/console_adapter.rs` imports `crate::call::runtime::command_payload::CallCommandPayload` directly (not via re-export), which requires the module to be at least `pub(crate)`. Using `pub mod` is simpler and matches how no other runtime submodule hides its path.
   - **Fix:** `pub mod command_payload;` in `src/call/runtime/mod.rs`.
   - **Commit:** `a261113`

5. **[Rule 3 - Blocker] Widened `src/proxy/proxy_call::sip_session` from `pub(crate)` to `pub`.**
   - **Issue:** Integration tests under `tests/` cannot reach `pub(crate)` modules; plan required `SipSession::with_handle`, `SessionSnapshot`, `SipSessionHandle` as test fixtures.
   - **Fix:** One-line visibility widen with comment; no semantic change to any internal call site (existing `pub(crate)` callers continue to work against `pub`).
   - **File:** `src/proxy/proxy_call.rs`
   - **Commit:** `7726385`

### Style adjustments

6. **Inlined pagination fields into `CallListQuery`** rather than a separate `Query<Pagination>` extractor, matching `src/handler/api_v1/dids.rs` and `cdrs.rs` (documented reason: `serde_urlencoded` doesn't support `flatten`). Plan mentioned this as a potential fallback — I adopted it up-front to avoid churn.

7. **`parse_status_filter` returns `ApiResult<ActiveProxyCallStatus>`** instead of a `String` so the filter comparison uses the native `Eq` on the enum (cleaner than string compare). Plan's shape was string-based; this is a minor clean-up with identical semantics.

## Hand-off for Plans 04-02..04-05

Each later plan replaces a specific `anyhow::bail!` stub arm in `src/call/adapters/console_adapter.rs::console_to_call_command`. Search for the plan ref in the bail message to find the exact arm:

| Plan | Stub arms to replace |
|---|---|
| 04-02 (mute/unmute/hangup) | `ApiMute`, `ApiUnmute`, `ApiHangup` (3) |
| 04-03 (transfer) | `BlindTransfer`, `AttendedTransferStart`, `AttendedTransferComplete`, `AttendedTransferCancel` (4) |
| 04-04 (play/speak/dtmf) | `Play`, `Speak`, `Dtmf` (3) |
| 04-05 (record) | `Record` (1) |

**Adapter surface they're consuming:**
- Input: `CallCommandPayload` variant → `session_id: &str`
- Output: `Result<CallCommand>`
- Routing: ALL api_v1 command handlers MUST call `dispatch_console_command(&registry, session_id, payload)` — never reach past it to `send_command`/build `CallCommand` directly. This is the CALL-10 contract.
- Handler wiring: extend `calls::router()` with additional `.route("/calls/{id}/hangup", post(handle_hangup))` etc. in later plans; reuse `State(state): State<AppState>`, `Path(id): Path<String>`, `Json(body): Json<T>` extractor triplet.
- `Leg` → `track_id` resolution: use `SipSession::CALLER_TRACK_ID` / `SipSession::CALLEE_TRACK_ID` inside handler; if snapshot is unavailable → 409 `conflict` with "media tracks not yet established" per D-09.

## RWI NOT in blast radius (RESEARCH correction confirmed)

Plan CONTEXT.md line 286 initially claimed "RWI processor" consumed `CallCommandPayload`. RESEARCH §5 corrected this — RWI uses its own `RwiCommandPayload`. Grep evidence after Plan 04-01 lands:

```
$ rg "CallCommandPayload" src/rwi/
  (no matches)
```

Confirmed — `src/rwi/*` was NOT modified and `CallCommandPayload` relocation is a 5-file compile-time change (command_payload.rs new, 4 files updated: call_control.rs, console_adapter.rs, command_dispatch.rs, runtime/mod.rs). No runtime behavior change.

## Commits

| Commit | Task | Message |
|---|---|---|
| `a261113` | Task 1 | feat(04-01): relocate CallCommandPayload to call::runtime::command_payload with 8 new API variants |
| `a734c6e` | Task 2 | feat(04-01): add GET /api/v1/calls list + get-by-id handlers |
| `7726385` | Task 3 | test(04-01): integration tests for GET /api/v1/calls + /calls/{id} |

## Success Criteria

- [x] CALL-01 closed: `GET /api/v1/calls` paginated + 5 filters + 400 on bad values + 401 on missing Bearer.
- [x] CALL-02 closed: `GET /api/v1/calls/{id}` rich `ActiveCallView` with `SessionSnapshot` nested; 404 on unknown id.
- [x] CALL-10 foundation: `CallCommandPayload` in neutral module, 8 new API variants stubbed, adapter is exhaustive.
- [x] 7 integration tests + 6 calls.rs units + 5 payload units + 3 existing adapter units pass.
- [x] Phase 1/2/3 regression baseline green (verified: trunks 23, routing_resolve 7, trunk_credentials 8).
- [x] No new external deps (`urlencoding` already a main dep).
- [x] RWI untouched.

## Self-Check: PASSED

Created files verified to exist on disk:
- `src/call/runtime/command_payload.rs` FOUND
- `src/handler/api_v1/calls.rs` FOUND
- `tests/api_v1_calls.rs` FOUND

Commits verified in `git log --oneline`:
- `a261113` FOUND
- `a734c6e` FOUND
- `7726385` FOUND

Test battery: 59/59 passed (7 + 6 + 5 + 3 + 23 + 7 + 8).
