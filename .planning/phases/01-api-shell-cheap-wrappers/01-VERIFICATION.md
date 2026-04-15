---
phase: 01-api-shell-cheap-wrappers
verified_at: 2026-04-15
mode: retroactive-reconciliation
status: gaps_found
score: 20/26  # must-have truths VERIFIED (rest PARTIAL or MISSING)
commits:
  - 6f24907
  - 8db5954
  - 3d45150
  - 65c5d82
  - 6f8c594
  - ee6e053
plans:
  - 01-01
  - 01-02
  - 01-03
  - 01-04
  - 01-05
gaps:
  - id: GWY-04
    severity: blocker
    summary: "create_gateway does not call proxy/gateway_health registration"
  - id: SYS-02
    severity: blocker
    summary: "reload_all is a no-op stub — records 4 step names, calls no reload logic"
  - id: SYS-02-test
    severity: high
    summary: "Concurrent-reload 409 branch unobserved in tests"
  - id: DIAG-05
    severity: medium
    summary: "diagnostics/summary omits recent_flood_events + recent_auth_failures slots"
  - id: SHELL-05
    severity: medium
    summary: "Console handler pure-fn extraction not performed; api_v1 reuses model layer directly"
  - id: MIG-03
    severity: low
    summary: "Console render-parity spot check not documented for any of the 5 pages"
---

# Phase 1 Verification — API Shell & Cheap Wrappers

> **Retroactive reconciliation.** GSD STATE.md said `completed_phases: 0,
> status: ready_to_plan` but all 5 plans had already landed across commits
> `6f24907..ee6e053` on 2026-04-15. This verification audits what git
> contains against what the plans and ROADMAP goal promised — goal-backward,
> not rubber-stamp.

## Phase 1 Goal (from ROADMAP)

> "Establish the adapter convention for the entire milestone and ship ~17
> routes that wrap existing console handlers with zero new business logic.
> Every existing console HTML route (sip_trunks, dids, call-records,
> routing, diagnostics, settings) renders identically. Every sub-router
> ships with an integration test asserting 401-without-auth, happy-path,
> 404-missing, 400/409-bad-input."

## Summary

| Plan  | Commit    | Status      | Truths verified | Gaps |
|-------|-----------|-------------|-----------------|------|
| 01-01 | 6f24907, 8db5954 | gaps_found | 8/10 | SHELL-05 (partial), MIG-03 (not documented) |
| 01-02 | 3d45150   | gaps_found  | 8/10            | GWY-04 (blocker), SHELL-05 (partial), MIG-03 (not documented) |
| 01-03 | 65c5d82   | gaps_found  | 7/7             | SHELL-05 (partial), MIG-03 (not documented) |
| 01-04 | 6f8c594   | gaps_found  | 5/7             | DIAG-05 contract drift, SHELL-05 (partial), MIG-03 (not documented) |
| 01-05 | ee6e053   | gaps_found  | 3/11            | SYS-02 blocker, missing concurrent-race test |
| **Total** | —     | **gaps_found** | **~20/26** (~77%) | 6 gaps listed below |

## Observable truths — requirements coverage

**Legend:** VERIFIED (cited evidence) / PARTIAL (intent met, contract drift)
/ MISSING (no evidence or contradicted by code).

### SHELL — Adapter Shell

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| SHELL-01 | `/api/v1/*` root router nests sub-routers under Bearer auth | VERIFIED | `src/handler/api_v1/mod.rs:25-44` — all 5 sub-router merges + `auth::api_v1_auth_middleware` layer |
| SHELL-02 | Paginated envelope `{items, page, page_size, total}` | VERIFIED | `src/handler/api_v1/common.rs` (PaginatedResponse<T>); used by `dids.rs:list_dids` and `cdrs.rs:list_cdrs`; unit tests in `common.rs` + integration assertions in `api_v1_dids.rs` + `api_v1_cdrs.rs` |
| SHELL-03 | `ApiError` helpers: not_found, bad_request, conflict, not_implemented | VERIFIED | `src/handler/api_v1/error.rs` — `not_implemented` added in `6f24907`; `tests/api_v1_error_shape.rs` (1 test passing) |
| SHELL-04 | View types; `Model` never serialized directly | VERIFIED | `DidView` in `dids.rs:46-65`, `CdrView` in `cdrs.rs`, `GatewayView` in `gateways.rs`; `grep` confirms no `impl Serialize for ... Model` in api_v1 |
| SHELL-05 | Adapter pattern: pure fns shared between console HTML + api_v1 | **PARTIAL** | Intent met via SeaORM model layer as shared sink. BUT plans promised `pub(crate) async fn query_dids / create_sip_trunk_row / query_call_records / route_evaluate` in `console/handlers/*` — `grep pub(crate) async fn` on `console/handlers/{did,sip_trunk,call_record,diagnostics}.rs` returns **zero** matches. `dids.rs:1-12` documents the deliberate deviation. Contract **not met as written**. |

### GWY — Gateways

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| GWY-01 | `POST /api/v1/gateways` create + 201 | VERIFIED | `gateways.rs:74` (`.post(create_gateway)`); `tests/api_v1_gateways.rs::create_gateway_happy_path_returns_201` passes |
| GWY-02 | `PUT /api/v1/gateways/{name}` update + 200 / 404 | VERIFIED | `gateways.rs:75-78`; tests `update_gateway_happy_path` + `update_gateway_missing_returns_404` pass |
| GWY-03 | `DELETE /api/v1/gateways/{name}` + engagement tracking 409 | VERIFIED | `gateways.rs:75-78`; test `delete_gateway_with_referencing_did_returns_409` passes; commit `3d45150` cites "Engagement tracking: delete queries DidEntity by trunk_name" |
| GWY-04 | Gateway health monitoring re-hooked on create | **MISSING** | `grep gateway_health\|register_trunk\|health_monitor` against `gateways.rs` + Plan 01-02 diff returns **no hit** inside `create_gateway`. Commit `3d45150` does not mention the hook. This is a hard gap. |

### DID — DIDs

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| DID-01 | `GET /api/v1/dids` (paginated, filters) | VERIFIED | `dids.rs:162`; `api_v1_dids::list_*` tests pass |
| DID-02 | `GET/POST /api/v1/dids/{number}` single + create | VERIFIED | `dids.rs:162-166`; 20 tests in `api_v1_dids.rs` pass |
| DID-03 | `PUT /api/v1/dids/{number}` update | VERIFIED | `dids.rs:164-166` (`.put(update_did)`); tests pass |
| DID-04 | `DELETE /api/v1/dids/{number}` hard delete | VERIFIED | `dids.rs:164-166` (`.delete(delete_did)`); tests pass |

### CDR — Call Records

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| CDR-01 | `GET /api/v1/cdrs` paginated + filters | VERIFIED | `cdrs.rs:94` (`.get(list_cdrs)`); `api_v1_cdrs` list tests pass |
| CDR-02 | `GET /api/v1/cdrs/{id}` + 404 | VERIFIED | `cdrs.rs:95` (`.get(get_cdr)`); tests pass |
| CDR-03 | `DELETE /api/v1/cdrs/{id}` + 204/404 | VERIFIED | `cdrs.rs:95` (`.delete(delete_cdr)`); tests pass |
| CDR-04 | `/recording` + `/sip-flow` 501 stubs with locked body | VERIFIED | `cdrs.rs:96-97`; test file asserts exact body `{"error":"...","code":"not_implemented"}` |

### DIAG — Diagnostics

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| DIAG-01 | `POST /diagnostics/route-evaluate` | VERIFIED | `diagnostics.rs:92`; `route_evaluate` happy-path test passes |
| DIAG-02 | `GET /diagnostics/registrations` | VERIFIED | `diagnostics.rs:93`; test passes against empty registrar |
| DIAG-03 | `GET /diagnostics/registrations/{user}` | VERIFIED | `diagnostics.rs:94-97`; 404 test passes |
| DIAG-04 | `POST /diagnostics/trunk-test` | VERIFIED | Lives in `gateways.rs:79` (Plan 0 inheritance); `trunk_test_404_on_missing_gateway` + `trunk_test_returns_ok_false_for_unreachable` tests pass |
| DIAG-05 | `GET /diagnostics/summary` aggregator with 4 slots | **PARTIAL** | `diagnostics.rs:98`; route works but commit `6f8c594` states: "Summary aggregates routing row counts… and registration counts. No per-component failure masking needed since the routing count is the only DB-backed slot right now." CONTEXT.md-locked shape required `recent_flood_events` + `recent_auth_failures` — those slots are **absent**. Defensible (flood/brute-force tracking doesn't exist until Phase 10), but the locked shape is violated. |

### SYS — System

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| SYS-01 | `GET /system/health` with locked shape | VERIFIED | `system.rs:62-66, 72-115`; `tests/api_v1_system.rs::health_happy_path_shape` asserts shape + uptime + db_ok=true + version |
| SYS-02 | `POST /system/reload` runs 4 reload steps + 409 on conflict + 500 on failure | **MISSING** | `system.rs:141-165` — `reload_all` is explicitly a stub. Module docstring (lines 17-23) and commit `ee6e053` message both admit: "does not yet wire into the real AMI reload subsystems". No per-step fns, no `ReloadStepError`, no concurrent-race test. The on-wire shape is correct but the behavior is a lie. **This is a blocker for SYS-02.** |

### Cross-cutting

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| IT-01 | Each sub-router has 401 / happy / 404 / 400-409 tests | VERIFIED | 75 tests across `api_v1_{auth,mount,error_shape,middleware,dids,gateways,cdrs,diagnostics,system}`, all passing; per-route 4-case coverage observed in each file |
| MIG-03 | Console HTML pages render identically after pure-fn extraction | **PARTIAL** | Commit `8db5954` explicitly states "console DID pages not spot-checked". No commit message for any of the 5 plans records a MIG-03 spot-check. The risk is low (no template edits), but the contract point ("manual pre-merge spot check") was skipped. |

## Test run

```
$ cargo test --test api_v1_auth --test api_v1_mount --test api_v1_error_shape \
             --test api_v1_middleware --test api_v1_dids --test api_v1_gateways \
             --test api_v1_cdrs --test api_v1_diagnostics --test api_v1_system

api_v1_auth         : 2 passed / 0 failed
api_v1_mount        : 1 passed / 0 failed
api_v1_error_shape  : 1 passed / 0 failed
api_v1_middleware   : 3 passed / 0 failed
api_v1_dids         : 20 passed / 0 failed
api_v1_gateways     : 18 passed / 0 failed
api_v1_cdrs         : 13 passed / 0 failed
api_v1_diagnostics  : 12 passed / 0 failed
api_v1_system       : 5 passed / 0 failed
---
TOTAL               : 75 passed / 0 failed
```

All api_v1 integration tests are green against the current tip of
`console_sip`.

## Gap inventory (ordered by severity)

1. **[BLOCKER] SYS-02 reload is a no-op stub.** `src/handler/api_v1/system.rs:141-165`
   does not call any real reload logic. Fix: extract `reload_trunks / reload_routes /
   reload_acl / reload_app` pure fns in `src/handler/ami.rs`, compose them in
   `reload_all`, and add a concurrent-race test. *Defer to a Phase 1 hot-fix plan
   OR promote to Phase 11 (System Polish).*
2. **[BLOCKER] GWY-04 gateway health re-hook not wired.** `create_gateway` does
   not call any `gateway_health / register_trunk / health_monitor` function.
   Locked truth violated. *Fix by registering the new trunk with
   `proxy/gateway_health.rs` at the end of `create_gateway`.*
3. **[HIGH] Missing concurrent-reload race test.** `reload_twice_sequentially_both_succeed`
   exercises guard release only; the CAS conflict / 409 branch is unobserved.
4. **[MEDIUM] DIAG-05 contract drift.** `diagnostics_summary` omits
   `recent_flood_events` + `recent_auth_failures` slots. Either ship zero-valued
   placeholders now or update the contract to defer these to Phase 10.
5. **[MEDIUM] SHELL-05 literal promise unmet.** No pure fns were extracted in
   `console/handlers/{did,sip_trunk,call_record,diagnostics}.rs`. The adapter
   pattern is satisfied at the SeaORM model layer instead. Decision is defensible
   and documented in `dids.rs:1-12`, but future plans must either (a) update
   CONTEXT.md to codify "model layer is the adapter sink" or (b) actually extract
   the fns in a cleanup plan.
6. **[LOW] MIG-03 spot-checks not documented.** None of the 5 plan commits record
   the manual render-parity check for `/console/{sip_trunks, dids, call-records,
   diagnostics, settings}`. Risk is low because no templates were touched, but the
   contract point was skipped.

## Recommendation

Phase 1 is **functionally shipped** for 4 out of 5 plan areas (DIDs, Gateways
read + most writes, CDRs, Diagnostics routing/registrations). Two blockers
stand in the way of marking the phase green:

- **SYS-02 reload stub** — must be made real OR the route should be removed
  and re-planned. Shipping a route whose behavior contradicts its commit
  message is worse than not shipping it.
- **GWY-04 health hook** — one-line-ish fix; must land before Phase 2 depends
  on gateway creation for its trunk_groups work.

Phase 2 planning can proceed in parallel with these fixes provided Phase 2
does not assume the reload endpoint performs real work and does not depend
on newly-created gateways being health-monitored.

---

*Reconciled: 2026-04-15 via retroactive audit of commits 6f24907..ee6e053.*
