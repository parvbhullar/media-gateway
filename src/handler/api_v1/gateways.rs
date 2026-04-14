//! `/api/v1/gateways` and `/api/v1/diagnostics/trunk-test` endpoints (Plan 1).
//!
//! Read-only views of the `rustpbx_sip_trunks` table plus an on-demand
//! OPTIONS probe that does NOT mutate the database.

use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::sip_trunk::{Column as TrunkColumn, Entity as TrunkEntity, Model as TrunkModel};
use crate::proxy::gateway_health::probe_trunk;

#[derive(Debug, Serialize)]
pub struct GatewayView {
    pub name: String,
    pub display_name: Option<String>,
    pub direction: String,
    pub proxy_addr: Option<String>,
    pub transport: String,
    pub status: String,
    pub is_active: bool,
    pub last_health_check_at: Option<chrono::DateTime<chrono::Utc>>,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub failure_threshold: i32,
    pub recovery_threshold: i32,
    pub health_check_interval_secs: i32,
}

impl From<TrunkModel> for GatewayView {
    fn from(m: TrunkModel) -> Self {
        Self {
            name: m.name,
            display_name: m.display_name,
            direction: m.direction.as_str().to_string(),
            proxy_addr: m.outbound_proxy.clone().or(m.sip_server.clone()),
            transport: m.sip_transport.as_str().to_string(),
            status: m.status.as_str().to_string(),
            is_active: m.is_active,
            last_health_check_at: m.last_health_check_at,
            consecutive_failures: m.consecutive_failures,
            consecutive_successes: m.consecutive_successes,
            failure_threshold: m.failure_threshold.unwrap_or(3),
            recovery_threshold: m.recovery_threshold.unwrap_or(2),
            health_check_interval_secs: m.health_check_interval_secs.unwrap_or(30),
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/gateways", get(list_gateways))
        .route("/gateways/{name}", get(get_gateway))
        .route("/diagnostics/trunk-test", post(trunk_test))
}

async fn list_gateways(State(state): State<AppState>) -> ApiResult<Json<Vec<GatewayView>>> {
    let db = state.db();
    let rows = TrunkEntity::find()
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(rows.into_iter().map(GatewayView::from).collect()))
}

async fn get_gateway(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<GatewayView>> {
    let db = state.db();
    let row = TrunkEntity::find()
        .filter(TrunkColumn::Name.eq(name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("gateway '{}' not found", name)))?;
    Ok(Json(row.into()))
}

#[derive(Debug, Deserialize)]
pub struct TrunkTestReq {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct TrunkTestResp {
    pub ok: bool,
    pub latency_ms: u64,
    pub detail: String,
}

async fn trunk_test(
    State(state): State<AppState>,
    Json(req): Json<TrunkTestReq>,
) -> ApiResult<Json<TrunkTestResp>> {
    let db = state.db();
    let row = TrunkEntity::find()
        .filter(TrunkColumn::Name.eq(req.name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("gateway '{}' not found", req.name)))?;
    let endpoint_inner = state.sip_server().inner.endpoint.inner.clone();
    let outcome = probe_trunk(&endpoint_inner, &row, Duration::from_secs(5)).await;
    Ok(Json(TrunkTestResp {
        ok: outcome.ok,
        latency_ms: outcome.latency_ms,
        detail: outcome.detail,
    }))
}
