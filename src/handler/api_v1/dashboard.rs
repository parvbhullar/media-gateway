//! `/api/v1/dashboard` — JSON dashboard summary (metrics, call direction,
//! active calls, timeline) that mirrors the console `/console/dashboard/data`
//! payload exactly. Both endpoints call
//! `console::handlers::dashboard::fetch_dashboard_payload` so the wire
//! shape stays identical.
//!
//! Available only when the `console` feature/state is enabled — the
//! payload depends on `ConsoleState` for display timezone and the SIP
//! server handle. If `state.console` is `None`, returns 503.

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::Deserialize;

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};

#[derive(Debug, Deserialize)]
pub struct DashboardQuery {
    pub range: Option<String>,
    /// Substring match against from_number OR to_number on call records,
    /// and caller OR callee on the active-calls preview.
    pub number: Option<String>,
    /// Exact direction filter (case-insensitive): inbound | outbound | internal.
    pub direction: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/dashboard", get(handle_dashboard))
}

#[cfg(feature = "console")]
async fn handle_dashboard(
    State(state): State<AppState>,
    Query(query): Query<DashboardQuery>,
) -> ApiResult<Json<crate::console::handlers::dashboard::DashboardPayload>> {
    let console = state
        .console
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("console state not initialized"))?;
    let filters = crate::console::handlers::dashboard::DashboardFilters {
        number: query.number,
        direction: query.direction,
    };
    let payload = crate::console::handlers::dashboard::fetch_dashboard_payload(
        console,
        query.range.as_deref(),
        filters,
    )
    .await;
    Ok(Json(payload))
}

#[cfg(not(feature = "console"))]
async fn handle_dashboard(
    State(_state): State<AppState>,
    Query(_query): Query<DashboardQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    Err(ApiError::unavailable(
        "dashboard requires the `console` feature",
    ))
}
