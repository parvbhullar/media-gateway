# Phase 1: API Shell & Cheap Wrappers

## Goal

Establish the adapter convention for the entire milestone and ship ~17 routes that wrap existing console handlers with zero new business logic.

## Dependencies

None (first phase).

## Requirements

- **SHELL-01**: `/api/v1/*` sub-router loading pattern supports one file per group, merged into the existing Bearer-authenticated root router
- **SHELL-02**: A shared `Pagination` extractor (`page`, `page_size`) and `PaginatedResponse<T>` envelope are usable from every api_v1 handler
- **SHELL-03**: `ApiError` supports `bad_request`, `conflict`, `not_implemented` in addition to existing variants
- **SHELL-04**: Every api_v1 handler uses a `DidView`-style view type; no SeaORM `Model` is ever serialized directly
- **SHELL-05**: A console handler refactor convention exists where data-fetch fns become module-level `pub(crate)` functions keyed on `&DatabaseConnection`, and both HTML and JSON handlers call them
- **GWY-01**: Operator can create a gateway via `POST /api/v1/gateways` with auth, health thresholds, and transport config
- **GWY-02**: Operator can update an existing gateway via `PUT /api/v1/gateways/{name}` without restarting health monitoring
- **GWY-03**: Operator can delete a gateway via `DELETE /api/v1/gateways/{name}`; deletion is blocked with 409 if any trunk-group or DID references it
- **GWY-04**: Gateway create hooks the existing `proxy/gateway_health.rs` monitor loop so health state is visible via existing GET routes immediately
- **DID-01**: Operator can list DIDs with pagination and filters (trunk, mode, active)
- **DID-02**: Operator can create a DID with routing mode (`ai_agent`, `sip_proxy`, `webrtc_bridge`, `ws_bridge`)
- **DID-03**: Operator can retrieve, update, and delete a DID by number via `/api/v1/dids/{number}` (URL-encoded `+`)
- **DID-04**: DID lifecycle uses the same underlying model the console UI uses; console rendering is unchanged after the refactor
- **CDR-01**: Operator can list CDRs with filters (trunk, did, status, start_date, end_date, page, page_size)
- **CDR-02**: Operator can retrieve a single CDR by id
- **CDR-03**: Operator can delete a CDR by id
- **CDR-04**: Recording and sip-flow sub-resources return `501 Not Implemented` in Phase 1, promoted to real handlers in the Recordings phase
- **DIAG-01**: Operator can run route-evaluate as a dry-run matching a caller/destination pair against the live routing table
- **DIAG-02**: Operator can probe a gateway's OPTIONS response on demand without affecting health counters
- **DIAG-03**: Operator can list SIP registrations and query a single user's registration
- **DIAG-04**: Operator can query locator state (list and clear) for a given aor
- **DIAG-05**: Operator can fetch a combined diagnostics summary (registrations, health, recent flood events, recent auth failures)
- **SYS-01**: `GET /api/v1/system/health` returns uptime, db status, active call count, version
- **SYS-02**: `POST /api/v1/system/reload` collapses existing AMI reload endpoints (trunks, routes, acl, app) into one call and returns the elapsed time
- **IT-01**: Every new api_v1 sub-router has a dedicated test file under `tests/` that asserts 401 without auth, happy path, 404 on missing resource, and 400/409 on bad input
- **MIG-03**: Console UI routes render identically on every page touched by a refactor (sip_trunks, dids, call_records, routing, settings, diagnostics) — verified by spot check before phase merge

## Success Criteria

1. Operator can CRUD Gateways, DIDs, and CDRs via `/api/v1/*` with Bearer auth and get back the JSON shapes documented in `docs/CARRIER-API.md`
2. Operator can run route-evaluate, probe a gateway, list registrations, query locator state, and fetch a diagnostics summary without touching the console UI
3. `GET /api/v1/system/health` returns uptime/db_ok/active_calls/version and `POST /api/v1/system/reload` collapses all four AMI reload endpoints into one call with elapsed time
4. Every existing console HTML route (sip_trunks, dids, call-records, routing, diagnostics, settings) renders identically after the data-fn extraction refactor
5. Every sub-router ships with an integration test asserting 401-without-auth, happy-path, 404-missing, 400/409-bad-input

## Affected Subsystems

- [handler](../04-subsystems/)
- [proxy](../04-subsystems/)
- [console](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

- `01-01-PLAN.md` — Adapter shell foundation
- `01-02-PLAN.md` — Gateways sub-router
- `01-03-PLAN.md` — DIDs & CDRs sub-routers
- `01-04-PLAN.md` — Diagnostics sub-router
- `01-05-PLAN.md` — System sub-router (health & reload)
- `01-06-PLAN.md` — Gap closure (SYS-02 real work, GWY-04 health observability)

## Completion Summary

- **78/78 tests passing** (75 baseline + 3 new from gap closure plan 01-06)
- **23/26 requirements verified + 3 deferred:**
  - DIAG-05 deferred to Phase 10 (Security Suite) — flood/auth-failure trackers do not exist until then
  - SHELL-05 ADR-closed — model-layer sharing accepted as the adapter sink
  - MIG-03 deferred to manual QA before merge — no templates touched
- **Gap closures:**
  - SYS-02 reload real work — `reload_steps.rs` module with 3 sequential reload steps
  - GWY-04 health observable — `tally_snapshot(id)` accessor on `GatewayHealthMonitor`

---
**Status:** ✅ Shipped
**Planning artifacts:** `.planning/phases/01-api-shell-cheap-wrappers/`
**Last reviewed:** 2026-04-16
