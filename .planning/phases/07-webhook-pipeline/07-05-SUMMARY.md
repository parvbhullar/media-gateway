---
phase: 07-webhook-pipeline
plan: 05
subsystem: webhooks/emit-sites + test-event + cancel
tags: [webhooks, integration, emit-sites, test-event, phase7, IT-WH, wave4]
requires: [07-01, 07-02, 07-03, 07-04]
provides:
  - Five emit sites wired (call.started, call.failed, call.completed,
    recording.completed, transcribe.requested)
  - Synchronous webhook.test fire on POST /api/v1/webhooks (D-28..D-30)
  - WebhookCancelRegistry triggered on PUT/DELETE (D-31, D-34)
  - pub fn deliver_test_event in proxy/webhook/processor.rs
  - tests/proxy_webhook_pipeline.rs (IT-WH, 8 cases)
affects:
  - src/proxy/proxy_call/sip_session.rs (call.started, call.failed)
  - src/callrecord/storage.rs (call.completed builder + emit helper)
  - src/callrecord/mod.rs (recv_loop emits call.completed; builder gains
    with_webhook_sender)
  - src/handler/api_v1/calls.rs (recording.completed + transcribe.requested)
  - src/handler/api_v1/webhooks.rs (CreateWebhookResponse, test fire,
    cancel-on-PUT, cancel-on-DELETE)
  - src/proxy/webhook/processor.rs (additive deliver_test_event)
  - src/proxy/webhook/mod.rs (re-export deliver_test_event)
  - tests/proxy_webhook_pipeline.rs (NEW IT-WH suite)
tech-stack:
  added: []
  patterns:
    - "Broadcast-channel-decoupled emit pattern: source modules call
      `state.webhook_sender().send(event)` (or `inner.webhook_sender.send(...)`)
      with no further coupling to the webhook subsystem."
    - "Pure JSON event-builder helpers per emit site for unit-testable
      shape parity with D-07."
    - "Synchronous deliver_test_event helper reuses perform_attempt for
      header/signing parity with the async retry path (D-30)."
key-files:
  created:
    - tests/proxy_webhook_pipeline.rs
  modified:
    - src/proxy/proxy_call/sip_session.rs
    - src/callrecord/storage.rs
    - src/callrecord/mod.rs
    - src/handler/api_v1/calls.rs
    - src/handler/api_v1/webhooks.rs
    - src/proxy/webhook/processor.rs
    - src/proxy/webhook/mod.rs
decisions:
  - "Test event uses inline tokio::time::timeout(webhook.timeout_ms)
    wrapper around deliver_test_event so the POST handler is bounded
    even if the receiver hangs (T-07-05-03 mitigation)."
  - "call.failed is suppressed when answer_time is set: a successfully
    answered call's terminal lifecycle event is call.completed, emitted
    from the callrecord finalize path. Avoids double-fire when both
    paths run for the same call."
  - "call.completed `data` is the full CallRecord JSON via
    serde_json::to_value(record) — no translation layer (D-07)."
  - "transcribe.requested event fires INSIDE maybe_drop_transcribe_marker
    only on marker-write success — consumes Phase 4 D-18 hand-off contract."
  - "POST test cases in IT-WH call deliver_test_event directly because
    the URL validator (D-27) denies the loopback mock URL via the API
    path. The CreateWebhookResponse → test_delivery contract is verified
    via lib-level tests for the response shape; the helper outcome
    behavior is verified end-to-end."
  - "Webhook sender plumbing into CallRecordManager added via
    `with_webhook_sender(sender)` builder method. Production wiring at
    server boot (app.rs construction site) is OUT OF SCOPE for this plan
    (app.rs is in the forbidden file set). The plumbing is complete; a
    follow-up wiring patch in app.rs will activate call.completed in
    production. Until then, the broadcast still flows via direct
    `state.webhook_sender().send(...)` from any other emit site."
metrics:
  tasks: 5
  commits: 5
  tests_added: 18
  files_modified: 7
  files_created: 1
requirements: [WH-02, WH-04, WH-05, WH-06]
---

# Phase 7 Plan 05: Webhook Emit Sites + Test Event + Cancel Wiring

**One-liner:** Wires the five Phase-7 emit sites (call.started/failed in
`sip_session.rs`, call.completed in `callrecord/storage.rs`,
recording.completed + transcribe.requested in `handler/api_v1/calls.rs`),
ships the synchronous `webhook.test` fire on POST `/api/v1/webhooks` per
D-28..D-30 with `test_delivery` in the response, hooks the
`WebhookCancelRegistry` on PUT/DELETE per D-31/D-34, and lands an 8-case
IT-WH integration test exercising the full subscribe→filter→deliver→retry→
fallback→cancel pipeline through the live processor task spawned in 07-04.

## Emit Sites

| Event | File | Line | Trigger |
|-------|------|------|---------|
| `call.started` | `src/proxy/proxy_call/sip_session.rs` | ~2219 | After `accept_call` sends 200 OK (sets `answer_time`). |
| `call.failed` | `src/proxy/proxy_call/sip_session.rs` | ~2430 | Inside `cleanup` when `answer_time.is_none()` (early termination, dialplan failure, timeout, 3xx/4xx/5xx). Suppressed for answered calls — those emit `call.completed`. |
| `call.completed` | `src/callrecord/storage.rs` | builder `build_call_completed_event` line 35 + emit `emit_call_completed` line 43; called from `src/callrecord/mod.rs::recv_loop` after successful save. | After persistence success. `data` = full `serde_json::to_value(&CallRecord)`. |
| `recording.completed` | `src/handler/api_v1/calls.rs` | builder at 924; emit at 975 in `record_call`. | After dispatch returns success on `/record`. Reads file metadata for `size_bytes`. |
| `transcribe.requested` | `src/handler/api_v1/calls.rs` | inside `maybe_drop_transcribe_marker` at 886, emit branch after `File::create` succeeds. | Phase 4 D-18 hand-off consumed; emit ONLY on marker-write success when `transcribe=true`. |

The Phase-4 D-18 hand-off audit comment lives in the doc comment on
`maybe_drop_transcribe_marker` (line ~880) and inline at the emit branch.

## Test Event (POST /api/v1/webhooks)

`src/proxy/webhook/processor.rs` exports:

```rust
pub async fn deliver_test_event(
    webhook: &WhModel,
    event: &WebhookEvent,
    envelope_body: &str,
    client: &reqwest::Client,
) -> Result<(), String>
```

— single attempt, no retry, no disk fallback, reuses `perform_attempt` for
identical header + HMAC signing as the async retry path (D-30).

`POST /api/v1/webhooks` now returns `CreateWebhookResponse`:

```json
{
  "id": "...", "name": "...", "url": "...", "secret": "...",
  "events": [...], "description": null, "is_active": true,
  "retry_count": 3, "timeout_ms": 5000,
  "created_at": "...", "updated_at": "...",
  "test_delivery": "succeeded" | "failed",
  "test_error": "<message if failed>"
}
```

Outer `tokio::time::timeout(webhook.timeout_ms)` wraps the helper call.
Failure (non-2xx, network error, or timeout) is non-fatal per WH-05: row
persists, response is 201.

## Cancel-on-Mutate

- `update_webhook` calls `state.webhook_cancel_registry().cancel(&id)` BEFORE
  validating the new payload (D-34: prior delivery loop must not see the new
  state).
- `delete_webhook` calls the same BEFORE the row is removed (D-31).

Both are tested end-to-end in `it_wh_delete_cancels_in_flight_retry`.

## IT-WH Tests (`tests/proxy_webhook_pipeline.rs`)

Eight cases, all green:

1. `it_wh_call_completed_delivers_with_valid_hmac` — covers WH-02 + WH-04
   (4 headers + HMAC v1 round-trip). Recomputes signature via
   `signer::signature_header` and asserts byte-equality.
2. `it_wh_retry_exhausts_writes_disk_fallback` — covers WH-03 (retry-exhaust
   → disk fallback at `{generated_dir}/webhooks/failed/`).
3. `it_wh_permanent_fail_400_immediate_fallback` — covers D-21 (4xx ≠
   408/429 → permanent, 1 attempt only).
4. `it_wh_delete_cancels_in_flight_retry` — covers WH-06 / D-31 (DELETE
   during retry sleep stops further attempts).
5. `it_wh_deliver_test_event_success` — covers WH-05 (200 mock → Ok).
6. `it_wh_deliver_test_event_failure` — covers WH-05 (500 mock → Err with
   "500" in message; non-fatal-row-persistence verified via
   api_v1_webhooks lib tests).
7. `it_wh_transcribe_requested_fires` — covers Phase 4 D-18 hand-off
   end-to-end deliverability.
8. `it_wh_event_filter_excludes_unsubscribed` — covers D-10 filter
   (events=["call.completed"] does NOT receive call.started; control
   shows call.completed IS delivered).

POST cases (#5, #6) call `deliver_test_event` directly because the API URL
validator (D-27) rejects the 127.0.0.1 mock URL. The `test_delivery` field
contract is verified at the unit-test level via the shape of
`CreateWebhookResponse` and the existing 16/16 api_v1_webhooks tests
continue to pass with the new flattened response.

## Threat-model coverage (from PLAN frontmatter)

| Threat | Mitigation in 07-05 |
|--------|---------------------|
| T-07-05-01 (PII in call.completed) | Accepted — operator opted in; v2.1 may add per-event redaction. |
| T-07-05-02 (Spoofed internal URL) | URL validator (07-02) enforced D-26/D-27 at write time. |
| T-07-05-03 (Sync test event blocks POST) | `tokio::time::timeout(webhook.timeout_ms)` + single-attempt helper bound the call. |
| T-07-05-04 (Silent emit failure) | Send errors logged at trace via existing tracing; non-fatal per D-06. |
| T-07-05-05 (transcribe path leak) | `recording_path` is operator-controlled; same surface as Phase 4 D-18. |
| T-07-05-06 (PUT race with retry) | `cancel_registry().cancel()` BEFORE applying changes (D-34). |
| T-07-05-07 (test mode abuse) | Single-attempt only; no retry queue exposure. |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Storage test used wrong field name post-camelCase rename**
- **Found during:** Task 2 verification (`cargo test callrecord::storage`).
- **Issue:** `CallRecord` uses `#[serde(rename_all = "camelCase")]` so
  `call_id` serializes to `callId`. Initial test asserted `data["call_id"]`
  → null because the key is actually `callId`.
- **Fix:** Updated assertions to use `data["callId"]`. Also documented the
  serializer convention inline.
- **Files modified:** `src/callrecord/storage.rs`
- **Commit:** included in `2d5c959`

**2. [Rule 3 - Blocking] IT-WH cases used API path with loopback URL → URL validator rejected**
- **Found during:** Task 5 first run (8/8 cases failed with `bad_request` from D-27 denylist).
- **Issue:** Mock server binds to `127.0.0.1:0` (ephemeral), but
  `validate_webhook_url` denies loopback per D-27.
- **Fix:** Added `seed_webhook_direct(state, ...) -> id` helper that
  inserts the webhook row directly via SeaORM, bypassing the API validator.
  Production POST still validates per D-27. The two POST-test-event cases
  call `deliver_test_event` directly with a fixture `WhModel`.
- **Files modified:** `tests/proxy_webhook_pipeline.rs`
- **Commit:** `521d790`

### Architectural notes (not deviations, but documented for next plan)

- `CallRecordManagerBuilder::with_webhook_sender(sender)` is added but the
  production wiring at the construction site (`src/app.rs:273`) is NOT
  modified — `app.rs` is in this plan's forbidden file set per the user
  prompt. Until a follow-up patch wires it in, the recv_loop's
  `webhook_sender_ref` is `None` in production and `call.completed` will
  not emit from the CDR finalize path. The other 4 emit sites are fully
  active. A 1-line `.with_webhook_sender(state.webhook_sender())` in
  app.rs closes this gap.
- `tests/common/mod.rs` was NOT extended with `start_test_webhook_server`
  / `seed_webhook` because the IT-WH file is the only consumer in this
  phase; helpers live there. Extracting them is straightforward when
  Phase 8/9 adds another integration test that needs the same pattern.

## Verification Run

- `cargo check -p rustpbx --lib` — clean.
- `cargo check -p rustpbx --release` — clean (56s).
- `cargo test -p rustpbx --lib proxy::webhook` — 37 passed.
- `cargo test -p rustpbx --lib handler::api_v1::webhooks` — 23 passed.
- `cargo test -p rustpbx --lib handler::api_v1::calls` — 35 passed.
- `cargo test -p rustpbx --test api_v1_webhooks` — 16 passed.
- `cargo test -p rustpbx --test proxy_webhook_pipeline` — 8 passed.

## File Ownership

`git diff --name-only` against the Phase 7 starting commit lists ONLY:

```
src/callrecord/mod.rs
src/callrecord/storage.rs
src/handler/api_v1/calls.rs
src/handler/api_v1/webhooks.rs
src/proxy/proxy_call/sip_session.rs
src/proxy/webhook/mod.rs
src/proxy/webhook/processor.rs
tests/proxy_webhook_pipeline.rs
```

ZERO diff against forbidden set: `src/handler/api_v1/mod.rs`,
`src/models/migration.rs`, `src/proxy/server.rs`, `src/app.rs`.

## Commits

- `3231cad` — Task 1: call.started + call.failed in sip_session
- `2d5c959` — Task 2: call.completed builder/emit + recv_loop hook
- `9249c1c` — Task 3: recording.completed + transcribe.requested
- `8ee7116` — Task 4: test event on POST + cancel on PUT/DELETE
- `521d790` — Task 5: IT-WH 8-case integration test suite

## Self-Check: PASSED

- `src/proxy/proxy_call/sip_session.rs` FOUND
- `src/callrecord/storage.rs` FOUND
- `src/callrecord/mod.rs` FOUND
- `src/handler/api_v1/calls.rs` FOUND
- `src/handler/api_v1/webhooks.rs` FOUND
- `src/proxy/webhook/processor.rs` FOUND
- `src/proxy/webhook/mod.rs` FOUND
- `tests/proxy_webhook_pipeline.rs` FOUND
- Commit `3231cad` FOUND
- Commit `2d5c959` FOUND
- Commit `9249c1c` FOUND
- Commit `8ee7116` FOUND
- Commit `521d790` FOUND
