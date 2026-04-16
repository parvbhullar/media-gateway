//! `/api/v1/routing/resolve` — RTE-03 dry-run route resolution.
//!
//! Plan 03-01 ships this as a 501-stub mounted in mod.rs. Plan 03-05
//! replaces the handler with a dry-run that builds an InviteOption
//! from the request body and calls `match_invite_with_trace`, returning
//! the resolved target + trace events.

use axum::{
    Router,
    extract::State,
    routing::post,
};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};

pub fn router() -> Router<AppState> {
    Router::new().route("/routing/resolve", post(resolve_route_stub))
}

async fn resolve_route_stub(
    State(_state): State<AppState>,
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "/routing/resolve — Plan 03-05",
    ))
}
