---
phase: 07-webhook-pipeline
plan: 03
subsystem: webhook-pipeline
tags: [webhooks, hmac, security, cancellation, phase7]
requirements: [WH-03, WH-06]
dependency_graph:
  requires: [07-01]
  provides:
    - "signer::sign — HMAC-SHA256 hex digest for X-Webhook-Signature header"
    - "signer::signature_header — full Stripe-format header value"
    - "WebhookCancelRegistry::insert/cancel/remove/contains_key with D-31 + D-34 semantics"
  affects: []
tech-stack:
  added: []
  patterns:
    - "Stripe-style HMAC-SHA256 with canonical payload {timestamp}.{body}"
    - "DashMap-backed CancellationToken registry with PUT-replace cancel-prior-then-insert ordering"
key-files:
  created: []
  modified:
    - src/proxy/webhook/signer.rs
    - src/proxy/webhook/cancel_registry.rs
decisions:
  - "Use hmac 0.13 KeyInit trait import (not new_from_slice on Hmac alias alone) — required by 0.13 API"
  - "Expose signature_header convenience helper alongside sign so 07-04 doesn't reinvent the t=,v1= concat"
  - "PUT-replace ordering: remove(prior) -> cancel(prior) -> insert(new) so the registry is never in a state where the prior token is uncancelled but unreachable"
metrics:
  duration: "~5 min"
  completed: 2026-04-26
  tasks: 2
  tests_added: 16
---

# Phase 7 Plan 03: Webhook Signer + Cancel Registry Summary

HMAC-SHA256 Stripe-style signer and DashMap-backed cancel registry filled into the 07-01 stubs. Pure-logic, no I/O, fully unit-tested.

## What landed

- `src/proxy/webhook/signer.rs` — `sign(timestamp, body, secret) -> String` returns 64-char lowercase hex HMAC-SHA256 over `format!("{}.{}", timestamp, body)` (D-14). `signature_header()` helper returns `t=<ts>,v1=<hex>` per D-15.
- `src/proxy/webhook/cancel_registry.rs` — `WebhookCancelRegistry` with `insert / cancel / remove / contains_key`. PUT-replace (D-34) cancels the prior token before storing the new one. `cancel` (D-31) cancels and removes; `remove` removes without cancelling (post-success cleanup).

## OpenSSL Reference Vectors (audit re-derivation)

Embedded in `signer.rs` tests. Re-derive any of these with:

```bash
printf '%s' '<canonical-payload>' | openssl dgst -sha256 -hmac '<secret>' -hex
```

| Test | timestamp | body | secret | expected hex |
|------|-----------|------|--------|--------------|
| `empty_body_and_empty_secret_matches_openssl` | 0 | "" | "" | `b849d5a581847b281957065739df36df2463d1977ea8d6e1e4e6cf33fadc68c3` |
| `ascii_body_matches_openssl_reference` | 1714060800 | `{"a":1}` | `secret` | `7aec304c817a19e63b9237165bbe9a1fd90c3d57a902d0982f9d8269804f0ff8` |
| `multibyte_utf8_body_matches_openssl_reference` | 1714060800 | `héllo` | `secret` | `98b3185b366f18f63455f9fe231f6f7c655c33aeacd7b5ad266e40a84e6f6345` |
| `doc_reference_vector_matches_openssl` | 1234567890 | `{"event":"webhook.test"}` | `my-secret` | `60ac7756312e13849495558ecfc3d8d1a40c18fce095adaf12979dca9bff99c5` |

The canonical payload concatenated with openssl is `{timestamp}.{body}` (e.g. `1714060800.{"a":1}`).

## D-34 PUT-Replace Semantics (confirmed)

`insert(id)` flow when an entry already exists:

1. `self.inner.remove(id)` — atomically extracts the prior `(_, prior_token)` from DashMap (no concurrent observer can see both old and new tokens simultaneously).
2. `prior.cancel()` — fires the prior token so any in-flight retry loop selecting on it bails out at the next await point.
3. `self.inner.insert(id, new_token)` — stores the fresh token.
4. Returns a clone of the new token to the caller (07-04 processor's retry loop).

Test `put_replace_cancels_prior_and_returns_fresh_token` confirms `t1.is_cancelled() == true && t2.is_cancelled() == false && inner.len() == 1` after two consecutive `insert("wh-1")` calls. The `contains_key_reflects_lifecycle` test exercises insert -> cancel -> insert -> remove transitions.

## Test Counts

- `signer`: 9 unit tests (4 reference vectors + 5 property/structural).
- `cancel_registry`: 6 sync unit tests + 1 `#[tokio::test]` concurrent insert smoke test (100 ids via `JoinSet`).

All 16 pass via `cargo test -p rustpbx --lib proxy::webhook::{signer,cancel_registry}`.

## Verification

- `cargo check -p rustpbx --lib` — clean.
- `cargo test -p rustpbx --lib proxy::webhook::signer` — 9 passed.
- `cargo test -p rustpbx --lib proxy::webhook::cancel_registry` — 7 passed.
- `git diff --name-only HEAD -- src/` shows ONLY the 2 declared files; zero diff against `src/handler/api_v1/mod.rs`, `src/models/migration.rs`, `src/proxy/server.rs`, `src/app.rs`.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Missing `KeyInit` trait import for `hmac` 0.13**
- **Found during:** Task 1 verification (`cargo check`)
- **Issue:** `hmac` 0.13 moved `new_from_slice` behind the `KeyInit` trait; importing only `{Hmac, Mac}` produced `error[E0599]: no function or associated item named new_from_slice found for struct Hmac`.
- **Fix:** Added `KeyInit` to the import: `use hmac::{Hmac, KeyInit, Mac};`.
- **Files modified:** `src/proxy/webhook/signer.rs`
- **Commit:** included in plan-level commit (atomic per plan instructions).

No architectural deviations. No CLAUDE.md-driven adjustments (Rust project; the global Python guidelines do not apply at the toolchain level here).

## Self-Check: PASSED

- `src/proxy/webhook/signer.rs` exists and contains `pub fn sign`, `pub fn signature_header`, and 9 `#[test]` attributes — verified.
- `src/proxy/webhook/cancel_registry.rs` exists and contains `insert`/`cancel`/`remove`/`contains_key` plus 7 test attributes (`#[test]` x6 + `#[tokio::test]` x1) — verified.
- `unimplemented!` is gone from both files.
- No diff against the four Wave-1-frozen files.
