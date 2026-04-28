---
phase: 07-webhook-pipeline
plan: 04
subsystem: webhooks/processor
tags: [webhooks, processor, retry, disk-fallback, hmac, phase7]
requires: [07-01, 07-02, 07-03]
provides: [run_webhook_processor, deliver_webhook, compute_backoff, parse_retry_after, build_request_headers, write_disk_fallback]
affects:
  - src/proxy/webhook/processor.rs
  - src/proxy/webhook/mod.rs
  - src/proxy/server.rs
tech_added: []
patterns:
  - "Per-event fresh DB read (mirrors Phase 5 D-17 / Phase 6 D-29 fresh-read pattern)"
  - "Per-webhook tokio::spawn fan-out for failure isolation (D-12)"
  - "Stripe-style HMAC signature header `t=<ts>,v1=<hex>` (D-15)"
  - "Test-only injectable backoff schedule via deliver_webhook_with_schedule"
key_files:
  created: []
  modified:
    - src/proxy/webhook/processor.rs
    - src/proxy/webhook/mod.rs
    - src/proxy/server.rs
decisions:
  - "Total-attempts semantics: retry_count=N → 1 initial + N retries = (N+1) total attempts"
  - "HTTP-date Retry-After NOT supported (integer-seconds only) — documented limitation"
  - "Non-unix file mode 0600 fallback: inherit umask defaults; documented limitation"
  - "Database is optional in server config; processor only spawns when DB is present"
  - "Test backoff schedule injected via deliver_webhook_with_schedule helper to keep retry tests fast (5ms × 3 instead of 1s+5s+30s)"
metrics:
  tasks: 3
  commits: 3
  tests_added: 19
  files_modified: 3
requirements: [WH-02, WH-03]
---

# Phase 7 Plan 04: Webhook Delivery Processor Summary

Implemented the full webhook delivery processor: a background task subscribing to `WebhookEventSender` that fans events out to all matching active webhooks with HMAC signing, retry-with-jitter, status-policy classification, Retry-After honoring, pre-flight DB recheck, and disk fallback.

## What Was Built

### `src/proxy/webhook/processor.rs` (full body)

- **Helpers (Task 1):**
  - `compute_backoff(attempt_idx, retry_count, schedule) -> Option<Duration>` — schedule slot lookup with `±25%` jitter via `rand::thread_rng().gen_range`. Returns `None` when retries are exhausted, signaling the caller to fall back to disk.
  - `parse_retry_after(value: &str) -> Option<Duration>` — parses integer-seconds only.
  - `build_request_headers(...)` — assembles `Content-Type`, `User-Agent: supersip/<CARGO_PKG_VERSION>`, `X-Webhook-Event`, `X-Webhook-Secret` (D-16 known-weakness, in-source comment cites threat_model T-07-04-01), `X-Webhook-Request-Id`, `X-Webhook-Signature: t=<ts>,v1=<hex>`.
  - `write_disk_fallback(...)` — writes JSON envelope per D-24 schema (`envelope`, `webhook_id`, `webhook_url`, `attempts[]`, `first_attempt_at`, `final_failure_at`) under `{generated_dir}/webhooks/failed/{ts}-{wid}-{eid}.json`. Mode `0o600` on unix via `OpenOptionsExt::mode`; on non-unix the file inherits the process umask.
  - `AttemptOutcome` / `Verdict` (Success | PermanentFail | Retry) per D-21 status-code policy. `AttemptLog` mirrors D-24 schema with status, error string, duration_ms only — never headers (T-07-04-10).

- **Delivery driver (Task 2):**
  - `deliver_webhook(webhook, event, body, db, registry, generated_dir, client)` — public surface. Delegates to `deliver_webhook_with_schedule(..., DEFAULT_BACKOFF_SCHEDULE)`.
  - `deliver_webhook_with_schedule(..., schedule)` — internal variant accepting an injectable schedule for fast tests. Production callers always go through `deliver_webhook`.
  - Behavior matrix:
    - Cancel during attempt OR sleep → return silently, NO disk fallback (D-31, D-34).
    - `Verdict::Success` (2xx) → registry remove, return.
    - `Verdict::PermanentFail` (4xx except 408/429) → break to disk fallback.
    - `Verdict::Retry` (408 / 429 / 5xx / network err) → compute backoff, honor `Retry-After` if `≤ scheduled` (D-22), sleep, then DB recheck (D-32). If webhook missing or `is_active=false` → abort silently, NO disk fallback. Otherwise continue.
  - On retry exhaustion OR permanent fail → write disk fallback (mode 0600 on unix), then registry remove.

- **Processor (Task 3):**
  - `pub async fn run_webhook_processor(db, sender, registry, generated_dir, cancel)` — broadcast subscribe loop. Per event:
    1. Fresh DB SELECT `WHERE is_active = true` (D-12, mirrors Phase 5 D-17 / Phase 6 D-29).
    2. Per-webhook event-name filter (D-10): empty `events` array = subscribe-all.
    3. Per matching webhook → `tokio::spawn(deliver_webhook(...))` for failure isolation (D-12, T-07-04-04).
  - Broadcast errors handled: `Lagged(n)` → `tracing::warn!` and continue; `Closed` → break (channel sender dropped).
  - Cancellation: `cancel.cancelled()` race against `rx.recv()` → break gracefully.

### `src/proxy/webhook/mod.rs`

- Added `pub use processor::run_webhook_processor;` — the only edit.

### `src/proxy/server.rs`

- Added an additive `tokio::spawn(crate::proxy::webhook::run_webhook_processor(...))` block immediately after `webhook_cancel_registry` construction (around line 585). Guarded by `if let Some(db) = database.clone()` because `database` is `Option<DatabaseConnection>` in some embedded/test configurations and the processor requires DB access for the fresh-read.
- Uses `cancel_token.child_token()` so the processor exits cleanly on server shutdown.
- This is the **second and final** touch of `src/proxy/server.rs` in Phase 7. 07-05 must NOT modify this file.

## Locked Retry Semantics

`webhook.retry_count = N` → **1 initial + N retries = (N+1) total attempts**.

Source-of-truth comment in `src/proxy/webhook/processor.rs`:

```rust
// line 268: Total attempts = 1 initial + `webhook.retry_count` retries (see module docs).
```

Module-level docs (lines 17-23) document the same:

> `webhook.retry_count` denotes the number of RETRIES after the initial attempt. Therefore a `retry_count` of 3 (the default) yields up to `1 (initial) + 3 (retries) = 4` total attempts. `retry_count = 0` means exactly one attempt with no retries; on failure, an immediate disk fallback is written.

Test coverage:

- `deliver_zero_retries_writes_fallback_after_one_failure` — exercises `retry_count = 0` (1 attempt, immediate fallback, no sleep).
- `deliver_502_exhausts_retries_writes_fallback` — exercises `retry_count = 3` (4 attempts confirmed via mock-server hit counter, then fallback file written).

## Retry-After Parsing Limitation

Only **integer seconds** are supported (e.g., `Retry-After: 5`). HTTP-date format (e.g., `Retry-After: Wed, 21 Oct 2026 07:28:00 GMT`) is **NOT supported** — `parse_retry_after` returns `None` for any non-numeric input, which means the scheduled backoff is used unchanged.

Rationale: keeps the v2.0 implementation simple. The test `parse_retry_after_http_date_not_supported` documents this behavior. v2.1 may add HTTP-date parsing via `httpdate` if operator demand emerges.

## Non-Unix File Mode 0600 Fallback

The disk fallback file is opened with `mode(0o600)` via `std::os::unix::fs::OpenOptionsExt`, gated behind `#[cfg(unix)]`. On non-unix targets (Windows in particular) the call is a noop and the file inherits the process umask / Windows ACL defaults.

Rationale: mode bits aren't meaningful on Windows. Operators deploying on Windows can rely on NTFS ACLs at the directory level instead. The unit test `write_disk_fallback_uses_mode_0600_on_unix` is also `#[cfg(unix)]` gated.

This matches threat model entry **T-07-04-02** (accept non-unix umask inheritance with operator-side filesystem-level mitigation).

## Reference Test Vector for the Signer

The signer's reference vector lives in `src/proxy/webhook/signer.rs` test module (not modified in this plan but exercised end-to-end here). Key fixtures:

- `doc_reference_vector_matches_openssl` — canonical timestamp/body/secret combination cross-checked against `openssl dgst -sha256 -hmac` output.
- `signing_is_deterministic` — same inputs → same output.
- `signature_header_has_stripe_format` — produces `t=<unix-seconds>,v1=<64-hex>`.

In the processor, `build_request_headers` invokes `signer::sign(timestamp, body, &webhook.secret)` and embeds the result in the `X-Webhook-Signature` header per D-15. The unit test `build_request_headers_sets_six_expected_headers` confirms presence and Stripe-format shape.

## Test Coverage (19 new tests in processor module)

Helpers (Task 1) — 12 tests:

- `compute_backoff_first_attempt_within_jitter_band`
- `compute_backoff_third_attempt_uses_30s_with_jitter`
- `compute_backoff_zero_retry_count_returns_none`
- `compute_backoff_attempt_at_or_past_retry_count_returns_none`
- `compute_backoff_beyond_schedule_reuses_last_slot`
- `parse_retry_after_integer_seconds_supported`
- `parse_retry_after_invalid_returns_none`
- `parse_retry_after_http_date_not_supported`
- `verdict_classifies_status_codes_per_d21`
- `build_request_headers_sets_six_expected_headers`
- `write_disk_fallback_creates_file_with_expected_content`
- `write_disk_fallback_uses_mode_0600_on_unix` (`#[cfg(unix)]`)

Delivery + processor (Tasks 2/3) — 7 tests:

- `deliver_happy_200_one_attempt`
- `deliver_400_permanent_fail_writes_fallback`
- `deliver_502_exhausts_retries_writes_fallback`
- `deliver_zero_retries_writes_fallback_after_one_failure`
- `deliver_cancel_during_sleep_no_fallback`
- `deliver_db_deactivate_during_retry_aborts_no_fallback`
- `run_processor_dispatches_to_active_webhooks_only` (end-to-end: in-mem sqlite + axum mock + broadcast send → assert active webhook hit, inactive untouched)

All 37 webhook-module tests pass (signer + cancel_registry + processor + module-level).

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Test failure under `#[tokio::test(start_paused = true)]` for `deliver_502_exhausts_retries_writes_fallback`**

- **Found during:** Task 2 verification (test had been written by previous agent).
- **Issue:** With `start_paused = true`, `Database::connect("sqlite::memory:")` raised `PoolTimedOut` because sqlx's internal acquire-timeout virtual clock advanced before the connection task could run.
- **Fix:** Refactored `deliver_webhook` to delegate to a new `deliver_webhook_with_schedule(..., schedule: &'static [Duration])` helper. Production code path goes through `deliver_webhook` with `DEFAULT_BACKOFF_SCHEDULE` (unchanged behavior). Test uses `TEST_BACKOFF_SCHEDULE = [5ms, 5ms, 5ms]` and removes `start_paused = true`. Real-time elapsed for the 4-attempt retry test is now ~15ms.
- **Files modified:** `src/proxy/webhook/processor.rs`
- **Commit:** `e3cb9b1`

**2. [Rule 3 - Blocking] `database` is `Option<DatabaseConnection>` in `SipServerInner`, not unconditional**

- **Found during:** Task 3 first compile attempt.
- **Issue:** The plan called for `database.clone()` directly into `run_webhook_processor`, but `database` is `Option<DatabaseConnection>`. Type mismatch error E0308.
- **Fix:** Wrapped the spawn block in `if let Some(db_for_processor) = database.clone() { ... }`. When the server is built without a DB, the webhook processor is silently disabled (matches behavior of other DB-dependent subsystems in the same file).
- **Files modified:** `src/proxy/server.rs`
- **Commit:** `2396ade`

### Auth Gates

None.

### Architectural Changes

None.

## Wave 1 Ownership Verification

`git diff --name-only HEAD~2 HEAD` returns exactly:

```
src/proxy/server.rs
src/proxy/webhook/mod.rs
src/proxy/webhook/processor.rs
```

ZERO diff against the forbidden set: `src/handler/api_v1/mod.rs`, `src/models/migration.rs`, `src/models/mod.rs`, `src/proxy/mod.rs`, `src/app.rs`. Confirmed via `git diff --name-only HEAD~2 HEAD | grep -E "..."` returning no matches.

## Verification Run

- `cargo check -p rustpbx --lib` — clean (12.6s incremental).
- `cargo check -p rustpbx --release` — clean (2m12s).
- `cargo test -p rustpbx --lib proxy::webhook` — 37 passed, 0 failed (1.09s).

## Self-Check: PASSED

- `src/proxy/webhook/processor.rs` FOUND
- `src/proxy/webhook/mod.rs` FOUND
- `src/proxy/server.rs` FOUND
- Commit `e3cb9b1` FOUND (`feat(07-04): add webhook delivery + retry + disk fallback`)
- Commit `2396ade` FOUND (`feat(07-04): wire run_webhook_processor in proxy server boot`)
