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
use tracing::warn;

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::did::{Column as DidColumn, Entity as DidEntity};
use crate::models::kind_schemas::{self, KindValidationError};
use crate::models::sip_trunk::{
    self, ActiveModel as TrunkActiveModel, Column as TrunkColumn, Entity as TrunkEntity,
    Model as TrunkModel, SipTransport, SipTrunkConfig, SipTrunkDirection, SipTrunkStatus,
};
use crate::proxy::gateway_health::ProbeOutcome;
use crate::proxy::health_probers;

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
    pub description: Option<String>,
    pub direction: String,
    /// Legacy SIP convenience field (populated only when `kind == "sip"`).
    pub proxy_addr: Option<String>,
    /// Legacy SIP convenience field (populated only when `kind == "sip"`).
    pub transport: Option<String>,
    pub status: String,
    pub is_active: bool,
    pub max_concurrent: Option<i32>,
    pub max_cps: Option<i32>,
    pub allowed_ips: Option<JsonValue>,
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
            description: m.description,
            direction: m.direction.as_str().to_string(),
            proxy_addr,
            transport,
            status: m.status.as_str().to_string(),
            is_active: m.is_active,
            max_concurrent: m.max_concurrent,
            max_cps: m.max_cps,
            allowed_ips: m.allowed_ips,
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
        .route("/gateways/{name}/promote", post(promote_file_gateway))
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
    // Probe via the per-kind health-probe registry (same path as the
    // background gateway-health monitor) so non-SIP kinds — webrtc, livekit
    // — are tested with their own prober instead of being forced through the
    // SIP OPTIONS probe (which would always fail to deserialize their
    // `kind_config`). Kinds with no registered prober (e.g. external_media,
    // which spawns a local sidecar and has no remote endpoint to reach)
    // return a clear, non-error explanation rather than a misleading failure.
    let outcome = match health_probers::lookup(&row.kind) {
        Some(prober) => prober.probe(&row, Duration::from_secs(5)).await,
        None => ProbeOutcome {
            ok: false,
            latency_ms: 0,
            detail: format!(
                "kind '{}' has no liveness probe (not remotely reachable)",
                row.kind
            ),
        },
    };
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
    pub description: Option<String>,
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
    /// Soft limit on concurrent calls through this gateway. null = unlimited.
    #[serde(default)]
    pub max_concurrent: Option<i32>,
    /// Soft limit on calls per second (CPS). null = unlimited.
    #[serde(default)]
    pub max_cps: Option<i32>,
    /// JSON array of IP addresses / CIDR blocks allowed to send inbound SIP
    /// to this trunk. null = accept from any source.
    #[serde(default)]
    pub allowed_ips: Option<JsonValue>,
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
    pub description: Option<String>,
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
    pub max_concurrent: Option<i32>,
    #[serde(default)]
    pub max_cps: Option<i32>,
    #[serde(default)]
    pub allowed_ips: Option<JsonValue>,
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

/// Refresh the in-memory trunks snapshot + regenerate trunks TOML after any
/// gateway mutation so the matcher sees changes immediately. Mirrors
/// `refresh_routes_index` in `routes.rs`. Errors are logged but don't fail
/// the parent request: the DB write succeeded; the worst case is a stale
/// snapshot until the next manual reload.
async fn refresh_trunks_index(state: &AppState) {
    let config_override = state.config_path.as_ref().and_then(|path| {
        crate::config::Config::load(path)
            .ok()
            .map(|cfg| std::sync::Arc::new(cfg.proxy))
    });
    if let Err(e) = state
        .sip_server()
        .inner
        .data_context
        .reload_trunks(true, config_override)
        .await
    {
        warn!(error = %e, "auto-reload of trunks failed after gateway mutation");
    }
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
        description: Set(normalize_optional_string(req.description)),
        direction: Set(req.direction.unwrap_or_default()),
        status: Set(SipTrunkStatus::default()),
        is_active: Set(req.is_active),
        max_concurrent: Set(req.max_concurrent),
        max_cps: Set(req.max_cps),
        allowed_ips: Set(req.allowed_ips),
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

    refresh_trunks_index(&state).await;

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
    if let Some(v) = req.description {
        am.description = Set(normalize_optional_string(Some(v)));
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
    if req.max_concurrent.is_some() {
        am.max_concurrent = Set(req.max_concurrent);
    }
    if req.max_cps.is_some() {
        am.max_cps = Set(req.max_cps);
    }
    if req.allowed_ips.is_some() {
        am.allowed_ips = Set(req.allowed_ips);
    }
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    refresh_trunks_index(&state).await;

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

    refresh_trunks_index(&state).await;

    Ok(StatusCode::NO_CONTENT)
}

/// Promote a file-sourced trunk into the DB.
///
/// Workflow:
///   1. Look up the trunk in the in-memory snapshot. Reject if it's not
///      found, not file-sourced, or sourced from a `*.generated.toml`
///      (auto-emitted mirror of the DB).
///   2. Reject if a DB row with the same name already exists.
///   3. Build `kind` + `kind_config_json` from the in-memory `TrunkConfig`.
///      For `kind = "sip"`, repack the legacy top-level fields into
///      `SipTrunkConfig`. For other kinds, pass the existing `kind_config`
///      blob through (it was loaded from the file as-is).
///   4. Insert the DB row.
///   5. Edit the source file to remove the trunk's section.
///   6. Refresh the in-memory trunks index so the now-DB row shows up in
///      the editable list and the file mirror disappears.
///
/// Shared by the bearer-auth `/api/v1/gateways/{name}/promote` endpoint
/// (below) and the session-auth `/console/sip-trunk/promote/{name}`
/// endpoint (`src/console/handlers/sip_trunk.rs`).
pub async fn promote_file_gateway_inner(
    state: &AppState,
    name: &str,
) -> ApiResult<TrunkModel> {
    validate_name(name)?;
    let db = state.db();

    let snapshot = state
        .sip_server()
        .inner
        .data_context
        .trunks_snapshot();
    let trunk = snapshot
        .get(name)
        .ok_or_else(|| ApiError::not_found(format!("file trunk '{}' not found in snapshot", name)))?
        .clone();

    let source_path = match &trunk.origin {
        crate::proxy::routing::ConfigOrigin::File(p) => p.clone(),
        _ => {
            return Err(ApiError::bad_request(format!(
                "trunk '{}' is not file-sourced",
                name
            )));
        }
    };
    let is_generated = std::path::Path::new(&source_path)
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.ends_with(".generated.toml"));
    if is_generated {
        return Err(ApiError::bad_request(format!(
            "trunk '{}' is sourced from {} — that file is auto-generated from \
             the DB and cannot be promoted",
            name, source_path
        )));
    }

    if trunk_by_name(db, name).await?.is_some() {
        return Err(ApiError::conflict(format!(
            "gateway '{}' already exists in DB; cannot promote duplicate from file",
            name
        )));
    }

    let kind = trunk.kind.clone();
    let kind_config_json: JsonValue = if kind == "sip" {
        // Repack the legacy top-level SIP fields exposed on `TrunkConfig`
        // into the `SipTrunkConfig` shape stored in `rustpbx_trunks.kind_config`.
        let dest_host = trunk
            .dest
            .strip_prefix("sip:")
            .or_else(|| trunk.dest.strip_prefix("sips:"))
            .unwrap_or(&trunk.dest)
            .to_string();
        let transport = match trunk.transport.as_deref() {
            Some("tcp") => SipTransport::Tcp,
            Some("tls") => SipTransport::Tls,
            _ => SipTransport::Udp,
        };
        let sip_cfg = SipTrunkConfig {
            sip_server: Some(dest_host),
            sip_transport: transport,
            outbound_proxy: None,
            auth_username: trunk.username.clone(),
            auth_password: trunk.password.clone(),
            register_enabled: trunk.register_enabled.unwrap_or(false),
            register_expires: trunk.register_expires.map(|v| v as i32),
            register_extra_headers: None,
            rewrite_hostport: trunk.rewrite_hostport,
            did_numbers: None,
            incoming_from_user_prefix: trunk.incoming_from_user_prefix.clone(),
            incoming_to_user_prefix: trunk.incoming_to_user_prefix.clone(),
            default_route_label: None,
            billing_snapshot: None,
            analytics: None,
            carrier: None,
        };
        serde_json::to_value(&sip_cfg).map_err(|e| ApiError::internal(e.to_string()))?
    } else {
        // Non-SIP kinds carry their full kind_config inside the in-memory
        // TrunkConfig. Pass it through unchanged.
        trunk
            .kind_config
            .clone()
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "trunk '{}' (kind={}) is missing kind_config — file is malformed",
                    name, kind
                ))
            })?
    };

    kind_schemas::validate(&kind, &kind_config_json).map_err(map_kind_validation_err)?;

    let direction = match trunk.direction {
        Some(crate::proxy::routing::TrunkDirection::Inbound) => SipTrunkDirection::Inbound,
        Some(crate::proxy::routing::TrunkDirection::Outbound) => SipTrunkDirection::Outbound,
        Some(crate::proxy::routing::TrunkDirection::Bidirectional) | None => {
            SipTrunkDirection::Bidirectional
        }
    };

    let now = Utc::now();
    let am = TrunkActiveModel {
        name: Set(name.to_string()),
        kind: Set(kind),
        display_name: Set(None),
        direction: Set(direction),
        status: Set(SipTrunkStatus::default()),
        is_active: Set(!trunk.disabled.unwrap_or(false)),
        health_check_interval_secs: Set(None),
        failure_threshold: Set(None),
        recovery_threshold: Set(None),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        kind_config: Set(kind_config_json),
        ..Default::default()
    };
    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Remove the trunk's section from the source file. Failure here is a
    // soft error: the DB row already exists, so on the next reload it'll
    // win over the file (DB has higher precedence in the loader). We log
    // but still return success so the user gets visible feedback.
    if let Err(e) = crate::proxy::data_file_ops::remove_trunk_section_from_file(
        std::path::Path::new(&source_path),
        name,
    ) {
        warn!(
            error = %e,
            trunk = %name,
            file = %source_path,
            "promote: failed to remove trunk section from source file; DB row inserted, \
             file unchanged — operator should clean up manually"
        );
    }

    refresh_trunks_index(state).await;

    Ok(inserted)
}

/// Thin axum wrapper exposing `promote_file_gateway_inner` at
/// `POST /api/v1/gateways/{name}/promote` (bearer-auth path).
async fn promote_file_gateway(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<(StatusCode, Json<GatewayView>)> {
    let inserted = promote_file_gateway_inner(&state, &name).await?;
    Ok((StatusCode::CREATED, Json(GatewayView::from(inserted))))
}
