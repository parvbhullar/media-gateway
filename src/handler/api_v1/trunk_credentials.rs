//! `/api/v1/trunks/{name}/credentials` — TSUB-01 sub-router.
//!
//! Plan 03-01 ships this as a 501-stub mounted in mod.rs. Plan 03-02
//! replaces every handler body with the full implementation backed by
//! `supersip_trunk_credentials`.

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
            "/trunks/{name}/credentials",
            get(list_credentials_stub).post(add_credential_stub),
        )
        .route(
            "/trunks/{name}/credentials/{realm}",
            delete(delete_credential_stub),
        )
}

async fn list_credentials_stub(
    State(_state): State<AppState>,
    Path(_name): Path<String>,
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "trunk credentials list — Plan 03-02",
    ))
}

async fn add_credential_stub(
    State(_state): State<AppState>,
    Path(_name): Path<String>,
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "trunk credentials add — Plan 03-02",
    ))
}

async fn delete_credential_stub(
    State(_state): State<AppState>,
    Path(_p): Path<(String, String)>,
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "trunk credentials delete — Plan 03-02",
    ))
}
