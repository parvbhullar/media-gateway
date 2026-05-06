//! `/api/v1/sub-accounts` — master-only tenant management (Phase 13 — TEN-02).

use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use regex::Regex;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::auth::issue_api_key;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::{
    api_key, call_record, did, routing_tables, sip_trunk, supersip_sub_accounts, trunk_group,
    webhooks,
};

// ── View types (SHELL-04 — never serialise Model directly) ───────────────────

#[derive(Debug, Serialize)]
pub struct SubAccountView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<supersip_sub_accounts::Model> for SubAccountView {
    fn from(m: supersip_sub_accounts::Model) -> Self {
        Self {
            id: m.id,
            name: m.name,
            enabled: m.enabled,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// Create-only response — carries the api_key plaintext exactly once.
#[derive(Debug, Serialize)]
pub struct SubAccountCreated {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub api_key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sub-accounts", get(list).post(create))
        .route(
            "/sub-accounts/{id}",
            get(get_one).put(update).delete(delete_one),
        )
        .route("/sub-accounts/{id}/rotate-key", post(rotate_key))
}

// ── Guards ────────────────────────────────────────────────────────────────────

fn ensure_master(scope: &AccountScope) -> ApiResult<()> {
    if !scope.is_master {
        return Err(ApiError::forbidden("forbidden_cross_account"));
    }
    Ok(())
}

fn validate_subaccount_id(s: &str) -> ApiResult<()> {
    if s == "root" {
        return Err(ApiError::bad_request("id 'root' is reserved"));
    }
    let re = Regex::new(r"^[a-z0-9_-]{1,64}$").expect("valid regex");
    if !re.is_match(s) {
        return Err(ApiError::bad_request(
            "id must match ^[a-z0-9_-]{1,64}$ (lowercase alphanumeric, dash, underscore)",
        ));
    }
    Ok(())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn list(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
) -> ApiResult<Json<Vec<SubAccountView>>> {
    ensure_master(&scope)?;
    let rows = supersip_sub_accounts::Entity::find()
        .all(state.db())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(rows.into_iter().map(SubAccountView::from).collect()))
}

async fn get_one(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
) -> ApiResult<Json<SubAccountView>> {
    ensure_master(&scope)?;
    let row = supersip_sub_accounts::Entity::find_by_id(id.clone())
        .one(state.db())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("sub-account '{}' not found", id)))?;
    Ok(Json(SubAccountView::from(row)))
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

async fn create(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Json(req): Json<CreateRequest>,
) -> ApiResult<(StatusCode, Json<SubAccountCreated>)> {
    ensure_master(&scope)?;
    validate_subaccount_id(&req.id)?;

    let db = state.db();

    if supersip_sub_accounts::Entity::find_by_id(req.id.clone())
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .is_some()
    {
        return Err(ApiError::conflict(format!(
            "sub-account '{}' already exists",
            req.id
        )));
    }

    let now = Utc::now();
    let issued = issue_api_key();
    let sub_id = req.id.clone();

    let txn = db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let am = supersip_sub_accounts::ActiveModel {
        id: Set(req.id.clone()),
        name: Set(req.name.clone()),
        enabled: Set(req.enabled),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let row = am
        .insert(&txn)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let key_am = api_key::ActiveModel {
        name: Set(format!("{}-default", sub_id)),
        hash_sha256: Set(issued.hash.clone()),
        description: Set(Some(format!("Auto-generated for sub-account {}", sub_id))),
        created_at: Set(now),
        account_id: Set(sub_id.clone()),
        ..Default::default()
    };
    key_am
        .insert(&txn)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    txn.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(SubAccountCreated {
            id: row.id,
            name: row.name,
            enabled: row.enabled,
            api_key: issued.plaintext,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    pub name: Option<String>,
    pub enabled: Option<bool>,
}

async fn update(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
    Json(req): Json<UpdateRequest>,
) -> ApiResult<Json<SubAccountView>> {
    ensure_master(&scope)?;
    let db = state.db();
    let existing = supersip_sub_accounts::Entity::find_by_id(id.clone())
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("sub-account '{}' not found", id)))?;

    let mut am: supersip_sub_accounts::ActiveModel = existing.into();
    if let Some(name) = req.name {
        am.name = Set(name);
    }
    if let Some(enabled) = req.enabled {
        am.enabled = Set(enabled);
    }
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(SubAccountView::from(updated)))
}

async fn delete_one(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
) -> Response {
    if !scope.is_master {
        return ApiError::forbidden("forbidden_cross_account").into_response();
    }
    if id == "root" {
        return ApiError::bad_request("cannot delete root account").into_response();
    }

    let db = state.db();

    if supersip_sub_accounts::Entity::find_by_id(id.clone())
        .one(db)
        .await
        .map(|r| r.is_none())
        .unwrap_or(true)
    {
        return ApiError::not_found(format!("sub-account '{}' not found", id)).into_response();
    }

    macro_rules! count_for {
        ($entity:ty, $col:expr) => {
            <$entity>::find()
                .filter($col.eq(id.clone()))
                .count(db)
                .await
                .unwrap_or(0)
        };
    }

    let gateways = count_for!(sip_trunk::Entity, sip_trunk::Column::AccountId);
    let dids = count_for!(did::Entity, did::Column::AccountId);
    let trunks = count_for!(trunk_group::Entity, trunk_group::Column::AccountId);
    let routes = count_for!(routing_tables::Entity, routing_tables::Column::AccountId);
    let recordings = count_for!(call_record::Entity, call_record::Column::AccountId);
    let webhooks_n = count_for!(webhooks::Entity, webhooks::Column::AccountId);
    let api_keys = count_for!(api_key::Entity, api_key::Column::AccountId);

    let total = gateways + dids + trunks + routes + recordings + webhooks_n + api_keys;
    if total > 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "code": "in_use",
                "blockers": {
                    "gateways":   gateways,
                    "dids":       dids,
                    "trunks":     trunks,
                    "routes":     routes,
                    "recordings": recordings,
                    "webhooks":   webhooks_n,
                    "api_keys":   api_keys,
                }
            })),
        )
            .into_response();
    }

    match supersip_sub_accounts::Entity::delete_by_id(id.clone())
        .exec(db)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError::internal(e.to_string()).into_response(),
    }
}

async fn rotate_key(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    ensure_master(&scope)?;
    let db = state.db();

    if supersip_sub_accounts::Entity::find_by_id(id.clone())
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .is_none()
    {
        return Err(ApiError::not_found(format!(
            "sub-account '{}' not found",
            id
        )));
    }

    let now = Utc::now();
    let issued = issue_api_key();

    let txn = db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let existing_keys = api_key::Entity::find()
        .filter(api_key::Column::AccountId.eq(id.clone()))
        .filter(api_key::Column::RevokedAt.is_null())
        .all(&txn)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    for key in existing_keys {
        let mut am: api_key::ActiveModel = key.into();
        am.revoked_at = Set(Some(now));
        am.update(&txn)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }

    let key_am = api_key::ActiveModel {
        name: Set(format!("{}-key-{}", id, now.timestamp())),
        hash_sha256: Set(issued.hash),
        description: Set(Some(format!("Rotated key for sub-account {}", id))),
        created_at: Set(now),
        account_id: Set(id.clone()),
        ..Default::default()
    };
    key_am
        .insert(&txn)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    txn.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(json!({ "api_key": issued.plaintext })))
}
