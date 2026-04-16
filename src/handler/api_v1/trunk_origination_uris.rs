//! `/api/v1/trunks/{name}/origination_uris` — TSUB-02 sub-router.
//!
//! Plan 03-01 ships this as a 501-stub mounted in mod.rs. Plan 03-03
//! replaces every handler body with the full implementation backed by
//! `supersip_trunk_origination_uris`.

use axum::{
    Router,
    extract::{Path, State},
    routing::{delete, get},
};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/trunks/{name}/origination_uris",
            get(list_uris_stub).post(add_uri_stub),
        )
        .route(
            "/trunks/{name}/origination_uris/{uri}",
            delete(delete_uri_stub),
        )
}

async fn list_uris_stub(
    State(_state): State<AppState>,
    Path(_name): Path<String>,
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "trunk origination_uris list — Plan 03-03",
    ))
}

async fn add_uri_stub(
    State(_state): State<AppState>,
    Path(_name): Path<String>,
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "trunk origination_uris add — Plan 03-03",
    ))
}

async fn delete_uri_stub(
    State(_state): State<AppState>,
    Path(_p): Path<(String, String)>,
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "trunk origination_uris delete — Plan 03-03",
    ))
}
