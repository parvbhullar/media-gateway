//! `/api/v1/routing/tables[/{name}]` — RTE-01 routing-table CRUD surface.
//!
//! Phase 6 Plan 06-01 — STUB. All five table-level endpoints respond with
//! `501 Not Implemented` until Plan 06-02 lands the real handlers. The
//! stub exists so Wave 1 owns the `mod.rs` edit (file-ownership
//! invariant) and Plan 06-02 only replaces handler bodies, not router
//! wiring.
//!
//! NOTE: This file is the Wave-1 STUB. Plan 06-02 replaces the
//! handler bodies below — DO NOT change the `pub fn router()`
//! signature or the route paths or 06-02 will need to re-touch
//! `mod.rs` (forbidden by Phase 5 file-ownership invariant).
//!
//! Endpoints (D-27):
//!   - GET    /routing/tables               — list tables
//!   - POST   /routing/tables               — create table
//!   - GET    /routing/tables/{name}        — get one table
//!   - PUT    /routing/tables/{name}        — replace table metadata (NOT records, D-04)
//!   - DELETE /routing/tables/{name}        — delete table

use axum::{
    Json, Router,
    extract::Path,
    routing::get,
};
use serde_json::Value;

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/routing/tables",
            get(list_tables).post(create_table),
        )
        .route(
            "/routing/tables/{name}",
            get(get_table).put(update_table).delete(delete_table),
        )
}

async fn list_tables() -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-02"))
}

async fn create_table(
    Json(_body): Json<Value>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-02"))
}

async fn get_table(Path(_name): Path<String>) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-02"))
}

async fn update_table(
    Path(_name): Path<String>,
    Json(_body): Json<Value>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-02"))
}

async fn delete_table(Path(_name): Path<String>) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-02"))
}
