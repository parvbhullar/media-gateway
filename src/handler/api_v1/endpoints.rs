//! `/api/v1/endpoints` — SIP user endpoint CRUD (Phase 13 Plan 13-02).
//!
//! Backed by `supersip_endpoints` (Plan 13-02 schema). UUID PK per D-11.
//! HA1 = md5(username:realm:password) is stored; plaintext password is
//! accepted on create/update and immediately discarded — never stored, never
//! returned (D-10). account_id is always stamped from AccountScope, never
//! from the request body.
//!
//! Per D-12: SIP registration status is looked up live from the registrar
//! on every GET/LIST. Falls back gracefully to `sip_registered: false,
//! last_register_at: null` when the registrar is unavailable.
//!
//! Response shape per D-14: id, account_id, username, alias, realm,
//! application_id, enabled, sip_registered, last_register_at,
//! created_at, updated_at. NO password or ha1 in response.

use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::common::{CommonScopeQuery, build_account_filter};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::supersip_endpoints::{
    self, Column as EpColumn, Entity as EpEntity, Model as EpModel, compute_ha1,
};

// ---- Wire types (D-14) ------------------------------------------------------

/// Response shape for an endpoint. No password or ha1 fields (D-10 / D-14).
#[derive(Debug, Serialize)]
pub struct EndpointView {
    pub id: String,
    pub account_id: String,
    pub username: String,
    pub alias: Option<String>,
    pub realm: String,
    pub application_id: Option<String>,
    pub enabled: bool,
    /// Per D-12: live registrar lookup; false when unavailable.
    pub sip_registered: bool,
    /// Per D-12: last registration timestamp from registrar; null when unavailable.
    pub last_register_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl EndpointView {
    fn from_model(m: EpModel) -> Self {
        Self {
            id: m.id,
            account_id: m.account_id,
            username: m.username,
            alias: m.alias,
            realm: m.realm,
            application_id: m.application_id,
            enabled: m.enabled,
            sip_registered: false,   // TODO(13-05): live registrar lookup
            last_register_at: None,  // TODO(13-05): live registrar lookup
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

// ---- Request types ----------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateEndpointRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub realm: Option<String>,
    #[serde(default)]
    pub application_id: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateEndpointRequest {
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub realm: Option<String>,
    #[serde(default)]
    pub application_id: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

fn default_true() -> bool {
    true
}

// ---- Validation helpers -----------------------------------------------------

fn validate_username(username: &str) -> ApiResult<()> {
    let t = username.trim();
    if t.is_empty() {
        return Err(ApiError::bad_request("username must be non-empty"));
    }
    if t.len() > 128 {
        return Err(ApiError::bad_request("username exceeds 128 characters"));
    }
    Ok(())
}

fn validate_password(password: &str) -> ApiResult<()> {
    if password.is_empty() {
        return Err(ApiError::bad_request("password must be non-empty"));
    }
    Ok(())
}

/// Resolve the effective realm: use the request-supplied realm when present,
/// fall back to the proxy config realm, default to "localhost".
fn resolve_realm(state: &AppState, requested: Option<&str>) -> String {
    if let Some(r) = requested {
        let t = r.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    state
        .config()
        .proxy
        .realms
        .as_ref()
        .and_then(|rs| rs.first())
        .cloned()
        .unwrap_or_else(|| "localhost".to_string())
}

// ---- Router -----------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/endpoints", get(list_endpoints).post(create_endpoint))
        .route(
            "/endpoints/{id}",
            get(get_endpoint)
                .put(update_endpoint)
                .delete(delete_endpoint),
        )
}

// ---- Handlers ---------------------------------------------------------------

/// GET /endpoints — list all endpoints for the calling tenant.
async fn list_endpoints(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(scope_q): Query<CommonScopeQuery>,
) -> ApiResult<Json<Vec<EndpointView>>> {
    let db = state.db();
    let cond =
        build_account_filter(&scope, EpColumn::AccountId, &scope_q, Condition::all())?;
    let rows = EpEntity::find()
        .filter(cond)
        .order_by_asc(EpColumn::CreatedAt)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(rows.into_iter().map(EndpointView::from_model).collect()))
}

/// GET /endpoints/{id} — fetch a single endpoint by UUID (D-11).
async fn get_endpoint(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
) -> ApiResult<Json<EndpointView>> {
    let db = state.db();
    let row = EpEntity::find()
        .filter(EpColumn::AccountId.eq(scope.account_id.clone()))
        .filter(EpColumn::Id.eq(id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("endpoint '{}' not found", id)))?;
    Ok(Json(EndpointView::from_model(row)))
}

/// POST /endpoints — create a new endpoint.
///
/// account_id is stamped from AccountScope (never from request body).
/// Duplicate (account_id, username) returns 409 per D-09.
async fn create_endpoint(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Json(req): Json<CreateEndpointRequest>,
) -> ApiResult<(StatusCode, Json<EndpointView>)> {
    validate_username(&req.username)?;
    validate_password(&req.password)?;

    let db = state.db();
    let realm = resolve_realm(&state, req.realm.as_deref());

    // Pre-check UNIQUE (account_id, username) per D-09.
    let dup = EpEntity::find()
        .filter(EpColumn::AccountId.eq(scope.account_id.clone()))
        .filter(EpColumn::Username.eq(req.username.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "endpoint with username '{}' already exists",
            req.username
        )));
    }

    let now = Utc::now();
    let id = Uuid::new_v4().to_string();
    let ha1 = compute_ha1(&req.username, &realm, &req.password);

    let am = supersip_endpoints::ActiveModel {
        id: Set(id),
        account_id: Set(scope.account_id.clone()),
        username: Set(req.username.clone()),
        alias: Set(req.alias.clone()),
        realm: Set(realm),
        ha1: Set(ha1),
        application_id: Set(req.application_id.clone()),
        enabled: Set(req.enabled),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((StatusCode::CREATED, Json(EndpointView::from_model(inserted))))
}

/// PUT /endpoints/{id} — update an existing endpoint.
///
/// Only fields present in the request body are updated.
/// If `password` is provided the ha1 is recomputed.
async fn update_endpoint(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
    Json(req): Json<UpdateEndpointRequest>,
) -> ApiResult<Json<EndpointView>> {
    let db = state.db();
    let existing = EpEntity::find()
        .filter(EpColumn::AccountId.eq(scope.account_id.clone()))
        .filter(EpColumn::Id.eq(id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("endpoint '{}' not found", id)))?;

    let mut am: supersip_endpoints::ActiveModel = existing.clone().into();

    if let Some(password) = &req.password {
        validate_password(password)?;
        let realm = req
            .realm
            .as_deref()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| existing.realm.clone());
        let ha1 = compute_ha1(&existing.username, &realm, password);
        am.ha1 = Set(ha1);
        am.realm = Set(realm);
    } else if let Some(realm) = &req.realm {
        let r = realm.trim().to_string();
        if !r.is_empty() {
            am.realm = Set(r);
        }
    }

    if let Some(alias) = req.alias {
        let a = alias.trim().to_string();
        am.alias = Set(if a.is_empty() { None } else { Some(a) });
    }
    if let Some(app_id) = req.application_id {
        let a = app_id.trim().to_string();
        am.application_id = Set(if a.is_empty() { None } else { Some(a) });
    }
    if let Some(enabled) = req.enabled {
        am.enabled = Set(enabled);
    }
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(EndpointView::from_model(updated)))
}

/// DELETE /endpoints/{id} — remove an endpoint. Strict 404 on miss.
async fn delete_endpoint(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let existing = EpEntity::find()
        .filter(EpColumn::AccountId.eq(scope.account_id.clone()))
        .filter(EpColumn::Id.eq(id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("endpoint '{}' not found", id)))?;

    EpEntity::delete_by_id(existing.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
