//! `/api/v1/trunks/{name}/media` — TSUB-03 sub-router.
//!
//! Plan 03-01 ships this as a 501-stub mounted in mod.rs. Plan 03-04
//! replaces every handler body with the full implementation reading/
//! writing the `rustpbx_trunk_groups.media_config` JSON column.

use axum::{
    Router,
    extract::{Path, State},
    routing::{get, put},
};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/trunks/{name}/media", get(get_media_stub))
        .route("/trunks/{name}/media", put(put_media_stub))
}

async fn get_media_stub(
    State(_state): State<AppState>,
    Path(_name): Path<String>,
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "trunk media get — Plan 03-04",
    ))
}

async fn put_media_stub(
    State(_state): State<AppState>,
    Path(_name): Path<String>,
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "trunk media put — Plan 03-04",
    ))
}
