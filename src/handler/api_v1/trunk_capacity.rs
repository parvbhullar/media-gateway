//! `/api/v1/trunks/{name}/capacity` — TSUB-04 CRUD half + TSUB-07
//! response shape (Phase 5 Plan 05-02).
//!
//! Backed by `supersip_trunk_capacity` (Plan 05-01 schema). UNIQUE FK on
//! `trunk_group_id` per D-01: at most one capacity row per trunk group.
//! `max_calls` and `max_cps` are NULL-able (D-04: NULL = unlimited).
//!
//! GET shape (D-04):
//!   `{max_calls, max_cps, current_active, current_cps_rate}`
//!
//! `current_active` and `current_cps_rate` are placeholder zeros in this
//! plan. Plan 05-04 wires them to the registry snapshot + token-bucket
//! state respectively.
//!
//! PUT body (D-05): both fields optional; 0 is rejected with 400 (use
//! null to express "unlimited").
//!
//! Wave-2 file ownership: this file is the *only* source file owned by
//! Plan 05-02. `mod.rs` was edited by Plan 05-01 to register
//! `.merge(trunk_capacity::router())`; the router-fn signature here MUST
//! remain `pub fn router() -> Router<AppState>` so that wiring continues
//! to compile.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::trunk_capacity::{
    self, Column as TcapColumn, Entity as TcapEntity, Model as TcapModel,
};
use crate::models::trunk_group::{
    Column as TrunkGroupColumn, Entity as TrunkGroupEntity,
};

// ─── Wire types (D-04, D-05 — SHELL-04: never serialize Model directly) ─

#[derive(Debug, Serialize)]
pub struct TrunkCapacityView {
    pub max_calls: Option<u32>,
    pub max_cps: Option<u32>,
    pub current_active: u32,
    pub current_cps_rate: u32,
}

impl TrunkCapacityView {
    /// Build a view from a stored row, with live counters from the registry
    /// and the per-trunk-group capacity gate (Plan 05-04, D-02 + D-04).
    fn from_row(row: &TcapModel, current_active: u32, current_cps_rate: u32) -> Self {
        Self {
            max_calls: row.max_calls.and_then(|v| u32::try_from(v).ok()),
            max_cps: row.max_cps.and_then(|v| u32::try_from(v).ok()),
            current_active,
            current_cps_rate,
        }
    }

    /// Default shape returned when the trunk has no capacity row yet
    /// (D-01: missing row == unlimited == both fields null). Live counters
    /// are still surfaced so operators can see in-flight calls even before
    /// they configure max_calls.
    fn defaults(current_active: u32, current_cps_rate: u32) -> Self {
        Self {
            max_calls: None,
            max_cps: None,
            current_active,
            current_cps_rate,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutTrunkCapacityRequest {
    pub max_calls: Option<u32>,
    pub max_cps: Option<u32>,
}

// ─── Validation (D-05) ──────────────────────────────────────────────────

/// D-05: 0 is rejected with a message that names the contract for null.
/// Negative integers can't reach this layer because the field type is
/// `u32` and serde fails the deserialization with a 400 envelope.
fn validate_capacity(req: &PutTrunkCapacityRequest) -> ApiResult<()> {
    if matches!(req.max_calls, Some(0)) {
        return Err(ApiError::bad_request(
            "max_calls must be > 0; use null for unlimited",
        ));
    }
    if matches!(req.max_cps, Some(0)) {
        return Err(ApiError::bad_request(
            "max_cps must be > 0; use null for unlimited",
        ));
    }
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────

/// Resolve `{name}` → `trunk_group_id`, 404 if missing. Verbatim copy of
/// the Phase 3 sub-resource pattern (`trunk_credentials.rs`).
async fn lookup_trunk_group_id(
    db: &sea_orm::DatabaseConnection,
    name: &str,
) -> ApiResult<i64> {
    let group = TrunkGroupEntity::find()
        .filter(TrunkGroupColumn::Name.eq(name))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("trunk group '{}' not found", name))
        })?;
    Ok(group.id)
}

async fn find_capacity_row(
    db: &sea_orm::DatabaseConnection,
    trunk_group_id: i64,
) -> ApiResult<Option<TcapModel>> {
    TcapEntity::find()
        .filter(TcapColumn::TrunkGroupId.eq(trunk_group_id))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))
}

// ─── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/trunks/{name}/capacity",
        get(get_capacity).put(put_capacity),
    )
}

// ─── Handlers ────────────────────────────────────────────────────────────

/// GET /trunks/{name}/capacity — D-04 shape. 404 if parent trunk missing;
/// otherwise return the stored row mapped to wire shape, or defaults
/// (all-null + zeros) when no row exists yet.
async fn get_capacity(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<TrunkCapacityView>> {
    let db = state.db();
    let trunk_group_id = lookup_trunk_group_id(db, &name).await?;

    // Phase 5 Plan 05-04 (D-02, D-04, TSUB-07):
    // current_active counts entries in the registry whose trunk_group_name
    // matches the requested trunk; current_cps_rate is read from the live
    // token bucket (tokens consumed in the current 1s window).
    let current_active = state
        .sip_server()
        .inner
        .active_call_registry
        .count_active_for_trunk(&name);
    let current_cps_rate = state
        .sip_server()
        .inner
        .trunk_capacity_state
        .snapshot_cps_rate(trunk_group_id);

    let view = match find_capacity_row(db, trunk_group_id).await? {
        Some(row) => TrunkCapacityView::from_row(&row, current_active, current_cps_rate),
        None => TrunkCapacityView::defaults(current_active, current_cps_rate),
    };
    Ok(Json(view))
}

/// PUT /trunks/{name}/capacity — D-05 upsert. 400 on 0; 404 on missing
/// parent. UNIQUE FK on `trunk_group_id` guarantees at most one row per
/// trunk group; we pre-fetch and dispatch update-vs-insert manually so
/// the response is the post-write view (round-trip in one call).
async fn put_capacity(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<PutTrunkCapacityRequest>,
) -> ApiResult<Json<TrunkCapacityView>> {
    let db = state.db();
    validate_capacity(&req)?;
    let trunk_group_id = lookup_trunk_group_id(db, &name).await?;

    // u32 → i32 cast: domain max for max_calls/max_cps is well below i32::MAX.
    // Cap defensively so a malicious 2^31..2^32-1 input doesn't wrap negative.
    let max_calls_db = req
        .max_calls
        .map(|v| i32::try_from(v).unwrap_or(i32::MAX));
    let max_cps_db = req
        .max_cps
        .map(|v| i32::try_from(v).unwrap_or(i32::MAX));

    let now = Utc::now();
    let stored = match find_capacity_row(db, trunk_group_id).await? {
        Some(row) => {
            let mut am: trunk_capacity::ActiveModel = row.into();
            am.max_calls = Set(max_calls_db);
            am.max_cps = Set(max_cps_db);
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?
        }
        None => {
            let am = trunk_capacity::ActiveModel {
                trunk_group_id: Set(trunk_group_id),
                max_calls: Set(max_calls_db),
                max_cps: Set(max_cps_db),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };
            am.insert(db)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?
        }
    };

    // Phase 5 Plan 05-04: PUT also propagates the new limits into the live
    // capacity gate (so an operator-issued change takes effect immediately
    // for in-flight admission checks) and re-surfaces the same live
    // current_active / current_cps_rate snapshot the GET handler returns.
    let new_max_calls = req.max_calls;
    let new_max_cps = req.max_cps;
    state
        .sip_server()
        .inner
        .trunk_capacity_state
        .update_limits(trunk_group_id, new_max_calls, new_max_cps);

    let current_active = state
        .sip_server()
        .inner
        .active_call_registry
        .count_active_for_trunk(&name);
    let current_cps_rate = state
        .sip_server()
        .inner
        .trunk_capacity_state
        .snapshot_cps_rate(trunk_group_id);

    Ok(Json(TrunkCapacityView::from_row(
        &stored,
        current_active,
        current_cps_rate,
    )))
}
