//! `/api/v1/trunks/{name}/credentials` — TSUB-01 full implementation.
//!
//! Phase 3 Plan 03-02. Backed by `supersip_trunk_credentials` (Plan 03-01
//! schema). UNIQUE (trunk_group_id, realm) per D-01. Plaintext password
//! per D-03 (v2.1 hardening concern). DELETE-by-realm strict 404 per D-04.
//!
//! `TrunkCredentialView` is the wire type — `trunk_credentials::Model` is
//! NEVER serialized directly (SHELL-04).
//
// TODO(v2.1): encrypt `auth_password` at rest per the security hardening
// milestone. Phase 3 intentionally stores plaintext for parity with
// `gateways.rs` (Phase 1 convention) — rotating to encrypted-at-rest
// requires a paired read/write migration layered on a new crypto service
// and coordinated API surface change (write-only password fields).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::trunk_credentials::{
    self, Column as TcColumn, Entity as TcEntity, Model as TcModel,
};
use crate::models::trunk_group::{
    Column as TrunkGroupColumn, Entity as TrunkGroupEntity,
};

// ─── Wire types (SHELL-04) ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TrunkCredentialView {
    pub realm: String,
    pub username: String,
    // TODO(v2.1): remove plaintext password from GET response once the
    // operator workflow switches to write-only rotation (milestone v2.1).
    pub password: String,
}

impl From<TcModel> for TrunkCredentialView {
    fn from(m: TcModel) -> Self {
        Self {
            realm: m.realm,
            username: m.auth_username,
            password: m.auth_password,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddTrunkCredentialRequest {
    pub realm: String,
    pub username: String,
    pub password: String,
}

// ─── Validation helpers ──────────────────────────────────────────────────

/// Enforce D-05: realm is 1-255 chars and must not contain '/'
/// (router-path conflict with DELETE /trunks/{name}/credentials/{realm}).
fn validate_realm(realm: &str) -> ApiResult<()> {
    let trimmed = realm.trim();
    if trimmed.is_empty() || trimmed.len() > 255 {
        return Err(ApiError::bad_request(
            "realm must be 1-255 chars (D-05)",
        ));
    }
    if trimmed.contains('/') {
        return Err(ApiError::bad_request(
            "realm must not contain '/' (URL-path conflict, D-05)",
        ));
    }
    Ok(())
}

fn validate_credential_fields(req: &AddTrunkCredentialRequest) -> ApiResult<()> {
    validate_realm(&req.realm)?;
    if req.username.trim().is_empty() {
        return Err(ApiError::bad_request("username must be non-empty"));
    }
    if req.password.is_empty() {
        return Err(ApiError::bad_request("password must be non-empty"));
    }
    Ok(())
}

/// Resolve `{name}` to a `trunk_group_id`. Returns 404 if the parent
/// trunk group does not exist — every sub-resource handler calls this
/// first so that missing-parent precedes child lookup (consistent 404
/// contract across sub-resources).
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

// ─── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/trunks/{name}/credentials",
            get(list_credentials).post(add_credential),
        )
        .route(
            "/trunks/{name}/credentials/{realm}",
            delete(delete_credential),
        )
}

// ─── Handlers ────────────────────────────────────────────────────────────

/// GET /trunks/{name}/credentials — list all credentials for a trunk
/// group, ordered by `created_at` ASC (insertion order, stable). Empty
/// trunk returns `[]`. Missing trunk returns 404.
async fn list_credentials(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<TrunkCredentialView>>> {
    let db = state.db();
    let trunk_group_id = lookup_trunk_group_id(db, &name).await?;

    let rows = TcEntity::find()
        .filter(TcColumn::TrunkGroupId.eq(trunk_group_id))
        .order_by_asc(TcColumn::CreatedAt)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(
        rows.into_iter().map(TrunkCredentialView::from).collect(),
    ))
}

/// POST /trunks/{name}/credentials — add a credential. Pre-checks the
/// UNIQUE (trunk_group_id, realm) constraint for a friendly 409; the DB
/// UNIQUE index is the safety net for concurrent writes (races surface
/// as 500, rare and operator-driven).
async fn add_credential(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<AddTrunkCredentialRequest>,
) -> ApiResult<(StatusCode, Json<TrunkCredentialView>)> {
    let db = state.db();
    validate_credential_fields(&req)?;
    let trunk_group_id = lookup_trunk_group_id(db, &name).await?;

    // Pre-check duplicate (UNIQUE (trunk_group_id, realm) per D-01).
    let dup = TcEntity::find()
        .filter(TcColumn::TrunkGroupId.eq(trunk_group_id))
        .filter(TcColumn::Realm.eq(req.realm.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "credential for realm '{}' already exists on trunk '{}'",
            req.realm, name
        )));
    }

    let now = Utc::now();
    let am = trunk_credentials::ActiveModel {
        trunk_group_id: Set(trunk_group_id),
        realm: Set(req.realm.clone()),
        auth_username: Set(req.username.clone()),
        auth_password: Set(req.password.clone()),
        created_at: Set(now),
        ..Default::default()
    };

    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(TrunkCredentialView::from(inserted)),
    ))
}

/// DELETE /trunks/{name}/credentials/{realm} — strict 404-on-miss per
/// D-04. `{realm}` is URL-decoded by axum's Path extractor before it
/// reaches this handler.
async fn delete_credential(
    State(state): State<AppState>,
    Path((name, realm)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let trunk_group_id = lookup_trunk_group_id(db, &name).await?;

    let row = TcEntity::find()
        .filter(TcColumn::TrunkGroupId.eq(trunk_group_id))
        .filter(TcColumn::Realm.eq(realm.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "credential for realm '{}' not found on trunk '{}'",
                realm, name
            ))
        })?;

    TcEntity::delete_by_id(row.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
