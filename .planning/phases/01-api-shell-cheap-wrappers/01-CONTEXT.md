# Phase 1: API Shell & Cheap Wrappers — Context

**Gathered:** 2026-04-15
**Status:** Ready for planning
**Source:** PRD Express Path (`docs/plans/2026-04-14-phase-1-api-shell.md`)

<domain>
## Phase Boundary

Phase 1 ships ~17 routes at `/api/v1/*` that wrap existing console handlers with zero new business logic. It establishes **the adapter convention** that every subsequent phase in v2.0 will reuse. No schema changes, no proxy hot-path changes, no new rule engines.

**Route inventory for Phase 1:**

| Group | Routes | Source module |
|---|---|---|
| Gateways writes | `POST /api/v1/gateways`, `PUT /api/v1/gateways/{name}`, `DELETE /api/v1/gateways/{name}` (3) | `src/console/handlers/sip_trunk.rs` |
| DIDs | `GET /dids`, `POST /dids`, `GET /dids/{number}`, `PUT /dids/{number}`, `DELETE /dids/{number}` (5) | `src/console/handlers/did.rs` |
| CDRs | `GET /cdrs`, `GET /cdrs/{id}`, `DELETE /cdrs/{id}`, `GET /cdrs/{id}/recording` (501), `GET /cdrs/{id}/sip-flow` (501) (5) | `src/console/handlers/call_record.rs` |
| Diagnostics | `POST /diagnostics/route-evaluate`, `GET /diagnostics/registrations`, `GET /diagnostics/registrations/{user}`, `GET /diagnostics/summary` (4) | `src/console/handlers/diagnostics.rs` + `src/proxy/registrar.rs` + `src/proxy/locator.rs` |
| System | `GET /api/v1/system/health`, `POST /api/v1/system/reload` (2) | `src/handler/ami.rs` |

**Total:** 19 handler functions (17 live + 2 return 501 per spec).

**Out of scope for this phase:**

- Trunks core CRUD (Phase 2 — needs new `trunk_groups` table)
- Routing CRUD (Phase 6 — records sub-route adapter deferred)
- Active Calls (Phase 4 — may need new `CallCommandPayload` variants)
- Webhooks / Security / Translations / Manipulations (dedicated greenfield phases)
- Listeners Endpoints (Phase 12 — read-only projection)
- Recordings first-class (Phase 12)

</domain>

<decisions>
## Implementation Decisions

Every item below is a LOCKED decision from the design doc and PROJECT.md Key Decisions table.

### Adapter Pattern (SHELL-05)

- **Pure data-fetch fns** are extracted from existing console handlers into module-level `pub(crate) async fn` functions.
- Signature must be `(&DatabaseConnection, TypedInput) -> Result<TypedOutput, DbErr>`.
- **No `State`, no `Response`, no `AuthRequired`** in the pure fn signature — that's what lets both `ConsoleState` and `AppState` call them.
- Both the HTML handler (keeps its existing signature with `State<Arc<ConsoleState>>`) and the new JSON handler (takes `State<AppState>`) call the same pure fn.
- The pure fns live in the same file as the existing console handler, keyed on `&DatabaseConnection` only.

### No new State bridge needed

- `AppStateInner.console: Option<Arc<ConsoleState>>` already exists at [src/app.rs:74-75](../../../src/app.rs#L74-L75)
- `AppStateInner.db()` returns `&DatabaseConnection` at [src/app.rs:98-100](../../../src/app.rs#L98-L100)
- `ConsoleState.db()` returns the same underlying pool
- Therefore: api_v1 handlers call `state.db()` and pass it to the pure fn. Zero new plumbing.

### View types (SHELL-04)

- **Never serialize SeaORM `Model` types directly** — they couple the wire format to DB columns and break on schema evolution.
- Each api_v1 sub-router owns its own view struct (`DidView`, `CdrView`, `DiagnosticsSummary`, etc.) with `#[derive(Serialize)]`.
- View types implement `From<Model>` for the DB row type.
- Every view struct must be cross-checked against the JSON example in `docs/CARRIER-API.md` before merge.

### Pagination envelope (SHELL-02)

- Shape: `{items: [...], page, page_size, total}` — DECIDED in the design doc's open questions, now locked.
- Lives in a new `src/handler/api_v1/common.rs` alongside a `Pagination` query extractor with fields `page`, `page_size` (defaults: 1, 20).
- Every list endpoint uses this envelope.

### Error envelope (SHELL-03)

- Reuse existing `ApiError` / `ApiResult` from [src/handler/api_v1/error.rs](../../../src/handler/api_v1/error.rs).
- Add missing helpers: `ApiError::bad_request(msg)`, `::conflict(msg)`, `::not_implemented(msg)`.
- Mapping:
  - `DbErr::RecordNotFound` → `ApiError::not_found` → 404
  - `DbErr::*` otherwise → `ApiError::internal` → 500
  - Validation (bad JSON, missing required field) → `ApiError::bad_request` → 400
  - Engagement conflict on delete → `ApiError::conflict` → 409
  - `/cdrs/{id}/recording` and `/cdrs/{id}/sip-flow` → `ApiError::not_implemented` → 501 with body `{"error": "recording retrieval not implemented"}`

### Router wiring (SHELL-01)

Edit [src/handler/api_v1/mod.rs](../../../src/handler/api_v1/mod.rs) to declare new sub-modules and merge them:

```rust
pub mod auth;
pub mod error;
pub mod common;       // new — pagination + shared types
pub mod gateways;     // existing — extend with writes
pub mod dids;         // new
pub mod cdrs;         // new
pub mod diagnostics;  // new (gateways.rs already hosts trunk-test)
pub mod system;       // new

pub fn api_v1_router(state: AppState) -> Router {
    let protected: Router<AppState> = Router::new()
        .merge(gateways::router())
        .merge(dids::router())
        .merge(cdrs::router())
        .merge(diagnostics::router())
        .merge(system::router());

    Router::<AppState>::new()
        .nest("/api/v1", protected)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::api_v1_auth_middleware,
        ))
        .with_state(state)
}
```

Each new file exposes exactly one `pub fn router() -> Router<AppState>`. No global plumbing.

### DID delete semantics

- **Hard delete** (matches console behavior). No soft-delete / `is_active=false` for v2.0.
- Decided in the design doc's open questions section — locked.

### Reload response shape (SYS-02)

- `POST /api/v1/system/reload` collapses `handler/ami.rs::reload_trunks`, `reload_routes`, `reload_acl`, `reload_app` into one call.
- Response shape: `{reloaded: ["trunks", "routes", "acl", "app"], elapsed_ms: 42}`.
- Reloads run **sequentially** serialized via the existing `ConsoleState.pending_reload` `AtomicBool` — prevents concurrent reload storms.

### Integration test convention (IT-01)

- Each new api_v1 sub-router has a dedicated test file under `tests/api_v1_<group>.rs`.
- Every test file asserts minimum 4 cases per route:
  1. `401 Unauthorized` without Bearer token
  2. Happy path with valid token — asserts JSON shape matches `docs/CARRIER-API.md` example
  3. `404 Not Found` on missing resource
  4. `400 Bad Request` or `409 Conflict` on bad input (whichever applies)
- Test fixtures seed via existing `src/fixtures.rs`.
- New test modules must be registered in `scripts/run_tests.sh` per project CLAUDE.md convention.

### Console render-parity spot check (MIG-03)

- After every data-fn extraction, manually load the corresponding console HTML page in a browser and verify it renders identically.
- Pages to check: `sip_trunks`, `dids`, `call-records`, `routing`, `diagnostics`, `settings`.
- No automated test for this — it's a pre-merge spot check only.

### Engagement tracking for gateway delete (GWY-03)

- Before deleting a `sip_trunk` row, query:
  - Any DID with `trunk_name = {name}`?
  - Any routing record referencing `{name}` as a target?
- If either query returns a row, respond `409 Conflict` with body naming the referencing resource.
- One indexed count query per check is acceptable for Phase 1.

### Diagnostics summary aggregation (DIAG-05)

- `GET /api/v1/diagnostics/summary` aggregates in-process from the other four diagnostics endpoints — no new data source.
- Shape: `{registrations: {count, users}, locator: {active_aors}, recent_flood_events: N, recent_auth_failures: N}`.
- If any component errors, its slot returns `null` and the summary still ships 200.

### System health shape (SYS-01)

- `GET /api/v1/system/health` returns:
  ```json
  {
    "uptime_secs": 1234,
    "db_ok": true,
    "active_calls": 42,
    "version": "0.x.y"
  }
  ```
- Maps existing AMI `health_handler` internals into JSON form without changing AMI.

### Claude's Discretion

Areas the design doc does not fully lock. Planner and implementer should choose sensibly and document the choice in the PR:

- Exact JSON field names for `GatewayView` additions on create/update response (prefer matching the existing `GatewayView` in `src/handler/api_v1/gateways.rs:22-35`)
- Query parameter parsing crate (`serde_qs` vs `axum::Query` default) — prefer the one already used by the existing `gateways.rs`
- Whether to add a structured request-id extractor now (Phase 1) or defer to Phase 11 — recommend defer unless trivially cheap
- Whether to log the full JSON body on bad-request errors for debugging — recommend log only the error message + route, never request body (PII risk)

</decisions>

<specifics>
## Specific Implementation Details

### Task order (from design doc 1.1–1.7)

1. **Task 1.1 — Scaffolding** (~0.5 day): extend `ApiError` helpers, add `common.rs` with `Pagination` and `PaginatedResponse<T>`.
2. **Task 1.2 — DIDs** (~1 day): extract pure fns from `console/handlers/did.rs`, create `api_v1/dids.rs`, 5 routes, tests.
3. **Task 1.3 — Gateways writes** (~1 day): extract pure fns from `console/handlers/sip_trunk.rs`, extend existing `api_v1/gateways.rs`, 3 write routes + engagement tracking, tests.
4. **Task 1.4 — CDRs** (~1 day): extract pure fns from `console/handlers/call_record.rs`, create `api_v1/cdrs.rs`, 3 live routes + 2 501 stubs, tests.
5. **Task 1.5 — Diagnostics** (~1 day): extract from `console/handlers/diagnostics.rs` and proxy modules, create `api_v1/diagnostics.rs`, 4 routes + summary aggregator, tests.
6. **Task 1.6 — System** (~0.5 day): create `api_v1/system.rs` wrapping `handler/ami.rs` data, 2 routes (health + reload), tests.
7. **Task 1.7 — Integration test wiring** (~1 day): ensure all new `tests/api_v1_*.rs` files registered in `scripts/run_tests.sh`, fixtures seeded, 401/happy/404/400-409 coverage verified for every route.

**Ordering rationale:** DIDs first because it's the simplest schema and proves the pattern end-to-end. Gateways writes second because the view type already exists. CDRs third because it introduces pagination. Diagnostics fourth because it mixes console + proxy modules. System last because it's the thinnest wrapper.

### Files touched

**New files (6):**
- `src/handler/api_v1/common.rs`
- `src/handler/api_v1/dids.rs`
- `src/handler/api_v1/cdrs.rs`
- `src/handler/api_v1/diagnostics.rs`
- `src/handler/api_v1/system.rs`
- `tests/api_v1_phase1.rs` (or split per group)

**Modified files (6):**
- `src/handler/api_v1/mod.rs` (declare and merge new sub-routers)
- `src/handler/api_v1/error.rs` (add helpers)
- `src/handler/api_v1/gateways.rs` (add 3 write routes)
- `src/console/handlers/did.rs` (extract pure fns)
- `src/console/handlers/sip_trunk.rs` (extract pure fns)
- `src/console/handlers/call_record.rs` (extract pure fns)
- `src/console/handlers/diagnostics.rs` (extract pure fns)
- `src/handler/ami.rs` (extract pure fns)
- `scripts/run_tests.sh` (register new test modules)

**Not touched in Phase 1:**
- `src/proxy/*` (no hot-path changes)
- `src/models/*` (no schema changes)
- `src/app.rs` (existing state plumbing is sufficient)
- Console HTML templates (render must remain identical)

### Test fixtures

Phase 1 seeds via `src/fixtures.rs`:
- Minimum 2 sip_trunk rows (one active, one disabled)
- Minimum 2 DIDs (one proxy, one ai_agent)
- Minimum 2 call_record rows
- 1 pre-provisioned API key for Bearer auth (use `api_key_store` fixtures, already exists)

</specifics>

<deferred>
## Deferred Ideas

- Trunk core CRUD — Phase 2 (gated on `trunk_groups` schema)
- Trunk credentials / origination URIs / media / capacity / ACL — Phases 3 and 5
- Routing CRUD (tables + records) — Phase 6 (records sub-route adapter is non-trivial)
- Routing `/resolve` dry-run — Phase 3 (thin shim over `proxy/routing/matcher.rs`)
- Active calls list/control — Phase 4
- Mid-call `play`/`speak`/`dtmf`/`record` — Phase 4
- Webhook CRUD + background processor — Phase 7
- Translations engine — Phase 8
- Manipulations engine — Phase 9
- Security firewall/flood/brute-force — Phase 10
- `/system/info` / `/config` / `/stats` / `/cluster` — Phase 11
- CDR search / recent / export — Phase 11
- Listeners read-only projection + 501 writes — Phase 12
- Recordings first-class CRUD — Phase 12
- Endpoints as SIP user-agents — Phase 13
- Applications / XML routing — Phase 13
- Sub-accounts + account scoping retrofit — Phase 13

## Validation Architecture

Phase 1 validation is straightforward:

**Unit-level:**
- None required. All logic is pure data-fetch + JSON serialization. Unit tests on view type conversions are optional.

**Integration-level (required):**
- One `tests/api_v1_<group>.rs` file per sub-router (IT-01).
- Each file covers: 401 without auth, happy path with fixture, 404 missing, 400/409 bad input.
- Minimum ~68 test cases across 5 sub-routers (17 routes × 4 cases).

**End-to-end / manual:**
- Console render-parity spot check on 6 pages (MIG-03) — manual pre-merge only.

**Regression:**
- Full existing `cargo test` suite must continue to pass — Phase 1 must not break any existing test.

**Verification commands (per project CLAUDE.md):**
```bash
cargo build --all-targets
cargo test
cargo clippy --all-targets
bash scripts/run_tests.sh api_v1_phase1
```

</deferred>

---

*Phase: 01-api-shell-cheap-wrappers*
*Context gathered: 2026-04-15 via PRD Express Path*
*Source: docs/plans/2026-04-14-phase-1-api-shell.md*
