---
plan: 01-04
phase: 01-api-shell-cheap-wrappers
completed_at: 2026-04-15
commits:
  - 6f8c594  # Diagnostics sub-router + summary aggregator
status: gaps_found
requirements:
  - DIAG-01
  - DIAG-02
  - DIAG-03
  - DIAG-04
  - DIAG-05
---

# Plan 01-04 — Diagnostics Sub-Router

> **Retroactive reconciliation** — GSD state lagged behind git history; this
> SUMMARY reconstructs the execution record from commit `6f8c594` (+594 LOC).

## What was built

- `src/handler/api_v1/diagnostics.rs` — new 243-line sub-router with
  request/response view types and 4 handlers.
- `src/handler/api_v1/mod.rs` — `pub mod diagnostics;` +
  `.merge(diagnostics::router())`.
- `tests/api_v1_diagnostics.rs` — 12 integration tests.

## Routes registered

| Method | Path | Handler |
|---|---|---|
| POST | `/api/v1/diagnostics/route-evaluate`            | `route_evaluate` |
| GET  | `/api/v1/diagnostics/registrations`             | `list_registrations` |
| GET  | `/api/v1/diagnostics/registrations/{user}`      | `get_registration` |
| GET  | `/api/v1/diagnostics/summary`                   | `diagnostics_summary` |

Evidence: `src/handler/api_v1/diagnostics.rs:92-99`.

DIAG-04 `/diagnostics/trunk-test` lives in `api_v1/gateways.rs:79` (Plan 0
inheritance) and is covered by `trunk_test_*` cases in `api_v1_gateways`.

## Verification results

```
cargo test --test api_v1_diagnostics -> 12 passed / 0 failed
```

Observed cases: `route_evaluate` happy-path match + no-match + empty fields
400 + invalid direction 400 + direction filter; `registrations` empty list;
`get_registration` 404 missing; `summary` shape + routing count correctness;
4x 401-without-auth.

## Gaps found

1. **SHELL-05 partial — no pure fns in `console/handlers/diagnostics.rs`.**
   Plan promised `route_evaluate / list_registrations / get_registration /
   locator_snapshot`. `grep` confirms no `pub(crate)` helpers added.

2. **DIAG-05 summary does not expose `recent_flood_events` /
   `recent_auth_failures`.** Commit message admits: "Summary aggregates
   routing row counts (active vs inactive) and registration counts. No
   per-component failure masking needed since the routing count is the
   only DB-backed slot right now." The CONTEXT.md-locked summary shape was
   `{registrations: {count, users}, locator: {active_aors},
   recent_flood_events, recent_auth_failures}`. The flood/auth-failure
   slots are absent — they will need to land in Phase 10 (Security Suite)
   where flood/brute-force tracking actually exists. This is a
   **contract drift** on DIAG-05.

3. **Registrar / locator live-state accessors not exercised.** The
   `list_registrations` handler is tested with an empty registrar only —
   no fixture seeds a registration, so happy-path data flow through
   `proxy/registrar.rs` is unobserved.

4. **MIG-03 spot-check on `/console/diagnostics` not documented.**
