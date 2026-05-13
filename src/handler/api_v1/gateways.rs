//! `/api/v1/gateways` and `/api/v1/diagnostics/trunk-test` endpoints.
//!
//! Wave 2B refactor: the underlying `sip_trunk` model has been unified into
//! `trunk` with a `kind` discriminator and a JSON `kind_config` blob holding
//! all kind-specific config (see Phase 8a of the
//! `imperative-sauteeing-cake` plan). The wire path
//! `/api/v1/gateways` is unchanged; `GatewayView` keeps its existing
//! top-level SIP fields for back-compat (populated only when `kind == "sip"`)
//! and gains `kind` + `kind_config` fields so WebRTC trunks have a wire shape.
//!
//! Tolerant input: when `kind` is absent on POST/PUT, the handler treats the
//! request as a legacy SIP trunk and folds the top-level SIP fields into a
//! `SipTrunkConfig`. WebRTC writes use `kind = "webrtc"` and a nested
//! `kind_config` object.

use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::did::{Column as DidColumn, Entity as DidEntity};
use crate::models::kind_schemas::{self, KindValidationError};
use crate::models::sip_trunk::{
    self, ActiveModel as TrunkActiveModel, Column as TrunkColumn, Entity as TrunkEntity,
    Model as TrunkModel, SipTransport, SipTrunkConfig, SipTrunkDirection, SipTrunkStatus,
};
use crate::proxy::gateway_health::probe_trunk;

/// Map a `KindValidationError` into the file's existing `ApiError` envelope.
/// All variants surface as HTTP 400 with the error message carried through;
/// `Invalid { kind, message }` preserves any field-attributed detail that
/// the underlying serde / `validate()` call produced.
fn map_kind_validation_err(e: KindValidationError) -> ApiError {
    ApiError::bad_request(e.to_string())
}

#[derive(Debug, Serialize)]
pub struct GatewayView {
    pub name: String,
    pub kind: String,
    pub display_name: Option<String>,
    pub direction: String,
    /// Legacy SIP convenience field (populated only when `kind == "sip"`).
    pub proxy_addr: Option<String>,
    /// Legacy SIP convenience field (populated only when `kind == "sip"`).
    pub transport: Option<String>,
    pub status: String,
    pub is_active: bool,
    pub last_health_check_at: Option<chrono::DateTime<chrono::Utc>>,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub failure_threshold: i32,
    pub recovery_threshold: i32,
    pub health_check_interval_secs: i32,
    /// Full kind-specific config blob (per Phase 8a wire shape).
    pub kind_config: JsonValue,
}

impl GatewayView {
    fn from_model(m: TrunkModel) -> Self {
        let (proxy_addr, transport) = match m.kind.as_str() {
            "sip" => match m.sip() {
                Ok(cfg) => (
                    cfg.outbound_proxy.clone().or(cfg.sip_server.clone()),
                    Some(cfg.sip_transport.as_str().to_string()),
                ),
                Err(_) => (None, None),
            },
            _ => (None, None),
        };
        Self {
            name: m.name,
            kind: m.kind.clone(),
            display_name: m.display_name,
            direction: m.direction.as_str().to_string(),
            proxy_addr,
            transport,
            status: m.status.as_str().to_string(),
            is_active: m.is_active,
            last_health_check_at: m.last_health_check_at,
            consecutive_failures: m.consecutive_failures,
            consecutive_successes: m.consecutive_successes,
            failure_threshold: m.failure_threshold.unwrap_or(3),
            recovery_threshold: m.recovery_threshold.unwrap_or(2),
            health_check_interval_secs: m.health_check_interval_secs.unwrap_or(30),
            kind_config: m.kind_config,
        }
    }
}

impl From<TrunkModel> for GatewayView {
    fn from(m: TrunkModel) -> Self {
        Self::from_model(m)
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
// Write routes (Phase 8a — tolerant input, strict output)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateGatewayRequest {
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub direction: Option<SipTrunkDirection>,
    // Legacy top-level SIP fields (folded into kind_config when kind == "sip")
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
    /// Required for `kind != "sip"`. For SIP, optional; if present it is
    /// merged with the legacy top-level fields (legacy fields win on
    /// conflict for back-compat).
    #[serde(default)]
    pub kind_config: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateGatewayRequest {
    #[serde(default)]
    pub kind: Option<String>,
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
    #[serde(default)]
    pub kind_config: Option<JsonValue>,
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

/// Build a `(kind, kind_config_json)` tuple from a create request, honouring
/// the tolerant-input rule (legacy top-level SIP fields fold into a SIP
/// `kind_config`).
fn build_kind_and_config_for_create(req: &CreateGatewayRequest) -> ApiResult<(String, JsonValue)> {
    let kind = req.kind.clone().unwrap_or_else(|| "sip".to_string());
    // SIP retains the legacy top-level fold-in path; non-SIP kinds use the
    // request's `kind_config` blob verbatim. Final validation for every kind
    // runs through `kind_schemas::validate` so adding a new kind only
    // requires registering a new validator.
    let kind_config_json: JsonValue = if kind == "sip" {
        let mut cfg: SipTrunkConfig = match &req.kind_config {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| ApiError::bad_request(format!("invalid sip kind_config: {e}")))?,
            None => SipTrunkConfig::default(),
        };
        // Legacy top-level fields override (back-compat).
        if let Some(v) = normalize_optional_string(req.sip_server.clone()) {
            cfg.sip_server = Some(v);
        }
        if let Some(v) = normalize_optional_string(req.outbound_proxy.clone()) {
            cfg.outbound_proxy = Some(v);
        }
        if let Some(t) = req.transport {
            cfg.sip_transport = t;
        }
        if let Some(v) = normalize_optional_string(req.auth_username.clone()) {
            cfg.auth_username = Some(v);
        }
        if let Some(v) = normalize_optional_string(req.auth_password.clone()) {
            cfg.auth_password = Some(v);
        }
        serde_json::to_value(&cfg)
            .map_err(|e| ApiError::internal(format!("serialize sip config: {e}")))?
    } else {
        req.kind_config.clone().ok_or_else(|| {
            ApiError::bad_request(format!("{kind} trunks require kind_config"))
        })?
    };

    kind_schemas::validate(&kind, &kind_config_json).map_err(map_kind_validation_err)?;
    Ok((kind, kind_config_json))
}

/// Build the updated `(kind, kind_config_json)` for a PUT by merging the
/// request on top of the existing row's stored config. Legacy SIP top-level
/// fields fold into the SIP config; for webrtc, the request must supply a
/// full `kind_config` object (replace semantics).
fn build_kind_and_config_for_update(
    existing: &TrunkModel,
    req: &UpdateGatewayRequest,
) -> ApiResult<(String, JsonValue)> {
    let kind = req
        .kind
        .clone()
        .unwrap_or_else(|| existing.kind.clone());
    // SIP retains its legacy fold-in / merge-on-existing path; other kinds
    // use the request blob (or fall back to the stored blob when the kind
    // is unchanged). Final validation runs through `kind_schemas::validate`.
    let kind_config_json: JsonValue = if kind == "sip" {
        let mut cfg: SipTrunkConfig = if existing.kind == "sip" {
            existing
                .sip()
                .map_err(|e| ApiError::internal(e.to_string()))?
        } else if let Some(v) = &req.kind_config {
            serde_json::from_value(v.clone())
                .map_err(|e| ApiError::bad_request(format!("invalid sip kind_config: {e}")))?
        } else {
            SipTrunkConfig::default()
        };
        // Apply request-supplied kind_config (replace) over existing.
        if let Some(v) = &req.kind_config {
            cfg = serde_json::from_value(v.clone())
                .map_err(|e| ApiError::bad_request(format!("invalid sip kind_config: {e}")))?;
        }
        // Legacy top-level fields override.
        if let Some(v) = req.sip_server.clone() {
            cfg.sip_server = normalize_optional_string(Some(v));
        }
        if let Some(v) = req.outbound_proxy.clone() {
            cfg.outbound_proxy = normalize_optional_string(Some(v));
        }
        if let Some(t) = req.transport {
            cfg.sip_transport = t;
        }
        if let Some(v) = req.auth_username.clone() {
            cfg.auth_username = normalize_optional_string(Some(v));
        }
        if let Some(v) = req.auth_password.clone() {
            cfg.auth_password = normalize_optional_string(Some(v));
        }
        serde_json::to_value(&cfg)
            .map_err(|e| ApiError::internal(format!("serialize sip config: {e}")))?
    } else {
        req.kind_config
            .clone()
            .or_else(|| {
                if existing.kind == kind {
                    Some(existing.kind_config.clone())
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                ApiError::bad_request(format!("{kind} trunks require kind_config"))
            })?
    };

    kind_schemas::validate(&kind, &kind_config_json).map_err(map_kind_validation_err)?;
    Ok((kind, kind_config_json))
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

    let (kind, kind_config) = build_kind_and_config_for_create(&req)?;

    let now = Utc::now();
    let am = TrunkActiveModel {
        name: Set(req.name.clone()),
        kind: Set(kind),
        display_name: Set(normalize_optional_string(req.display_name)),
        direction: Set(req.direction.unwrap_or_default()),
        status: Set(SipTrunkStatus::default()),
        is_active: Set(req.is_active),
        health_check_interval_secs: Set(req.health_check_interval_secs),
        failure_threshold: Set(req.failure_threshold),
        recovery_threshold: Set(req.recovery_threshold),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        kind_config: Set(kind_config),
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

    let (kind, kind_config) = build_kind_and_config_for_update(&existing, &req)?;

    let mut am: TrunkActiveModel = existing.into();
    am.kind = Set(kind);
    am.kind_config = Set(kind_config);
    if let Some(v) = req.display_name {
        am.display_name = Set(normalize_optional_string(Some(v)));
    }
    if let Some(v) = req.direction {
        am.direction = Set(v);
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
