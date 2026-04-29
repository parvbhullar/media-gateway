//! Phase 8 — Translations CRUD router (TRN-01).
//!
//! Plan 08-01 stub. Endpoints are mounted and respond, but bodies are
//! placeholders so the parent router compiles and 08-02 can replace handler
//! bodies WITHOUT touching `mod.rs` (Phase 5 / 6 / 7 file-ownership pattern).
//!
//! Phase 8 file-ownership: 08-01 owns `mod.rs` registration; 08-02 fills
//! handler bodies; 08-03 / 08-04 do not touch this file.
//!
//! Endpoints (D-26):
//!   - GET    /translations          — list (stub: empty paginated payload)
//!   - POST   /translations          — create (stub: 501)
//!   - GET    /translations/{name}   — fetch by name (D-04)         (stub: 501)
//!   - PUT    /translations/{name}   — full replacement             (stub: 501)
//!   - DELETE /translations/{name}   — remove                       (stub: 501)
//!
//! Response shape for GET list (D-27): `{ results, total, page, page_size }`
//! so the 08-02 happy-path test on an empty list passes against this stub.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde_json::{Value, json};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};

// ─── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/translations", get(list).post(create))
        .route(
            "/translations/{name}",
            get(fetch).put(replace).delete(remove),
        )
}

// ─── Stub handlers (08-02 fills bodies) ──────────────────────────────────

async fn list(State(_state): State<AppState>) -> ApiResult<Json<Value>> {
    // D-27 paginated envelope; empty until 08-02 wires the DB query.
    Ok(Json(json!({
        "results": [],
        "total": 0,
        "page": 1,
        "page_size": 50,
    })))
}

async fn create(State(_state): State<AppState>) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented(
        "translations create — body lands in 08-02",
    ))
}

async fn fetch(
    State(_state): State<AppState>,
    Path(_name): Path<String>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented(
        "translations fetch — body lands in 08-02",
    ))
}

async fn replace(
    State(_state): State<AppState>,
    Path(_name): Path<String>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented(
        "translations replace — body lands in 08-02",
    ))
}

async fn remove(
    State(_state): State<AppState>,
    Path(_name): Path<String>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented(
        "translations remove — body lands in 08-02",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds_without_panic() {
        // The router constructor must succeed — if any stub handler signature
        // diverges from axum's expectations, this would fail to compile.
        let _r = router();
    }
}
