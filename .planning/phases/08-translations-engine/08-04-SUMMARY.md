---
phase: 08-translations-engine
plan: 04
subsystem: translations
tags: [trn-04, trn-05, trn-06, hot-path, integration-tests, phase-8-signoff]
requires: [08-01, 08-02, 08-03]
provides:
  - translation_engine_call_site
  - it_trn_06_integration_tests
affects:
  - src/proxy/call.rs
  - tests/proxy_translation_engine.rs
tech_stack:
  added: []
  patterns:
    - "Pre-routing translation pass per Phase 8 D-11/D-14"
    - "Optional engine + db on DefaultRouteInvite — engine is no-op when no active rules"
key_files:
  created:
    - tests/proxy_translation_engine.rs
    - .planning/phases/08-translations-engine/deferred-items.md
  modified:
    - src/proxy/call.rs
key_decisions:
  - "Engine wired via Option<Arc<TranslationEngine>> + Option<DatabaseConnection> fields on DefaultRouteInvite — keeps test fixtures and custom RouteInvite factories unaffected"
  - "Engine error logged at warn and call proceeds with un-translated values (T-08-04-01)"
  - "Insertion lives in route_invite only (real dispatch path); preview_route is left untouched"
  - "DialDirection threaded into the engine is the existing &DialDirection arg on the route_invite signature, dereferenced once via *direction"
metrics:
  tasks_completed: 3
  files_created: 2
  files_modified: 1
  duration_minutes: ~10
  completed_date: 2026-04-26
---

# Phase 8 Plan 08-04: Translation Engine Call-Site Wire-Up + IT-TRN-06 Summary

Wave-3 (FINAL) closure of Phase 8: hot-path insertion of `TranslationEngine::translate` immediately before `match_invite_with_codecs` (the matcher façade that internally invokes `match_invite_with_trace`), plus IT-TRN-06 integration tests covering D-29 cases 1-5.

## What Landed

### Task 1: `tests/proxy_translation_engine.rs` (5 tests, all GREEN)

| # | Test                                                             | D-29 case |
| - | ---------------------------------------------------------------- | --------- |
| 1 | `it_trn_06_uk_normalize_inbound`                                 | #1 (TRN-04) |
| 2 | `it_trn_06_us_normalize_inbound`                                 | #2 (TRN-04) |
| 3 | `it_trn_06_direction_filter_outbound_call_inbound_rule`          | #3 (TRN-05) |
| 4 | `it_trn_06_cascade_priority_order`                               | #4 |
| 5 | `it_trn_06_per_field_independence`                               | #5 |

Pattern: each test spins up `sqlite::memory:`, runs the translations migration (`TestMigrator` wraps `models::translations::Migration`), seeds rule rows via the public `ActiveModel`, then calls `engine.translate(&mut invite_option, direction, &db)` and asserts both the mutated `InviteOption` URI bytes and the returned `TranslationTrace` shape. Mirrors the in-tree `#[cfg(test)] mod tests` in `src/proxy/translation/engine.rs` but exercises the engine through the crate's public API surface from a separate test binary.

### Task 2: `src/proxy/call.rs` insertion (12 lines, additive only)

```diff
@@ pub struct DefaultRouteInvite {
     pub source_trunk_hint: Option<String>,
+    /// Phase 8 D-11: pre-routing translation engine + DB handle for D-13 fresh read.
+    pub translation_engine: Option<Arc<crate::proxy::translation::TranslationEngine>>,
+    pub db: Option<sea_orm::DatabaseConnection>,
 }

@@ async fn route_invite(...) -> Result<RouteResult> {
+        let mut option = option;
+        // Phase 8 D-11: pre-routing translation pass. TODO(observability): attach trace to CDR.
+        if let (Some(engine), Some(db)) = (self.translation_engine.as_ref(), self.db.as_ref()) {
+            if let Err(e) = engine.translate(&mut option, *direction, db).await {
+                warn!(error = %e, "translation engine failed; continuing without translation");
+            }
+        }
         let (trunks_snapshot, routes_snapshot, source_trunk) =

@@ Box::new(DefaultRouteInvite {
                     ...
                     source_trunk_hint,
+                    translation_engine: Some(self.inner.server.translation_engine.clone()),
+                    db: self.inner.server.database.clone(),
                 })
```

**Bound check confirmed:** `git diff --stat src/proxy/call.rs` reports `1 file changed, 12 insertions(+)`. Acceptance criterion (`<= 12 lines`) **satisfied exactly**.

**DialDirection derivation source:** the existing `direction: &DialDirection` parameter on `RouteInvite::route_invite`; passed to the engine as `*direction` (DialDirection is `Copy`). No new state field, no new threading.

**Engine + DB handles:** populated at the single `DefaultRouteInvite` construction site inside `CallModule` (call.rs:1106) from the already-public `SipServerInner.translation_engine` and `SipServerInner.database` fields plumbed in 08-01. Both fields on `DefaultRouteInvite` are `Option<>` so test fixtures and custom `create_route_invite` factories that don't go through `CallModule` continue to work without recompiling.

**Why `route_invite` only, not `preview_route`:** D-11 specifies the live INVITE pipeline — `route_invite` is the dispatch path; `preview_route` is the dry-run preview used by `/api/v1/routing/resolve`. Keeping the preview translation-free preserves preview semantics (operators can see the un-translated routing decision).

### Task 3: human-verify checkpoint

Hot-path insertion is bounded (12 lines), inserts a single `engine.translate` call before the matcher, never mutates control flow on engine error, and is no-op when no active rules exist in DB.

## Locked Behavior Summary (Phase 8 Sign-Off)

| Layer            | Where                                                          | Behavior                                                                       |
| ---------------- | -------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Schema           | `src/models/translations.rs` (08-01)                           | `supersip_translations` entity + forward-only migration; UNIQUE(name)         |
| AppState         | `src/proxy/server.rs` + `src/app.rs` (08-01)                   | `Arc<TranslationEngine>` constructed at boot; `state.translation_engine()`    |
| CRUD             | `src/handler/api_v1/translations.rs` (08-02)                   | `/api/v1/translations` POST/GET/PUT/DELETE; `validate_translation` + `engine.invalidate()` on writes |
| Engine           | `src/proxy/translation/engine.rs` (08-03)                      | Priority-ASC cascade + direction filter + DashMap regex cache (`{id}::caller`/`{id}::destination`) |
| Pipeline         | `src/proxy/call.rs::route_invite` (08-04)                      | `engine.translate(&mut option, *direction, db)` before matcher; warn-and-continue on error |
| Integration test | `tests/proxy_translation_engine.rs` (08-04)                    | 5/5 D-29 cases green                                                          |

## File-Ownership Invariant Held

This plan modified ONLY:
- `src/proxy/call.rs` (12-line additive insertion + 2 fields on DefaultRouteInvite)
- `tests/proxy_translation_engine.rs` (NEW, 289 lines)

UNTOUCHED (frozen by Wave 1+2):
- `src/handler/api_v1/mod.rs`
- `src/models/migration.rs`
- `src/models/mod.rs`
- `src/proxy/server.rs`
- `src/app.rs`
- `src/proxy/translation/engine.rs`

Verified via `git diff --name-only HEAD~2 HEAD` → only the two allowed files.

## Verification Snapshot

| Check                                                                                        | Result                       |
| -------------------------------------------------------------------------------------------- | ---------------------------- |
| `cargo check -p rustpbx --lib`                                                               | PASS                         |
| `cargo check -p rustpbx --release`                                                           | PASS (2m 06s)                |
| `cargo test -p rustpbx --test proxy_translation_engine`                                      | 5/5 PASS                     |
| `cargo test -p rustpbx --test api_v1_translations` (08-02 IT-01 regression)                  | 18/18 PASS                   |
| `cargo test -p rustpbx --lib proxy::translation` (08-03 unit-test regression)                | 16/16 PASS                   |
| `cargo test -p rustpbx --lib` (full lib regression)                                          | 1297 PASS / 11 FAIL (pre-existing RTP e2e flakes — see `deferred-items.md`; verified by `git stash` baseline run that the same 11 fail without Phase 8 changes) |
| `grep -c "engine\.translate" src/proxy/call.rs`                                              | 1                            |
| `git diff --stat src/proxy/call.rs`                                                          | `+12` insertions             |
| `git diff --name-only HEAD~2 HEAD` only contains allowed files                               | `src/proxy/call.rs`, `tests/proxy_translation_engine.rs` |
| Forbidden Wave-1+2 files in diff                                                             | NONE                         |

## Phase 8 Requirement Closure (TRN-01..06)

| ID     | Requirement                                                                                          | Closure Citation |
| ------ | ---------------------------------------------------------------------------------------------------- | ---------------- |
| TRN-01 | New `rustpbx_translations` (named `supersip_translations`) table + entity exists                     | `src/models/translations.rs` (08-01-SUMMARY §1) |
| TRN-02 | Operator CRUD via `/api/v1/translations` with caller/destination patterns, replacements, direction   | `src/handler/api_v1/translations.rs` + `tests/api_v1_translations.rs` 18 tests (08-02-SUMMARY) |
| TRN-03 | `proxy/translation/engine.rs` compiles and caches regex rules keyed on rule id                       | `src/proxy/translation/engine.rs:36-180` (08-03-SUMMARY §2) |
| TRN-04 | Inbound call pipeline applies matching translation rules BEFORE routing                              | `src/proxy/call.rs:265-272` invokes engine before `build_context` + `match_invite_with_codecs` (08-04 Task 2) |
| TRN-05 | Direction filter — inbound-only rules do not fire on outbound legs                                   | `tests/proxy_translation_engine.rs::it_trn_06_direction_filter_outbound_call_inbound_rule` + engine `direction_matches` at `src/proxy/translation/engine.rs:190-197` |
| TRN-06 | Integration test exercises `02079460123 → +442079460123` and `4155551234 → +14155551234`             | `tests/proxy_translation_engine.rs::it_trn_06_uk_normalize_inbound` + `it_trn_06_us_normalize_inbound` |

## Deviations from Plan

### Auto-fixed Issues

None. The plan executed as written.

### Documented Decisions

**1. Engine + DB plumbed via `Option<>` fields on `DefaultRouteInvite`, not via a `state` ref**

- **Why:** `DefaultRouteInvite` doesn't carry a SipServerRef and the trait `RouteInvite` doesn't either. Adding the engine and DB as struct fields is the smallest plumbing change; the construction site (single call site at `CallModule::handle_invite`) already has full access to `self.inner.server.translation_engine` and `self.inner.server.database`. Keeping them `Option<>` means custom `create_route_invite` factories and any third-party `RouteInvite` impls (or test fixtures) keep compiling without changes.

**2. Insertion compressed from initial 21-line shape to 12-line shape**

- **Why:** The first cut used a verbose `unwrap_or_else(|e| { ...; TranslationTrace::default() })` pattern (matching the plan's `<interfaces>` example exactly). Acceptance criterion required `<= 12 lines`, so the trace binding was dropped (we don't persist it in v2.0 — TODO comment retained) and the warn-on-error flattened to a single-line `if let Err(...)`. Behaviorally identical: engine error → warn log → call proceeds without translation.

**3. `preview_route` not modified**

- **Why:** D-11 specifies the live dispatch pipeline. `preview_route` is dry-run for `/api/v1/routing/resolve` — preview semantics expect the un-translated routing decision so operators can debug raw routing rules. Phase 8 introduces no requirement to translate during preview.

## Auth Gates

None.

## Threat Flags

None — Phase 8's threat surface (operator-controlled regex rewrites at the trust boundary) is fully covered by the 08-CONTEXT.md threat register and was already mitigated by:
- Write-time validation (08-02 `validate_translation`: regex compile, length cap 4096, replacement well-formedness).
- Defense-in-depth at engine compile (08-03 `get_or_compile`: warn-and-skip on bad regex).
- Engine error → warn-and-continue (08-04 Task 2: T-08-04-01 mitigation).

## Pre-existing Issues Documented (Out of Scope)

11 RTP/media e2e tests fail on the `sip_fix` baseline both with and without Phase 8 changes. Root cause is media-layer timing, not routing or translation. Tracked in `deferred-items.md` for Phase 12 hardening.

## Commits (in order)

| Task | Commit  | Description                                                  |
| ---- | ------- | ------------------------------------------------------------ |
| 1    | 7d98e3a | test(08-04): add IT-TRN-06 integration tests for translation engine |
| 2    | 2f6945f | feat(08-04): wire TranslationEngine into proxy hot path before matcher |

## Self-Check: PASSED

- File `tests/proxy_translation_engine.rs` — FOUND
- File `src/proxy/call.rs` (modified) — FOUND
- File `.planning/phases/08-translations-engine/deferred-items.md` — FOUND
- Commit 7d98e3a — FOUND in `git log`
- Commit 2f6945f — FOUND in `git log`
- 5 IT-TRN-06 tests — all PASS
- 18 IT-01 tests — still all PASS (no regression)
- 16 engine unit tests — still all PASS (no regression)
- 12-line bound on src/proxy/call.rs — verified by `git diff --stat`
- File ownership — verified by `git diff --name-only HEAD~2 HEAD`
