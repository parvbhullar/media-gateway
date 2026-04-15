---
plan: 01-02
phase: 01-api-shell-cheap-wrappers
completed_at: 2026-04-15
commits:
  - 3d45150  # Gateways write routes + engagement tracking
status: gaps_found
requirements:
  - GWY-01
  - GWY-02
  - GWY-03
  - GWY-04
---

# Plan 01-02 — Gateways Write Routes + Engagement Tracking

> **Retroactive reconciliation** — GSD state lagged behind git history; this
> SUMMARY reconstructs the execution record from commit `3d45150`.

## What was built

- `src/handler/api_v1/gateways.rs` — extended existing router with 3 writes
  (+248 LOC):
  - `CreateGatewayRequest`, `UpdateGatewayRequest` inputs.
  - `create_gateway` (201 + `GatewayView`, 409 on duplicate, 400 on bad body).
  - `update_gateway` (200 merge, 404 missing).
  - `delete_gateway` (204 happy, 409 when engaged, 404 missing).
  - Input validation: `validate_name` rejects empty/whitespace/>128 chars.
  - Engagement tracking: `DidEntity` scanned by `trunk_name`; first
    referencing DID cited in 409 body.
- `tests/api_v1_gateways.rs` — +246 LOC, 11 new test cases (18 total).

## Routes registered

| Method | Path | Handler |
|---|---|---|
| POST   | `/api/v1/gateways`          | `create_gateway` |
| PUT    | `/api/v1/gateways/{name}`   | `update_gateway` |
| DELETE | `/api/v1/gateways/{name}`   | `delete_gateway` |

Evidence: `src/handler/api_v1/gateways.rs:72-80` — method-merged onto the
existing `/gateways` and `/gateways/{name}` routes from Plan 0.

## Verification results

```
cargo test --test api_v1_gateways -> 18 passed / 0 failed
```

Observed test cases include:
- `create_gateway_requires_auth`, `create_gateway_happy_path_returns_201`,
  `create_gateway_duplicate_returns_409`, `create_gateway_empty_name_returns_400`.
- `update_gateway_requires_auth`, `update_gateway_happy_path`,
  `update_gateway_missing_returns_404`.
- `delete_gateway_requires_auth`, `delete_gateway_happy_path_returns_204`,
  `delete_gateway_missing_returns_404`,
  `delete_gateway_with_referencing_did_returns_409`.

## Gaps found

1. **SHELL-05 partial — no `pub(crate)` pure fns in `console/handlers/sip_trunk.rs`.**
   Plan 01-02 promised `create_sip_trunk_row / update_sip_trunk_row /
   delete_sip_trunk_row / count_engagements` as module-level pure fns.
   `grep` returns zero matches. `gateways.rs` operates on `TrunkEntity`
   directly via SeaORM in the api_v1 handler. Same reasoning as Plan 01-01:
   shared sink is the model/entity layer, but the literal artifact promise
   is unmet.

2. **GWY-04 gateway health re-hook — NOT observable in code.**
   Plan 01-02 truth #9: "On create success, gateway health monitoring is
   re-hooked by calling the existing proxy/gateway_health mechanism". No
   `gateway_health` / `register_trunk` / `health_monitor` symbol is called
   from `create_gateway`. The commit message for `3d45150` does not mention
   health-hook wiring. **This is a hard gap** — GWY-04 is not demonstrably
   satisfied by current code.

3. **Routing-side engagement check deferred.** `count_engagements` promised
   to also scan routing records that name the trunk. Only DID scan is
   implemented. The plan itself allowed a `TODO(Phase 6)` for the routing
   side, so this is an in-spec partial, not a violation.

4. **MIG-03 spot-check on `/console/sip_trunks` not documented.**
