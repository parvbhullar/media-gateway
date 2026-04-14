# Phase 1 — API shell + cheap wrappers

**Date:** 2026-04-14
**Depends on:** [2026-04-14-carrier-api-gap-closure.md](./2026-04-14-carrier-api-gap-closure.md) Phase 0 decisions
**Goal:** Ship ~17 `/api/v1/*` routes that wrap existing console handlers with zero new business logic. Prove the API shell, lock the JSON contract pattern, establish the adapter convention every later phase will reuse.

## Scope

| Group | Routes | Source |
|---|---|---|
| Gateways writes | POST, PUT, DELETE `/gateways[/{name}]` (3) | `console/handlers/sip_trunk.rs` |
| DIDs | GET/POST/GET/PUT/DELETE `/dids[/{number}]` (5) | `console/handlers/did.rs` |
| CDRs | GET/GET/DELETE `/cdrs[/{id}]` + `/recording` 501 + `/sip-flow` (5) | `console/handlers/call_record.rs` |
| Diagnostics | `/route-evaluate`, `/registrations`, `/registrations/{user}`, `/summary` (4) | `console/handlers/diagnostics.rs` + proxy/registrar/locator |
| System | `/health`, `/reload` (2) | `handler/ami.rs` |

**Out of scope for Phase 1:** Trunks (Phase 2 needs the new table), Routing (records adapter deferred), Active Calls (Phase 4 handles enum variants), Webhooks / Security / Translations / Manipulations (greenfield phases), Endpoints (Phase 12).

## The adapter pattern

Every console handler today owns two concerns in one function: (a) fetch/mutate data via SeaORM, (b) render an HTML template. Phase 1 extracts (a) into a pure async fn keyed on `&DatabaseConnection`, then both the HTML handler and the new JSON handler call it.

### Before (example from `console/handlers/did.rs`)

```rust
async fn list_dids(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Query(q): Query<DidListQuery>,
) -> Response {
    let db = state.db();
    let rows = DidEntity::find()
        .filter(/* … */)
        .all(db).await.unwrap();
    state.render_with_headers("console/dids.html", json!({ "dids": rows }), &headers)
}
```

### After

```rust
// Pure data layer — no HTML, no auth, no state coupling.
pub(crate) async fn query_dids(
    db: &DatabaseConnection,
    filter: DidListFilter,
) -> Result<Vec<DidModel>, DbErr> {
    DidEntity::find().filter(filter.apply()).all(db).await
}

// HTML handler — unchanged behavior, now thin.
async fn list_dids(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_user): AuthRequired,
    Query(q): Query<DidListQuery>,
) -> Response {
    let rows = match query_dids(state.db(), q.into()).await {
        Ok(r) => r,
        Err(e) => return internal_error(e),
    };
    state.render_with_headers("console/dids.html", json!({ "dids": rows }), &headers)
}

// New JSON handler in api_v1/dids.rs.
async fn api_list_dids(
    State(state): State<AppState>,
    Query(q): Query<DidListQuery>,
) -> ApiResult<Json<Vec<DidView>>> {
    let rows = query_dids(state.db(), q.into())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(rows.into_iter().map(DidView::from).collect()))
}
```

**Three rules:**
1. **Pure fn signature is `(&DatabaseConnection, TypedInput) -> Result<TypedOutput, DbErr>`.** No `State`, no `Response`, no `AuthRequired`. This is what makes it usable from both `ConsoleState` and `AppState`.
2. **JSON handlers own their view types.** `DidView` in `api_v1/dids.rs` is the wire contract; `DidModel` is the storage type. Never serialize SeaORM models directly — they drift.
3. **Errors go through `ApiError` only.** Console keeps its existing error paths. Never let a SeaORM `DbErr` escape to JSON.

## ConsoleState ↔ AppState bridge

No new bridge code required. Today:

- `AppState = Arc<AppStateInner>` ([app.rs:80](../../src/app.rs#L80))
- `AppStateInner.db()` returns `&DatabaseConnection` ([app.rs:98-100](../../src/app.rs#L98-L100))
- `AppStateInner.console: Option<Arc<ConsoleState>>` ([app.rs:74-75](../../src/app.rs#L74-L75)) already exists
- `ConsoleState.db()` returns the same underlying pool

Because the pure fns key on `&DatabaseConnection`, api_v1 handlers just call `state.db()` and are done. The `console` field stays as a fallback if a phase-1 route discovers it needs a console-only helper (e.g., pagination utilities in `console/handlers/utils.rs`).

## Router wiring

Edit [`src/handler/api_v1/mod.rs`](../../src/handler/api_v1/mod.rs):

```rust
pub mod auth;
pub mod error;
pub mod gateways;   // existing
pub mod dids;       // new
pub mod cdrs;       // new
pub mod diagnostics; // new
pub mod system;     // new

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

Each new file exposes a single `pub fn router() -> Router<AppState>` that returns its own routes — same shape as the existing `gateways.rs`. No global state plumbing.

## View types (the JSON contract)

All view types live under `api_v1/<group>.rs` and implement `From<Model>`. Never `#[derive(Serialize)]` directly on a SeaORM `Model` — that couples the wire format to column names and breaks when the DB evolves.

```rust
// api_v1/dids.rs
#[derive(Debug, Serialize)]
pub struct DidView {
    pub number: String,
    pub trunk: Option<String>,
    pub mode: String,
    pub playbook: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<DidModel> for DidView {
    fn from(m: DidModel) -> Self {
        Self {
            number: m.number,
            trunk: m.trunk_name,
            mode: m.mode.as_str().to_string(),
            playbook: m.playbook,
            created_at: m.created_at,
        }
    }
}
```

Cross-check every view struct against the example payload in `docs/CARRIER-API.md` before merging.

## Error envelope

Reuse the existing `ApiError` / `ApiResult` from [`api_v1/error.rs`](../../src/handler/api_v1/error.rs). Adapter rules:

- `DbErr::RecordNotFound` → `ApiError::not_found(...)` → 404
- `DbErr::*` otherwise → `ApiError::internal(...)` → 500
- Validation (bad JSON, missing required field) → `ApiError::bad_request(...)` → 400
- Conflict (engagement tracking for deletes) → `ApiError::conflict(...)` → 409

`/api/v1/cdrs/{id}/recording` and `/api/v1/cdrs/{id}/sip-flow` return `501 Not Implemented` per spec, with a clear body:
```json
{"error": "recording retrieval not implemented"}
```

## Task breakdown

Order matters — `dids` lands first because it has the simplest schema and proves the pattern end-to-end. Each task is an independent PR.

### 1.1 — Adapter pattern scaffolding (~0.5 day)
- Add `ApiError::bad_request`, `::conflict`, `::not_implemented` helpers to [`api_v1/error.rs`](../../src/handler/api_v1/error.rs) if not already present.
- Add a shared `Pagination` extractor (`page`, `page_size`) and `PaginatedResponse<T>` envelope in a new `api_v1/common.rs`.
- No route changes yet.

### 1.2 — DIDs (5 routes, ~1 day)
- Extract pure fns from [`console/handlers/did.rs`](../../src/console/handlers/did.rs) into `console/handlers/did.rs` module-level `pub(crate)` functions: `query_dids`, `fetch_did`, `create_did_row`, `update_did_row`, `delete_did_row`.
- Confirm existing HTML handlers still work after refactor (no test expected to change).
- New `src/handler/api_v1/dids.rs` with `DidView`, `CreateDidRequest`, `UpdateDidRequest` and 5 handlers.
- Register in `api_v1/mod.rs`.
- Contract tests: one per route, using examples from CARRIER-API.md §DIDs.

### 1.3 — Gateways writes (3 routes, ~1 day)
- Extract `create_sip_trunk_row`, `update_sip_trunk_row`, `delete_sip_trunk_row` from [`console/handlers/sip_trunk.rs`](../../src/console/handlers/sip_trunk.rs).
- Reuse existing `GatewayView` from [`api_v1/gateways.rs`](../../src/handler/api_v1/gateways.rs).
- Add `CreateGatewayRequest`, `UpdateGatewayRequest`.
- Hook gateway health monitoring on create (existing [`proxy/gateway_health.rs`](../../src/proxy/gateway_health.rs) call).
- Delete must respect engagement tracking — if any DID/trunk-group references the sip_trunk, return 409 (pre-check query).

### 1.4 — CDRs (5 routes, ~1 day)
- Extract `query_call_records`, `fetch_call_record`, `delete_call_record_row` from [`console/handlers/call_record.rs`](../../src/console/handlers/call_record.rs).
- Map console query params (`trunk`, `did`, `status`, `start_date`, `end_date`, `page`, `page_size`) 1:1.
- New `src/handler/api_v1/cdrs.rs` with `CdrView` + paginated list.
- `/recording` and `/sip-flow` return `501` — stubbed handlers, no console extraction needed.

### 1.5 — Diagnostics (4 routes, ~1 day)
- Extract `route_evaluate`, `probe_trunk_options`, `locator_lookup` from [`console/handlers/diagnostics.rs`](../../src/console/handlers/diagnostics.rs).
- `/diagnostics/registrations` — read from `proxy/registrar.rs` + `proxy/locator.rs`. If there's no console equivalent, write the pure fn directly against the proxy module.
- `/diagnostics/summary` — aggregate in-process from the other 4 endpoints; no new data source.

### 1.6 — System (2 routes, ~0.5 day)
- `/system/health` — wrap [`handler/ami.rs::health_handler`](../../src/handler/ami.rs#L22) data layer. Response: `{uptime_secs, db_ok, active_calls, version}`.
- `/system/reload` — collapse ami's `reload/trunks|routes|acl|app` into one handler that triggers all four. Mirror the existing reload semantics.

### 1.7 — Integration tests (~1 day)
- New test file `tests/api_v1_phase1.rs`.
- One test per route, asserting: 401 without auth, 200 with valid bearer token, correct JSON shape (schema compare to CARRIER-API.md example), 404 on missing resource.
- Seed minimal fixtures via existing `src/fixtures.rs`.
- Register in `scripts/run_tests.sh` per project convention.

## Verification checklist

Before marking Phase 1 done:

- [ ] All 17 routes respond with the documented JSON shape, verified against `docs/CARRIER-API.md` examples.
- [ ] 401 on missing/invalid Bearer token across every route.
- [ ] Console HTML routes still render identically — spot-check the 5 pages (dids, sip-trunks, call-records, diagnostics, settings).
- [ ] `cargo test` passes, including the new `tests/api_v1_phase1.rs`.
- [ ] `cargo clippy --all-targets` clean.
- [ ] No new `unwrap()` / `expect()` in the adapter layer — all errors go through `ApiError`.
- [ ] No SeaORM `Model` serialized directly in any JSON response.

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Console handler extraction accidentally changes HTML behavior | Leave the handler signature unchanged; only move the inner logic into a `pub(crate) fn`. Spot-check render output after each extraction. |
| Engagement tracking check on gateway delete is expensive | One indexed count query per delete is fine; if performance matters later, move to a row-level FK constraint. |
| `/system/reload` triggers four AMI reloads concurrently | Serialize via the existing `pending_reload` `AtomicBool` in `ConsoleState`. |
| Pagination envelope inconsistent with spec | The spec shows `page`/`page_size` query params but no response envelope format. Pick `{items, page, page_size, total}` and document it — every later phase reuses it. |

## Open questions for Phase 0 review

1. **Pagination envelope shape** — no spec example exists. Proposal: `{items: [...], page, page_size, total}`. Needs sign-off before Phase 1.2 lands.
2. **Deletion semantics for DIDs** — spec says DELETE `/dids/{number}`. Does that hard-delete the row or soft-delete (set `is_active = false`)? Console uses hard delete. Proposal: match console.
3. **Reload response shape** — `/system/reload` returns what? Proposal: `{reloaded: ["trunks", "routes", "acl", "app"], elapsed_ms: 42}`.

Resolve these three before starting 1.1.
