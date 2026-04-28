---
phase: 07-webhook-pipeline
plan: 02
subsystem: api_v1
tags: [webhooks, crud, validation, ssrf, phase7]
requirements: [WH-01, WH-06]
provides:
  - WebhookView, CreateWebhookRequest, UpdateWebhookRequest wire types
  - validate_webhook_url, validate_event_names, validate_name, validate_timeout_ms, validate_retry_count (all pub for 07-04/07-05 reuse)
  - WEBHOOK_EVENT_NAMES const (locked D-05 set)
  - 5 CRUD handlers replacing 07-01 stubs
key-files:
  modified:
    - src/handler/api_v1/webhooks.rs
  created:
    - tests/api_v1_webhooks.rs
    - .planning/phases/07-webhook-pipeline/07-02-SUMMARY.md
decisions:
  - "RFC1918 explicitly ALLOWED in validate_webhook_url (D-27): operators legitimately webhook to k8s/internal services; trust model differs from Phase 6 HttpQuery (call-time vs setup-time input)."
  - "Test-event firing on POST DEFERRED to 07-05 (D-28..D-30): processor doesn't exist yet; 07-02 returns bare WebhookView with no test_delivery field."
  - "DELETE-triggered cancel (D-31) and PUT-triggered cancel (D-34) DEFERRED to 07-05: cancel registry plumbed in 07-01 but processor not yet subscribed."
metrics:
  tasks_completed: 3
  files_changed: 2
  unit_tests: 23
  integration_tests: 16
---

# Phase 7 Plan 07-02: Webhooks CRUD Summary

Replaced 07-01 stub bodies with full `/api/v1/webhooks[/{id}]` CRUD against `supersip_webhooks`, including five public validators (URL, event-names, name, timeout_ms, retry_count) and 16 IT-01 integration tests.

## What Landed

### Validators (`src/handler/api_v1/webhooks.rs`)
- `pub fn validate_webhook_url(url) -> Result<(), String>` — http/https scheme check; localhost/127.0.0.0/8/::1/fe80::/10/unspecified denied; RFC1918 (10/8, 172.16/12, 192.168/16) explicitly ALLOWED per D-27.
- `pub fn validate_event_names(events) -> Result<(), String>` — every name must be in locked `WEBHOOK_EVENT_NAMES` set (D-05). Empty list is OK (subscribe-all per D-08). Error message lists valid events (D-09).
- `pub fn validate_name(name)` — lowercase letters/digits + dashes only, ≤128 chars (URL-safe).
- `pub fn validate_timeout_ms(t)` — `[100, 30000]` per D-04.
- `pub fn validate_retry_count(n)` — `[0, 10]` per D-04.

### Handlers
- `GET /api/v1/webhooks` — list, ordered by name ASC.
- `POST /api/v1/webhooks` — create with full field validation, UUID v4 id generation, 409 on duplicate name, returns 201 + `WebhookView`.
- `GET /api/v1/webhooks/{id}` — fetch by id, 404 on missing.
- `PUT /api/v1/webhooks/{id}` — partial-update friendly full-replacement (every Option field is optional), revalidates per-field, 409 on rename collision, 404 on missing.
- `DELETE /api/v1/webhooks/{id}` — 204 on success, 404 on missing.

### Tests
- 23 validator unit tests (`cargo test -p rustpbx --lib handler::api_v1::webhooks::validators`) — all green.
- 16 integration tests (`cargo test -p rustpbx --test api_v1_webhooks`) — all green. Covers all 6 IT-01 categories: 401-no-auth, happy-path, missing-404, duplicate-409, bad-input-400, validation cases.

## Deviations from Plan

None — plan executed exactly as written.

## Deferred to 07-05 (explicit hand-off)

Both deferrals are documented inline in `webhooks.rs` with `07-05` / `D-28` / `D-31` / `D-34` markers so 07-05 can grep and wire them in:

1. **Synchronous test-event firing on POST** (D-28..D-30). The plan body for 07-02 was scoped to "POST simply creates the row and returns 201" because the webhook event processor lands in 07-04 (signer + processor) and 07-05 (the actual `WebhookEventSender::send_test`). When 07-05 runs, replace the comment block in `create_webhook` with a fire-and-forget `state.webhook_event_sender().send_test(&inserted).await` call and add a `test_delivery: TestDeliveryReport` field to `WebhookView` that surfaces success/failure of the synchronous probe.

2. **Cancel-registry triggers on DELETE / PUT** (D-31, D-34). The `WebhookCancelRegistry` was plumbed into `AppState` in 07-01, but 07-02 has no consumer to cancel against because the processor doesn't subscribe yet. When 07-05 wires the processor, replace the placeholder comments in `delete_webhook` / `update_webhook` with `state.webhook_cancel_registry().cancel(&id).await` so any in-flight retry for the modified webhook is aborted before the row mutates.

## D-27 RFC1918-Allowed Decision (intentional, not a gap)

Phase 6's `routing_records.rs` SSRF validator denies RFC1918 ranges because HttpQuery URLs are evaluated at *call time* against operator-supplied call data — a runtime SSRF surface. Phase 7 webhooks differ: the URL is set once at *config time* by the operator and never re-evaluated against external input. Operators legitimately need to webhook to internal services (k8s, service mesh, corporate networks), so D-27 explicitly allows RFC1918. The denylist is restricted to local-loopback (`127.0.0.0/8`, `::1`, `fe80::/10`, `localhost`, `0.0.0.0`).

The integration test `it_wh_create_rfc1918_url_allowed_d27` is a regression assertion against any future tightening.

## Wave-1 Ownership Preserved

`git diff --name-only HEAD~2 HEAD` shows ONLY:
- `src/handler/api_v1/webhooks.rs`
- `tests/api_v1_webhooks.rs`

ZERO diff against `src/handler/api_v1/mod.rs`, `src/models/migration.rs`, `src/proxy/server.rs`, `src/app.rs`, `src/proxy/mod.rs`.

## Verification

- `cargo check -p rustpbx --lib` — clean.
- `cargo test -p rustpbx --lib handler::api_v1::webhooks::validators` — 23 passed.
- `cargo test -p rustpbx --test api_v1_webhooks` — 16 passed.

## Self-Check: PASSED
- src/handler/api_v1/webhooks.rs: FOUND
- tests/api_v1_webhooks.rs: FOUND
- 065d571 (Task 1+2 feat commit): FOUND
- c2e60e4 (Task 3 test commit): FOUND
