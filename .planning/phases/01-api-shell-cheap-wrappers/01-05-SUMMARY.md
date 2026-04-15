---
plan: 01-05
phase: 01-api-shell-cheap-wrappers
completed_at: 2026-04-15
commits:
  - ee6e053  # System health + reload sub-router
status: gaps_found
requirements:
  - SYS-01
  - SYS-02
---

# Plan 01-05 — System Health + Reload

> **Retroactive reconciliation** — GSD state lagged behind git history; this
> SUMMARY reconstructs the execution record from commit `ee6e053` (+316 LOC).

## What was built

- `src/handler/api_v1/system.rs` — new 165-line module with:
  - `HealthResponse` (locked shape: `uptime_secs, db_ok, active_calls, version`).
  - `ReloadResponse` (locked shape: `reloaded, elapsed_ms`).
  - `pub(crate) async fn health_snapshot(&AppState)` —
    `src/handler/api_v1/system.rs:76-115`. Uses `AppStateInner.uptime`,
    a 250ms-timeout `execute_unprepared("SELECT 1")` DB probe,
    `state.sip_server().inner.active_call_registry.count()`, and
    `env!("CARGO_PKG_VERSION")`.
  - `pub(crate) async fn reload_all(&AppState)` —
    `src/handler/api_v1/system.rs:141-165`. Uses CAS on
    `state.reload_requested` + RAII `ReloadGuard` (lines 129-135) with
    `Drop` that resets the flag on panic.
- `src/handler/api_v1/mod.rs` — `pub mod system;` + `.merge(system::router())`.
- `tests/api_v1_system.rs` — 5 integration tests.

## Routes registered

| Method | Path | Handler |
|---|---|---|
| GET  | `/api/v1/system/health`  | `handle_health` |
| POST | `/api/v1/system/reload`  | `handle_reload` |

Evidence: `src/handler/api_v1/system.rs:62-66`.

## Verification results

```
cargo test --test api_v1_system -> 5 passed / 0 failed
```

Cases: `health_requires_auth`, `health_happy_path_shape`, `reload_requires_auth`,
`reload_happy_path_shape`, `reload_twice_sequentially_both_succeed`.

## Gaps found

1. **SYS-02 reload is a no-op stub — CRITICAL.**
   `reload_all` (`src/handler/api_v1/system.rs:141-165`) flips the reload
   guard, records `reloaded = ["trunks","routes","acl","app"]`, measures
   elapsed time — and **does not call any actual reload logic**. The
   module docstring admits this at lines 17-23: *"Phase 1 ships a minimal
   `reload_all` that flips the guard, sleeps briefly, and returns the
   elapsed_ms. Wiring it to the actual AMI reload subsystems (trunks /
   routes / acl / app) is deferred…"* The commit message for `ee6e053`
   repeats: *"does not yet wire into the real AMI reload subsystems. A
   later plan replaces the stub body with real calls"*.

   Plan 01-05 required (truth #8): *"runs the 4 reload steps (trunks,
   routes, acl, app) sequentially"* and (truth #11): *"If any individual
   reload step errors, the whole endpoint returns 500"*. **Neither is
   observable** — there are no individual `reload_trunks / reload_routes /
   reload_acl / reload_app` pure fns, no `ReloadStepError` enum, no
   possibility of a step failing. The on-wire response is a stable lie.

2. **Missing concurrent-reload race test.** Plan required a
   `reload_concurrent_returns_one_200_one_409` test. The shipped file has
   `reload_twice_sequentially_both_succeed` which only proves guard
   release — it does **NOT** exercise the concurrent CAS path. The
   conflict / 409 branch of `reload_all` is therefore unobserved in test.

3. **AMI `/ami/v1/reload/*` endpoints not refactored.** Plan required
   extracting per-step pure fns (`reload_trunks / reload_routes / ...`) in
   `handler/ami.rs` and re-wiring the legacy AMI handlers to call them.
   Not done — `grep` confirms no `reload_trunks` symbol, no `ReloadStepError`
   type in `ami.rs`. Legacy AMI endpoints still inline their logic.

These three items together mean SYS-02 is **functionally unimplemented** —
the endpoint is a shape-compliant stub. Recommend a follow-up plan in
Phase 1.1 or as part of Phase 11 (System Polish) to promote `reload_all`
to real behavior and add the concurrent-race test.
