//! `/api/v1/dids` — carrier-API DID management (Phase 1, Plan 01 Task 2).
//!
//! This sub-router is a thin JSON adapter over the existing
//! [`crate::models::did::Model`] pure functions. The data layer methods
//! (`Model::upsert`, `Model::get`, `Model::delete`, etc.) are already keyed
//! on `&DatabaseConnection` per the Phase 1 adapter pattern locked in
//! `.planning/phases/01-api-shell-cheap-wrappers/01-CONTEXT.md` §"Adapter
//! Pattern" — no console handler extraction is needed because the model
//! layer is already the shared sink between HTML and JSON handlers.
//!
//! `DidView` is the wire type. `models::did::Model` (the SeaORM row) is
//! NEVER serialized directly, preserving SHELL-04.
//!
//! Error envelope is the shared [`ApiError`] from
//! [`crate::handler::api_v1::error`]. Routes:
//!
//! - `GET    /api/v1/dids`            (paginated list with filters)
//! - `POST   /api/v1/dids`            (create — 201 on success, 409 on duplicate)
//! - `GET    /api/v1/dids/{number}`   (fetch one — URL-decoded `+`)
//! - `PUT    /api/v1/dids/{number}`   (replace — 200 with view)
//! - `DELETE /api/v1/dids/{number}`   (hard delete — 204)

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::config_merge::read_default_country;
use crate::handler::api_v1::common::{Pagination, PaginatedResponse};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::did::{
    self, Column as DidColumn, DidError, Entity as DidEntity, Model as DidModel, NewDid,
    normalize_did,
};

// ---------------------------------------------------------------------------
// Wire types — kept decoupled from SeaORM Model per SHELL-04.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct DidView {
    pub number: String,
    pub trunk_name: Option<String>,
    pub extension_number: Option<String>,
    pub failover_trunk: Option<String>,
    pub label: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<DidModel> for DidView {
    fn from(m: DidModel) -> Self {
        Self {
            number: m.number,
            trunk_name: m.trunk_name,
            extension_number: m.extension_number,
            failover_trunk: m.failover_trunk,
            label: m.label,
            enabled: m.enabled,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct DidListQuery {
    // Pagination fields are inlined rather than `#[serde(flatten)]` from
    // `Pagination` because `serde_urlencoded` (used by axum::Query) does
    // not support flatten across typed fields.
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub page_size: Option<u64>,
    #[serde(default)]
    pub trunk: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub unassigned: Option<bool>,
}

impl DidListQuery {
    fn pagination(&self) -> Pagination {
        Pagination {
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDidRequest {
    pub number: String,
    #[serde(default)]
    pub trunk_name: Option<String>,
    #[serde(default)]
    pub extension_number: Option<String>,
    #[serde(default)]
    pub failover_trunk: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateDidRequest {
    #[serde(default)]
    pub trunk_name: Option<String>,
    #[serde(default)]
    pub extension_number: Option<String>,
    #[serde(default)]
    pub failover_trunk: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

fn default_enabled() -> bool {
    true
}

fn normalize_optional_string(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn did_error_to_api(err: DidError) -> ApiError {
    match err {
        DidError::Empty => ApiError::bad_request("DID number is required"),
        DidError::MissingRegion => ApiError::bad_request(
            "No default country configured; DID must start with + (E.164)",
        ),
        DidError::InvalidNumber(msg) => {
            ApiError::bad_request(format!("Invalid phone number: {msg}"))
        }
        DidError::UnknownCountry(code) => {
            ApiError::bad_request(format!("Unknown country code: {code}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/dids", get(list_dids).post(create_did))
        .route(
            "/dids/{number}",
            get(get_did).put(update_did).delete(delete_did),
        )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_dids(
    State(state): State<AppState>,
    Query(q): Query<DidListQuery>,
) -> ApiResult<Json<PaginatedResponse<DidView>>> {
    let db = state.db();
    let pagination = q.pagination();
    let page_no = pagination.page.max(1);
    let page_size = pagination.limit();

    // Build filter once; reuse it for both count and rows via clone.
    let mut conds = Condition::all();
    if q.unassigned.unwrap_or(false) {
        conds = conds.add(DidColumn::TrunkName.is_null());
    } else if let Some(trunk) = q
        .trunk
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        conds = conds.add(DidColumn::TrunkName.eq(trunk));
    }
    if let Some(search) = q.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let like = format!("%{}%", search);
        conds = conds.add(
            Condition::any()
                .add(DidColumn::Number.like(like.clone()))
                .add(DidColumn::Label.like(like)),
        );
    }

    let paginator = DidEntity::find()
        .filter(conds)
        .order_by_asc(DidColumn::Number)
        .paginate(db, page_size);

    let total = paginator
        .num_items()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let rows = paginator
        .fetch_page(page_no.saturating_sub(1))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(PaginatedResponse::new(
        rows.into_iter().map(DidView::from).collect(),
        page_no,
        page_size,
        total,
    )))
}

async fn create_did(
    State(state): State<AppState>,
    Json(req): Json<CreateDidRequest>,
) -> ApiResult<(StatusCode, Json<DidView>)> {
    let db = state.db();
    let region = read_default_country(db).await;
    let normalized = normalize_did(&req.number, region.as_deref()).map_err(did_error_to_api)?;

    // Duplicate check — engagement semantics: POST is strict create, not upsert.
    match DidModel::get(db, &normalized).await {
        Ok(Some(_)) => {
            return Err(ApiError::conflict(format!(
                "DID {normalized} already exists"
            )));
        }
        Ok(None) => {}
        Err(e) => return Err(ApiError::internal(e.to_string())),
    }

    let new = NewDid {
        number: normalized.clone(),
        trunk_name: normalize_optional_string(&req.trunk_name),
        extension_number: normalize_optional_string(&req.extension_number),
        failover_trunk: normalize_optional_string(&req.failover_trunk),
        label: normalize_optional_string(&req.label),
        enabled: req.enabled,
    };

    DidModel::upsert(db, new)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let row = DidModel::get(db, &normalized)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::internal("row vanished after insert"))?;

    refresh_did_index(&state).await;

    Ok((StatusCode::CREATED, Json(DidView::from(row))))
}

/// Refresh the in-memory DID index after a DB mutation so the routing
/// matcher sees the change immediately. Mirrors the auto-reload behavior
/// of the console handlers (`src/console/handlers/did.rs`). Logs a warning
/// on failure but never errors the parent request — the DB write succeeded
/// and the worst case is a stale index until the next manual reload.
async fn refresh_did_index(state: &AppState) {
    state
        .sip_server()
        .inner
        .data_context
        .reload_did_index()
        .await;
}

async fn get_did(
    State(state): State<AppState>,
    Path(number): Path<String>,
) -> ApiResult<Json<DidView>> {
    let db = state.db();
    let region = read_default_country(db).await;
    let normalized = normalize_did(&number, region.as_deref())
        .map_err(|_| ApiError::not_found(format!("DID {number} not found")))?;

    let row = DidModel::get(db, &normalized)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("DID {normalized} not found")))?;

    Ok(Json(DidView::from(row)))
}

async fn update_did(
    State(state): State<AppState>,
    Path(number): Path<String>,
    Json(req): Json<UpdateDidRequest>,
) -> ApiResult<Json<DidView>> {
    let db = state.db();
    let region = read_default_country(db).await;
    let normalized = normalize_did(&number, region.as_deref())
        .map_err(|_| ApiError::not_found(format!("DID {number} not found")))?;

    let existing = DidModel::get(db, &normalized)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("DID {normalized} not found")))?;

    // PUT semantics: fields left unset in the request body are REPLACED with
    // None (not left as-is). Callers that only want to change one field
    // should read the current state first, then PUT the full desired state.
    // PATCH semantics (merge) can be added later if needed.
    let new = NewDid {
        number: normalized.clone(),
        trunk_name: normalize_optional_string(&req.trunk_name),
        extension_number: normalize_optional_string(&req.extension_number),
        failover_trunk: normalize_optional_string(&req.failover_trunk),
        label: normalize_optional_string(&req.label),
        enabled: req.enabled.unwrap_or(existing.enabled),
    };

    DidModel::upsert(db, new)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let row = DidModel::get(db, &normalized)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::internal("row vanished after update"))?;

    refresh_did_index(&state).await;

    Ok(Json(DidView::from(row)))
}

async fn delete_did(
    State(state): State<AppState>,
    Path(number): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let region = read_default_country(db).await;

    // Unparseable numbers cannot match any stored row; return 404 rather
    // than silently 204. Strict delete semantics per CARRIER-API spec.
    let normalized = normalize_did(&number, region.as_deref())
        .map_err(|_| ApiError::not_found(format!("DID {number} not found")))?;

    match DidModel::get(db, &normalized).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err(ApiError::not_found(format!(
                "DID {normalized} not found"
            )));
        }
        Err(e) => return Err(ApiError::internal(e.to_string())),
    }

    did::Model::delete(db, &normalized)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    refresh_did_index(&state).await;

    Ok(StatusCode::NO_CONTENT)
}
