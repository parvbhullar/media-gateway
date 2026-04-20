---
phase: 04-active-calls-mid-call-control
plan: 03
subsystem: api_v1 / call.runtime / call.adapters / proxy.sip_session
tags: [call-04, call-10, d-19, d-20, d-21, d-22]
requires:
  - plan 04-01 (CallCommandPayload with BlindTransfer/AttendedTransferStart/Complete/Cancel variants)
  - plan 04-02 (map_command_result helper + validate_leg + require_session + dispatch_console_command entry point)
provides:
  - POST /api/v1/calls/{id}/transfer (tagged body: blind | attended)
  - POST /api/v1/calls/{id}/transfer/complete
  - POST /api/v1/calls/{id}/transfer/cancel
  - parse_target helper (SIP URI passthrough + E.164 normalization + rsipstack::sip::Uri validation)
  - CommandResult.payload (Option<serde_json::Value>) + success_with_payload + Default impl
  - SessionSnapshot.pending_consult_leg_id (Option<String>, serde skip-if-none)
affects:
  - src/call/runtime/command_executor.rs (CommandResult extended; all constructors backfilled)
  - src/proxy/proxy_call/sip_session.rs (SessionSnapshot extended; construction sites backfilled)
  - src/call/adapters/console_adapter.rs (4 stub arms replaced with real CallCommand construction + 4 unit tests)
  - src/handler/api_v1/calls.rs (3 handlers + parse_target + TransferRequest + ConsultLegRequest + 7 unit tests)
  - tests/api_v1_calls.rs (+7 integration tests; total 24)
tech-stack:
  added: []
  patterns:
    - "Additive struct extension via explicit field backfill (no #[derive(Default)] regression risk): CommandResult.payload and SessionSnapshot.pending_consult_leg_id added with None at every existing construction site"
    - "CommandResult::Default impl + success_with_payload constructor so future plans can use ..Default::default() or a named payload constructor without churning existing call sites"
    - "Post-dispatch snapshot read: transfer_call reads handle.snapshot().and_then(|s| s.pending_consult_leg_id) AFTER dispatch_console_command and merges it as the extra arg to map_command_result — the SIP-session layer owns the stamp-before-REFER invariant; the handler is read-only"
    - "parse_target normalizes E.164 with external_ip fallback and validates BOTH paths via rsipstack::sip::Uri::try_from, so bad URIs and bad external_ip configs both surface as 400 at request-time rather than opaque 500s at REFER time (T-04-03-02, T-04-03-03)"
    - "D-21 default leg = callee encoded at handler layer (leg: Option<String> -> validate_leg -> unwrap_or(Leg::Callee))"
    - "CALL-10 preservation: all 3 transfer routes + the /transfer/complete and /transfer/cancel routes dispatch through dispatch_console_command verbatim — no direct send_command, no direct CallCommand construction in handlers"
key-files:
  created:
    - .planning/phases/04-active-calls-mid-call-control/04-03-SUMMARY.md
  modified:
    - src/call/runtime/command_executor.rs
    - src/proxy/proxy_call/sip_session.rs
    - src/call/adapters/console_adapter.rs
    - src/handler/api_v1/calls.rs
    - tests/api_v1_calls.rs
decisions:
  - "pending_consult_leg_id read is best-effort (T-04-03-05 accepted): if the SIP-layer attended-transfer handler has not stamped the snapshot by the time the handler returns, the client sees 200 without consult_leg_id. Phase 4 ships this window; the SIP session owns the stamp-before-REFER invariant in a later plan."
  - "validate_leg reused from Plan 04-02 rather than duplicated; leg is forward-compat in payload (session-layer picks the leg per D-21) but the adapter ignores it today (LegId::new(session_id))"
  - "parse_target validates normalized E.164 URIs post-format so misconfigured external_ip surfaces at request-time as `invalid external_ip configuration '...'` rather than at REFER dispatch — aligns with api_v1 'clean 4xx / opaque 5xx never' boundary"
  - "Branch remains sip_fix (not console_sip from the executor prompt) — sip_fix is the actual current worktree branch"
metrics:
  duration: "~6 min (Task 3 only; Tasks 1 and 2 pre-committed as 283485b and 7757a4a)"
  completed: 2026-04-21
---

# Phase 4 Plan 03: Transfer Routes — Summary

Shipped the full mid-call transfer surface for api_v1: blind transfer,
attended-transfer start, attended-transfer complete, and attended-transfer
cancel. All four reach the session layer through `dispatch_console_command`
verbatim so CALL-10 holds by construction. The attended-start path reads
`SessionSnapshot::pending_consult_leg_id` post-dispatch and surfaces the id
in the HTTP response body without touching the SIP session from the
handler. Closes **CALL-04**. Advances **CALL-10** with three more
dispatch-through routes.

## Routes

| Method | Path | Body | Success Body |
|--------|------|------|--------------|
| POST | `/api/v1/calls/{id}/transfer` | `{"type":"blind","target":"sip:...","leg":"caller\|callee"}` | `{"message":"dispatched"}` |
| POST | `/api/v1/calls/{id}/transfer` | `{"type":"attended","target":"+14155551234"}` | `{"message":"dispatched","consult_leg_id":"<id>"}` (when snapshot stamped) |
| POST | `/api/v1/calls/{id}/transfer/complete` | `{"consult_leg":"<id>"}` | `{"message":"dispatched"}` |
| POST | `/api/v1/calls/{id}/transfer/cancel`   | `{"consult_leg":"<id>"}` | `{"message":"dispatched"}` |

**Error envelope (inherited from plans 04-01/02):** `{"error":"...","code":"bad_request|not_found|conflict|internal"}`.

### Status matrix

| Case | Status |
|------|--------|
| Missing Bearer | 401 |
| Unknown session id | 404 |
| Invalid transfer type (`{"type":"foo"}`) | 400 (serde rejection or our `bad_request` shape depending on axum extraction order) |
| Invalid target (not sip:/sips:/+E.164) | 400 `bad_request` with `"expected SIP URI (sip:/sips:) or E.164 (+...)"` |
| Invalid SIP URI (e.g. control chars) | 400 `bad_request` with `"invalid target URI '...'"` |
| Invalid `external_ip` config during E.164 normalization | 400 `bad_request` with `"invalid external_ip configuration '...'"` |
| Unknown `consult_leg` on complete/cancel | 404 (via `map_command_result` D-07 "not found" pattern — requires the SIP-layer `TransferComplete`/`TransferCancel` handler to return a failure with `"not found"` substring) |
| Session rx dropped | 409 `conflict` with `"command dispatch failed: ..."` |
| Blind happy | 200 `{"message":"dispatched"}` |
| Attended happy, snapshot stamped | 200 `{"message":"dispatched","consult_leg_id":"<id>"}` |
| Attended happy, snapshot NOT yet stamped (race) | 200 `{"message":"dispatched"}` — see Known Limitations |

## CommandResult extension (Task 1, commit `283485b`)

Added `payload: Option<serde_json::Value>` (last field) and a
`success_with_payload` constructor plus `impl Default for CommandResult`.
All four existing constructors (`success`, `success_with_leg`, `failure`,
`degraded`, `not_supported`) backfill `payload: None`. This is the
**future-hook** for any CommandResult that needs to return structured data
alongside `success: true` — plan 04-05's `recording_path` is the next
planned consumer. Today the field is unused on the wire because the
attended-transfer consult-leg-id flow is handled via the handler-side
SessionSnapshot read rather than CommandResult payload propagation (the
adapter would have needed to re-enter the session, which is out of
character for the dispatcher's fire-and-map shape).

## SessionSnapshot extension (Task 1, commit `283485b`)

Added `pending_consult_leg_id: Option<String>` with
`#[serde(skip_serializing_if = "Option::is_none")]`. Every
`SessionSnapshot {}` construction site in `src/proxy/proxy_call/sip_session.rs`
plus both test literals in `tests/api_v1_calls.rs` (the `get_active_call_by_id_returns_rich_view`
literal and the `seed_active_call` helper) backfill `pending_consult_leg_id: None`.

**Contract for future SIP-layer contributors:** the
`CallCommand::Transfer { attended: true, .. }` handler in
`src/proxy/proxy_call/session.rs` **MUST** stamp
`handle.update_snapshot(SessionSnapshot { pending_consult_leg_id: Some(...), .. })`
BEFORE returning `CommandResult::success(...)` and BEFORE issuing the
outbound INVITE or consult-leg REFER. The api_v1 handler reads the
snapshot synchronously after `dispatch_console_command` returns; any
asynchronous stamp after that point is missed (best-effort; see Known
Limitations).

## console_adapter arms (Task 2, commit `7757a4a`)

Replaced four stubs:

| Payload variant | CallCommand emitted |
|-----------------|---------------------|
| `BlindTransfer { target, leg }` | `CallCommand::Transfer { leg_id: LegId::new(session_id), target, attended: false }` |
| `AttendedTransferStart { target, leg }` | `CallCommand::Transfer { leg_id: LegId::new(session_id), target, attended: true }` |
| `AttendedTransferComplete { consult_leg }` | `CallCommand::TransferComplete { consult_leg: LegId::new(consult_leg) }` |
| `AttendedTransferCancel { consult_leg }` | `CallCommand::TransferCancel { consult_leg: LegId::new(consult_leg) }` |

`leg` is accepted and forward-compatible — the session-layer picks the leg
per D-21 default=callee. Four new adapter unit tests cover each arm.

## parse_target semantics (Task 3, commit `8d89643`)

```rust
fn parse_target(raw: &str, external_ip: Option<&str>) -> ApiResult<String>
```

| Input | Output | Notes |
|-------|--------|-------|
| `"sip:1001@example.com"` | `"sip:1001@example.com"` | validated via `rsipstack::sip::Uri::try_from`, passed through |
| `"sips:alice@secure.x"` | `"sips:alice@secure.x"` | validated, passed through |
| `"+14155551234"` with `external_ip = Some("1.2.3.4")` | `"sip:+14155551234@1.2.3.4"` | normalized + validated |
| `"+14155551234"` with `external_ip = None` | `"sip:+14155551234@127.0.0.1"` | fallback + validated (production MUST set `external_ip`) |
| `"4155551234"` (no `+`) | `400 bad_request` | |
| `""` or `"   "` | `400 bad_request` ("empty target") | |
| `"not-a-uri"` | `400 bad_request` | |
| Malformed URI after normalization | `400 bad_request` ("invalid external_ip configuration...") | surfaces misconfiguration at request-time |

6 parse_target unit tests in `src/handler/api_v1/calls.rs` plus 2
integration tests (`blind_transfer_e164_normalizes_with_localhost_fallback`,
`transfer_invalid_target_returns_400`).

## Test inventory

### `tests/api_v1_calls.rs` — 24 integration tests (17 prior + 7 new)

- `transfer_requires_auth` — 401 without Bearer
- `blind_transfer_dispatches` — happy blind, asserts `CallCommand::Transfer { attended: false, target: "sip:1001@example.com", .. }`
- `blind_transfer_e164_normalizes_with_localhost_fallback` — E.164 target → `sip:+14155551234@127.0.0.1`
- `attended_transfer_returns_consult_leg_id` — pre-stamps snapshot, asserts body has `consult_leg_id`
- `transfer_invalid_target_returns_400` — `"not-a-uri"` rejected
- `transfer_complete_dispatches` — asserts `CallCommand::TransferComplete { consult_leg: "consult-xyz" }`
- `transfer_cancel_dispatches_and_unknown_call_is_404` — happy cancel + 404 on unknown session

### Unit tests (`cargo test -p rustpbx --lib handler::api_v1::calls`) — 19 total

Includes 7 `parse_target` cases (SIP passthrough, SIPS passthrough, E.164
+ external_ip, E.164 + localhost fallback, plain number rejected, empty/
whitespace rejected, garbage rejected) on top of the 6 `validate_leg` /
`map_command_result` / existing-filter tests from plans 04-01/02.

### Adapter unit tests (`cargo test -p rustpbx --lib call::adapters::console_adapter`) — 10 total

4 new tests for the 4 transfer arms on top of the 6 prior.

### Regression

- `api_v1_trunks` — 23 passed (unchanged baseline)
- Phase 1/2/3 baseline preserved

## Requirements

- **CALL-04** — **closed**: blind + attended-start + attended-complete +
  attended-cancel all ship through `dispatch_console_command`; attended
  response carries `consult_leg_id` when the SIP layer has stamped the
  snapshot.
- **CALL-10** — **advanced**: three more dispatch-through routes; no
  handler calls `send_command` directly.

## Known Limitations

### Consult-leg-id race (T-04-03-05, accepted)

The attended-transfer response body includes `consult_leg_id` ONLY when
the SIP-layer attended-transfer handler has stamped
`SessionSnapshot::pending_consult_leg_id` by the time
`dispatch_console_command` returns and the api_v1 handler reads
`handle.snapshot()`. If the stamp happens asynchronously after the
handler's read, the client sees a 200 without `consult_leg_id` and must
either retry the `/api/v1/calls/{id}` GET to fetch the snapshot, or fall
back to hangup/bridge webhooks to observe the consult leg.

**Ownership:** the SIP-session layer (Plan 04-04 or later) owns the
stamp-before-REFER invariant. The api_v1 handler is intentionally
read-only.

### Unknown-consult-leg 404 path depends on SIP-layer return message

`/transfer/complete` and `/transfer/cancel` return 404 when the SIP-layer
`TransferComplete` / `TransferCancel` handlers return a `CommandResult`
with a failure message containing `"not found"`. If the SIP layer
returns a different message shape for unknown consult_leg, the response
degrades to 500 (internal). Plan 04-04 or the SIP-layer owner should
audit that the D-07 "not found" pattern is emitted on this path.

## Hand-off to Plan 04-04 (speak / play / dtmf)

- **`map_command_result`** takes `Option<serde_json::Value>` extra — plan 04-04 can merge structured fields (e.g., `play_id`, `dtmf_buffered`) the same way this plan merges `consult_leg_id`.
- **`CommandResult::payload`** hook is ready for any future flow that wants to propagate structured data from the session layer through the dispatcher (e.g., plan 04-05's `recording_path`).
- **`parse_target`** is transfer-specific — plans 04-04/05 don't use it, but if a future plan needs to accept a SIP URI (e.g., `/forward`), the helper is ready to reuse.
- **`validate_leg`** and **`require_session`** remain the canonical leg validation and 404 pre-check helpers — reuse verbatim.

## Files Modified

- `src/call/runtime/command_executor.rs` — `CommandResult.payload` + `success_with_payload` + `Default` (commit `283485b`)
- `src/proxy/proxy_call/sip_session.rs` — `SessionSnapshot.pending_consult_leg_id` (commit `283485b`)
- `src/call/adapters/console_adapter.rs` — 4 transfer arms + 4 unit tests (commit `7757a4a`)
- `src/handler/api_v1/calls.rs` — 3 handlers + `parse_target` + `TransferRequest` + `ConsultLegRequest` + 7 unit tests (commit `8d89643`)
- `tests/api_v1_calls.rs` — +7 integration tests (commit `fbe3c67`)

## Commits

- `283485b feat(04-03): extend CommandResult.payload and SessionSnapshot.pending_consult_leg_id`
- `7757a4a feat(04-03): wire BlindTransfer/AttendedTransfer* adapter arms to CallCommand`
- `8d89643 feat(04-03): add /transfer blind+attended+complete+cancel handlers`
- `fbe3c67 test(04-03): integration tests for transfer routes`

## Deviations from Plan

None — plan executed as written. Branch is `sip_fix` (matching Plan 04-02's
branch decision; the executor prompt reference to `console_sip` was
inherited from an earlier doc and does not exist in this worktree).

## Self-Check: PASSED

- `src/call/runtime/command_executor.rs` — contains `pub payload: Option<serde_json::Value>` and `fn success_with_payload`
- `src/proxy/proxy_call/sip_session.rs` — contains `pub pending_consult_leg_id: Option<String>`
- `src/call/adapters/console_adapter.rs` — 4 new arms + 4 unit tests
- `src/handler/api_v1/calls.rs` — 3 handlers, `parse_target`, `TransferRequest`, `ConsultLegRequest`
- `tests/api_v1_calls.rs` — 24 integration tests passing
- Commits `283485b`, `7757a4a`, `8d89643`, `fbe3c67` all present in `git log`
- Regression: `api_v1_trunks` 23 passed (baseline preserved)
