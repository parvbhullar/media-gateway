//! `/api/v1/routing/tables/{name}/records[/{record_id}]` — RTE-02 record
//! CRUD surface.
//!
//! Phase 6 Plan 06-01 — STUB. All five record-level endpoints respond
//! with `501 Not Implemented` until Plan 06-03 lands the real handlers.
//! The stub exists so Wave 1 owns the `mod.rs` edit (file-ownership
//! invariant) and Plan 06-03 only replaces handler bodies, not router
//! wiring.
//!
//! NOTE: This file is the Wave-1 STUB. Plan 06-03 replaces the
//! handler bodies below — DO NOT change the `pub fn router()`
//! signature or the route paths or 06-03 will need to re-touch
//! `mod.rs` (forbidden by Phase 5 file-ownership invariant).
//!
//! Endpoints (D-28):
//!   - GET    /routing/tables/{name}/records              — list records
//!   - POST   /routing/tables/{name}/records              — append record (server generates UUIDv4 record_id)
//!   - GET    /routing/tables/{name}/records/{record_id}  — get record
//!   - PUT    /routing/tables/{name}/records/{record_id}  — replace record (preserves position)
//!   - DELETE /routing/tables/{name}/records/{record_id}  — delete record

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
            "/routing/tables/{name}/records",
            get(list_records).post(create_record),
        )
        .route(
            "/routing/tables/{name}/records/{record_id}",
            get(get_record).put(update_record).delete(delete_record),
        )
}

async fn list_records(Path(_name): Path<String>) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-03"))
}

async fn create_record(
    Path(_name): Path<String>,
    Json(_body): Json<Value>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-03"))
}

async fn get_record(
    Path((_name, _record_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-03"))
}

async fn update_record(
    Path((_name, _record_id)): Path<(String, String)>,
    Json(_body): Json<Value>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-03"))
}

async fn delete_record(
    Path((_name, _record_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    Err(ApiError::not_implemented("phase 6 plan 06-03"))
}
