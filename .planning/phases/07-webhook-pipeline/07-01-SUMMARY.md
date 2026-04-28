---
phase: 07-webhook-pipeline
plan: 01
subsystem: webhooks
tags: [webhooks, schema, migration, scaffolding, phase7, wave1]
requires:
  - Phase 6 routing_tables migration (last in Migrator order)
  - Phase 5 active_call_registry plumbing pattern (AppState delegation)
  - Phase 5 / Phase 6 stub-router precedent (501 NotImplemented)
provides:
  - supersip_webhooks table (NEW, forward-only migration)
  - WebhookEvent type (D-07 Stripe-style envelope)
  - WebhookEventSender broadcast channel alias (cap 1024 per D-11)
  - WebhookCancelRegistry struct (DashMap<String, CancellationToken> per D-31)
  - AppState accessors webhook_sender() + webhook_cancel_registry()
  - Stub /api/v1/webhooks router (5 routes, 501 placeholders)
affects:
  - src/models/migration.rs (appended one Migration entry)
  - src/handler/api_v1/mod.rs (one new pub mod + one new merge call)
  - src/proxy/mod.rs (one new pub mod)
  - src/proxy/server.rs (SipServerInner gains 2 fields + 2 accessors)
  - src/app.rs (AppStateInner gains 2 delegating accessors)
tech-stack:
  added: []
  patterns:
    - "Co-located entity + Migration (mirrors trunk_acl_entries.rs / routing_tables.rs)"
    - "Forward-only migration (Phase 6 D-05 convention; down() is no-op)"
    - "AppState delegation via SipServer.inner (mirrors active_call_registry)"
    - "Stub router pattern: same fn signature now and in Wave-2 so mod.rs is touched only once"
key-files:
  created:
    - src/models/webhooks.rs
    - src/handler/api_v1/webhooks.rs
    - src/proxy/webhook/mod.rs
    - src/proxy/webhook/processor.rs
    - src/proxy/webhook/signer.rs
    - src/proxy/webhook/cancel_registry.rs
    - .planning/phases/07-webhook-pipeline/deferred-items.md
  modified:
    - src/models/mod.rs
    - src/models/migration.rs
    - src/handler/api_v1/mod.rs
    - src/proxy/mod.rs
    - src/proxy/server.rs
    - src/app.rs
    - src/proxy/tests/common.rs (Rule 3 blocker fix — fixture additive fields)
    - src/proxy/tests/test_auth.rs (Rule 3 blocker fix — fixture additive fields)
decisions:
  - "D-01 prefix override: REQUIREMENTS.md literal `rustpbx_webhooks` → actual `supersip_webhooks`. The Phase 3 D-00 project-wide convention (all NEW tables get the `supersip_` prefix) takes precedence over the literal in REQUIREMENTS.md. This was explicit in 07-CONTEXT.md D-01 and is documented here so verifiers cross-reference both names."
  - "Channel capacity 1024: matches the locator_webhook precedent. Slow-subscriber lag handling lands in 07-04 (RecvError::Lagged → warn-and-continue)."
  - "AppState plumbing via SipServer.inner (not directly on AppStateInner) — same shape as active_call_registry; avoids a structural change to AppStateInner construction path."
  - "Test fixtures (common.rs, test_auth.rs) explicitly construct stub `webhook_sender` + `webhook_cancel_registry` fields. Rule 3 blocker fix — adding fields to SipServerInner without updating constructors is a compile-stop, not an architectural change."
metrics:
  duration: ~10 minutes (interactive), ~5 minutes wall after compile
  completed: 2026-04-26
---

# Phase 7 Plan 01: Webhook Schema + Scaffolding Summary

**One-liner:** Lays the `supersip_webhooks` schema, broadcast channel, cancel registry, and stub router so Waves 2/3/4 can implement CRUD, signer, and processor on disjoint files without touching cross-cutting wiring.

## What landed

1. **`supersip_webhooks` entity + forward-only migration** (D-01..D-04) with all 11 columns: `id` (PK String 64), `name` (UNIQUE 128), `url` (Text), `secret` (Text), `events` (Json default `[]`), `description` (Text nullable), `is_active` (Bool default true), `retry_count` (Int default 3), `timeout_ms` (Int default 5000), `created_at`/`updated_at` (DateTimeUtc default current_timestamp). Co-located unit tests verify table creation and the UNIQUE name index.

2. **Stub `/api/v1/webhooks` router** with five 501 endpoints (GET/POST list+create, GET/PUT/DELETE by id). Body lands in 07-02; the `pub fn router() -> Router<AppState>` signature is the Wave-1 invariant so 07-02 won't need to touch `api_v1/mod.rs`. Smoke test asserts GET returns 501 with `not_implemented` body.

3. **`src/proxy/webhook/` module directory** with `mod.rs` (declares `WebhookEvent` D-07 shape, `WebhookEventSender` type alias, helper `new_event_id()`/`current_unix_timestamp()`), `signer.rs` (stub `sign()` — body in 07-03), `cancel_registry.rs` (stub `WebhookCancelRegistry` with insert/cancel/remove signatures — body in 07-03), `processor.rs` (stub `run_webhook_processor()` — body in 07-04).

4. **AppState plumbing** — `SipServerInner` gains `webhook_sender: WebhookEventSender` (constructed at boot via `tokio::sync::broadcast::channel(1024)`) and `webhook_cancel_registry: Arc<WebhookCancelRegistry>`. Public accessors on `SipServer` (`webhook_sender()`, `webhook_cancel_registry()`) delegate; `AppStateInner` exposes the same names via further delegation, mirroring how `active_call_registry` works today.

## Threat-model coverage (from PLAN frontmatter)

| Threat | Mitigation in 07-01 |
|--------|---------------------|
| T-07-01-01 (Tampering on schema) | UNIQUE index on `name` enforced at migration time; column types match D-02 exactly. Auth gating is pre-existing Phase 1 middleware on `/api/v1`. |
| T-07-01-02 (Info disclosure on stub) | All 5 stub handlers return a static `not_implemented` body — no DB or runtime data is exposed. |
| T-07-01-03 (DoS on broadcast buffer) | Channel capacity 1024 is locked at boot; matches locator_webhook precedent. Lag handling for slow subscribers lands in 07-04 processor. |
| T-07-01-04 (Privilege via cancel registry) | Registry uses interior mutability through DashMap; `&self` access only; mutation surface is bounded to insert/cancel/remove (bodies in 07-03). No raw `DashMap` handle escapes. |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Test fixtures missing new SipServerInner fields**
- **Found during:** Task 3 verification (`cargo test -p rustpbx --lib proxy::webhook`)
- **Issue:** `src/proxy/tests/common.rs:77` and `src/proxy/tests/test_auth.rs:448` directly construct `SipServerInner { ... }` literal with named fields. Adding `webhook_sender` + `webhook_cancel_registry` to the struct (Task 3, action 6) caused E0063 missing-field errors, blocking the lib-test compile.
- **Fix:** Appended both fields to each fixture, using a 16-slot broadcast channel (sufficient for unit tests) and a fresh `WebhookCancelRegistry::new()`.
- **Files modified:** `src/proxy/tests/common.rs`, `src/proxy/tests/test_auth.rs`
- **Commit:** `c9c9fec`

## Deferred Issues (out of scope)

- **`tests/did_index.rs:13` references `DidIndex::from_map_for_test`** — function does not exist anywhere in the codebase. Pre-existing on parent commit `e0242fd`; not caused by Phase 7. Recorded in `.planning/phases/07-webhook-pipeline/deferred-items.md`. Tracking for a future cleanup pass.

## D-01 prefix override (per plan output spec)

REQUIREMENTS.md literally specifies `rustpbx_webhooks` for the WH-01 storage table. Phase 3 D-00 locked all NEW tables to the `supersip_` prefix; Phase 7 follows that convention rather than the REQUIREMENTS.md literal. Verifiers should cross-reference `supersip_webhooks` (entity table_name) against `rustpbx_webhooks` (REQUIREMENTS.md) and treat them as the same logical artifact.

## Verification

- `cargo check -p rustpbx --lib` — clean
- `cargo test -p rustpbx --lib models::webhooks` — 2 passed
- `cargo test -p rustpbx --lib handler::api_v1::webhooks` — 1 passed
- `cargo test -p rustpbx --lib proxy::webhook` — 2 passed
- Files modified match the 12 declared in PLAN frontmatter (plus 2 test fixtures fixed under Rule 3 + 1 deferred-items log)

## Commits

- `c169cb1` — Task 1: `supersip_webhooks` entity + migration
- `e0242fd` — Task 2: stub router with 501 placeholders
- `c9c9fec` — Task 3: `WebhookEventSender` + `WebhookCancelRegistry` plumbing

## Self-Check: PASSED

- src/models/webhooks.rs — FOUND
- src/handler/api_v1/webhooks.rs — FOUND
- src/proxy/webhook/mod.rs — FOUND
- src/proxy/webhook/processor.rs — FOUND
- src/proxy/webhook/signer.rs — FOUND
- src/proxy/webhook/cancel_registry.rs — FOUND
- Commit c169cb1 — FOUND
- Commit e0242fd — FOUND
- Commit c9c9fec — FOUND
