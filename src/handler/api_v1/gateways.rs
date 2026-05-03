//! `/api/v1/gateways` and `/api/v1/diagnostics/trunk-test` endpoints.
//!
//! Plan 0 shipped list + get + trunk-test. Plan 1 (Phase 1 Plan 01-02) adds
//! write routes (POST/PUT/DELETE) that wrap the `rustpbx_sip_trunks`
//! ActiveModel with the shared [`ApiError`] envelope, strict create (409 on
//! duplicate), replace-on-update, and engagement-tracked delete (409 if any
//! DID references the gateway).
//!
//! The `sip_trunk::Model` is never serialized directly — [`GatewayView`]
//! owns the wire format, per SHELL-04.

use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::did::{Column as DidColumn, Entity as DidEntity};
use crate::models::sip_trunk::{
    self, ActiveModel as TrunkActiveModel, Column as TrunkColumn, Entity as TrunkEntity,
    Model as TrunkModel, SipTransport, SipTrunkDirection, SipTrunkStatus,
};
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
        .route("/gateways", get(list_gateways).post(create_gateway))
        .route(
            "/gateways/{name}",
            get(get_gateway).put(update_gateway).delete(delete_gateway),
        )
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

// ---------------------------------------------------------------------------
// Write routes (Phase 1 Plan 01-02)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateGatewayRequest {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub direction: Option<SipTrunkDirection>,
    #[serde(default)]
    pub sip_server: Option<String>,
    #[serde(default)]
    pub outbound_proxy: Option<String>,
    #[serde(default)]
    pub transport: Option<SipTransport>,
    #[serde(default)]
    pub auth_username: Option<String>,
    #[serde(default)]
    pub auth_password: Option<String>,
    #[serde(default)]
    pub health_check_interval_secs: Option<i32>,
    #[serde(default)]
    pub failure_threshold: Option<i32>,
    #[serde(default)]
    pub recovery_threshold: Option<i32>,
    #[serde(default = "default_true")]
    pub is_active: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateGatewayRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub direction: Option<SipTrunkDirection>,
    #[serde(default)]
    pub sip_server: Option<String>,
    #[serde(default)]
    pub outbound_proxy: Option<String>,
    #[serde(default)]
    pub transport: Option<SipTransport>,
    #[serde(default)]
    pub auth_username: Option<String>,
    #[serde(default)]
    pub auth_password: Option<String>,
    #[serde(default)]
    pub health_check_interval_secs: Option<i32>,
    #[serde(default)]
    pub failure_threshold: Option<i32>,
    #[serde(default)]
    pub recovery_threshold: Option<i32>,
    #[serde(default)]
    pub is_active: Option<bool>,
}

fn default_true() -> bool {
    true
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn validate_name(name: &str) -> ApiResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("gateway name is required"));
    }
    if trimmed.len() > 128 {
        return Err(ApiError::bad_request("gateway name exceeds 128 characters"));
    }
    Ok(())
}

async fn trunk_by_name(
    db: &sea_orm::DatabaseConnection,
    name: &str,
) -> ApiResult<Option<TrunkModel>> {
    TrunkEntity::find()
        .filter(TrunkColumn::Name.eq(name))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))
}

async fn create_gateway(
    State(state): State<AppState>,
    Json(req): Json<CreateGatewayRequest>,
) -> ApiResult<(StatusCode, Json<GatewayView>)> {
    validate_name(&req.name)?;
    let db = state.db();

    if trunk_by_name(db, &req.name).await?.is_some() {
        return Err(ApiError::conflict(format!(
            "gateway '{}' already exists",
            req.name
        )));
    }

    let now = Utc::now();
    let am = TrunkActiveModel {
        name: Set(req.name.clone()),
        display_name: Set(normalize_optional_string(req.display_name)),
        direction: Set(req.direction.unwrap_or_default()),
        status: Set(SipTrunkStatus::default()),
        sip_server: Set(normalize_optional_string(req.sip_server)),
        sip_transport: Set(req.transport.unwrap_or_default()),
        outbound_proxy: Set(normalize_optional_string(req.outbound_proxy)),
        auth_username: Set(normalize_optional_string(req.auth_username)),
        auth_password: Set(normalize_optional_string(req.auth_password)),
        is_active: Set(req.is_active),
        register_enabled: Set(false),
        rewrite_hostport: Set(true),
        health_check_interval_secs: Set(req.health_check_interval_secs),
        failure_threshold: Set(req.failure_threshold),
        recovery_threshold: Set(req.recovery_threshold),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((StatusCode::CREATED, Json(GatewayView::from(inserted))))
}

async fn update_gateway(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<UpdateGatewayRequest>,
) -> ApiResult<Json<GatewayView>> {
    let db = state.db();
    let existing = trunk_by_name(db, &name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("gateway '{}' not found", name)))?;

    let mut am: TrunkActiveModel = existing.into();
    if let Some(v) = req.display_name {
        am.display_name = Set(normalize_optional_string(Some(v)));
    }
    if let Some(v) = req.direction {
        am.direction = Set(v);
    }
    if let Some(v) = req.sip_server {
        am.sip_server = Set(normalize_optional_string(Some(v)));
    }
    if let Some(v) = req.outbound_proxy {
        am.outbound_proxy = Set(normalize_optional_string(Some(v)));
    }
    if let Some(v) = req.transport {
        am.sip_transport = Set(v);
    }
    if let Some(v) = req.auth_username {
        am.auth_username = Set(normalize_optional_string(Some(v)));
    }
    if let Some(v) = req.auth_password {
        am.auth_password = Set(normalize_optional_string(Some(v)));
    }
    if let Some(v) = req.health_check_interval_secs {
        am.health_check_interval_secs = Set(Some(v));
    }
    if let Some(v) = req.failure_threshold {
        am.failure_threshold = Set(Some(v));
    }
    if let Some(v) = req.recovery_threshold {
        am.recovery_threshold = Set(Some(v));
    }
    if let Some(v) = req.is_active {
        am.is_active = Set(v);
    }
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(GatewayView::from(updated)))
}

async fn delete_gateway(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let existing = trunk_by_name(db, &name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("gateway '{}' not found", name)))?;

    let referencing = DidEntity::find()
        .filter(DidColumn::TrunkName.eq(name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some(did) = referencing {
        return Err(ApiError::conflict(format!(
            "gateway '{}' is referenced by DID '{}' and cannot be deleted",
            name, did.number
        )));
    }

    sip_trunk::Entity::delete_by_id(existing.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
