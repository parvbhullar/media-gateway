//! `/api/v1/applications` — SIP application CRUD + DID attach/detach (Phase 13 Plan 13-03).
//!
//! Backed by `supersip_applications` (APP-01) and `supersip_application_numbers`
//! (APP-02). UUID PK per plan spec. account_id always stamped from AccountScope
//! (never from request body) — D-05.
//!
//! Routes:
//!   GET    /applications              — list
//!   POST   /applications              — create (201)
//!   GET    /applications/{id}         — get one
//!   PUT    /applications/{id}         — update
//!   DELETE /applications/{id}         — delete (204)
//!   POST   /applications/{id}/numbers — attach DIDs (transactional)
//!   DELETE /applications/{id}/numbers/{did_id} — detach one DID

use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::common::{CommonScopeQuery, build_account_filter};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::supersip_application_numbers::{
    ActiveModel as NumActiveModel, Column as NumColumn, Entity as NumEntity,
};
use crate::models::supersip_applications::{
    self, Column as AppColumn, Entity as AppEntity, Model as AppModel,
};
use crate::models::did::{Column as DidColumn, Entity as DidEntity};

// ---- Wire types -------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ApplicationView {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub answer_url: String,
    pub hangup_url: Option<String>,
    pub message_url: Option<String>,
    pub auth_headers: serde_json::Value,
    pub answer_timeout_ms: i32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<AppModel> for ApplicationView {
    fn from(m: AppModel) -> Self {
        Self {
            id: m.id,
            account_id: m.account_id,
            name: m.name,
            answer_url: m.answer_url,
            hangup_url: m.hangup_url,
            message_url: m.message_url,
            auth_headers: m.auth_headers,
            answer_timeout_ms: m.answer_timeout_ms,
            enabled: m.enabled,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ApplicationNumberView {
    pub application_id: String,
    pub did_id: String,
    pub account_id: String,
    pub created_at: DateTime<Utc>,
}

// ---- Request types ----------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateApplicationRequest {
    pub name: String,
    pub answer_url: String,
    #[serde(default)]
    pub hangup_url: Option<String>,
    #[serde(default)]
    pub message_url: Option<String>,
    #[serde(default = "default_auth_headers")]
    pub auth_headers: serde_json::Value,
    #[serde(default = "default_timeout")]
    pub answer_timeout_ms: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    // D-05: account_id from body is silently ignored — not in this struct
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateApplicationRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub answer_url: Option<String>,
    #[serde(default)]
    pub hangup_url: Option<String>,
    #[serde(default)]
    pub message_url: Option<String>,
    #[serde(default)]
    pub auth_headers: Option<serde_json::Value>,
    #[serde(default)]
    pub answer_timeout_ms: Option<i32>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AttachNumbersRequest {
    pub did_ids: Vec<String>,
}

fn default_auth_headers() -> serde_json::Value {
    serde_json::json!({})
}

fn default_timeout() -> i32 {
    5000
}

fn default_true() -> bool {
    true
}

// ---- Validation helpers -----------------------------------------------------

fn validate_url(url: &str, field: &str) -> ApiResult<()> {
    let t = url.trim();
    if t.is_empty() {
        return Err(ApiError::bad_request(format!("{field} is required")));
    }
    if !t.starts_with("http://") && !t.starts_with("https://") {
        return Err(ApiError::bad_request(format!(
            "{field} must start with http:// or https://"
        )));
    }
    Ok(())
}

fn validate_name(name: &str) -> ApiResult<()> {
    let t = name.trim();
    if t.is_empty() {
        return Err(ApiError::bad_request("application name is required"));
    }
    if t.len() > 255 {
        return Err(ApiError::bad_request(
            "application name exceeds 255 characters",
        ));
    }
    Ok(())
}

// ---- Router -----------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/applications", get(list).post(create))
        .route(
            "/applications/{id}",
            get(get_one).put(update).delete(delete_one),
        )
        .route("/applications/{id}/numbers", post(attach_numbers))
        .route(
            "/applications/{id}/numbers/{did_id}",
            delete(detach_number),
        )
}

// ---- Handlers ---------------------------------------------------------------

/// GET /applications — list all applications for the calling tenant.
async fn list(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(scope_q): Query<CommonScopeQuery>,
) -> ApiResult<Json<Vec<ApplicationView>>> {
    let db = state.db();
    let cond =
        build_account_filter(&scope, AppColumn::AccountId, &scope_q, Condition::all())?;
    let rows = AppEntity::find()
        .filter(cond)
        .order_by_asc(AppColumn::CreatedAt)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(rows.into_iter().map(ApplicationView::from).collect()))
}

/// GET /applications/{id} — fetch one by UUID.
async fn get_one(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
) -> ApiResult<Json<ApplicationView>> {
    let db = state.db();
    let row = find_app(db, &scope.account_id, &id).await?;
    Ok(Json(ApplicationView::from(row)))
}

/// POST /applications — create a new application.
async fn create(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Json(req): Json<CreateApplicationRequest>,
) -> ApiResult<(StatusCode, Json<ApplicationView>)> {
    validate_name(&req.name)?;
    validate_url(&req.answer_url, "answer_url")?;
    if let Some(ref u) = req.hangup_url {
        validate_url(u, "hangup_url")?;
    }
    if let Some(ref u) = req.message_url {
        validate_url(u, "message_url")?;
    }

    let db = state.db();

    // UNIQUE (account_id, name) — pre-check for a clear 409
    let dup = AppEntity::find()
        .filter(AppColumn::AccountId.eq(scope.account_id.clone()))
        .filter(AppColumn::Name.eq(req.name.trim()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "application '{}' already exists",
            req.name
        )));
    }

    let now = Utc::now();
    let id = Uuid::new_v4().to_string();

    let am = supersip_applications::ActiveModel {
        id: Set(id),
        account_id: Set(scope.account_id.clone()),
        name: Set(req.name.trim().to_string()),
        answer_url: Set(req.answer_url.trim().to_string()),
        hangup_url: Set(req.hangup_url.map(|u| u.trim().to_string())),
        message_url: Set(req.message_url.map(|u| u.trim().to_string())),
        auth_headers: Set(req.auth_headers),
        answer_timeout_ms: Set(req.answer_timeout_ms),
        enabled: Set(req.enabled),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((StatusCode::CREATED, Json(ApplicationView::from(inserted))))
}

/// PUT /applications/{id} — partial-field update.
async fn update(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
    Json(req): Json<UpdateApplicationRequest>,
) -> ApiResult<Json<ApplicationView>> {
    let db = state.db();
    let existing = find_app(db, &scope.account_id, &id).await?;

    let mut am: supersip_applications::ActiveModel = existing.into();

    if let Some(name) = req.name {
        validate_name(&name)?;
        am.name = Set(name.trim().to_string());
    }
    if let Some(url) = req.answer_url {
        validate_url(&url, "answer_url")?;
        am.answer_url = Set(url.trim().to_string());
    }
    if let Some(url) = req.hangup_url {
        let trimmed = url.trim().to_string();
        if !trimmed.is_empty() {
            validate_url(&trimmed, "hangup_url")?;
            am.hangup_url = Set(Some(trimmed));
        } else {
            am.hangup_url = Set(None);
        }
    }
    if let Some(url) = req.message_url {
        let trimmed = url.trim().to_string();
        if !trimmed.is_empty() {
            validate_url(&trimmed, "message_url")?;
            am.message_url = Set(Some(trimmed));
        } else {
            am.message_url = Set(None);
        }
    }
    if let Some(headers) = req.auth_headers {
        am.auth_headers = Set(headers);
    }
    if let Some(timeout) = req.answer_timeout_ms {
        am.answer_timeout_ms = Set(timeout);
    }
    if let Some(enabled) = req.enabled {
        am.enabled = Set(enabled);
    }
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(ApplicationView::from(updated)))
}

/// DELETE /applications/{id} — hard delete. 204 on success.
async fn delete_one(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let existing = find_app(db, &scope.account_id, &id).await?;

    AppEntity::delete_by_id(existing.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /applications/{id}/numbers — attach DIDs to an application.
///
/// Body: `{ "did_ids": ["<did_number>", ...] }`
///
/// TRANSACTIONAL: validates ALL DIDs before inserting ANY row. Error codes:
///   - 400 `did_not_found` — DID doesn't exist
///   - 403 `forbidden_cross_account` — DID belongs to a different account
///   - 409 `did_in_use` — DID is already attached to another application
///     (response includes `current_application_id`)
async fn attach_numbers(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
    Json(req): Json<AttachNumbersRequest>,
) -> ApiResult<(StatusCode, Json<Vec<ApplicationNumberView>>)> {
    let db = state.db();
    let _app = find_app(db, &scope.account_id, &id).await?;

    // Validate all DIDs before inserting anything (transactional semantics)
    for did_id in &req.did_ids {
        let did_row = DidEntity::find()
            .filter(DidColumn::Number.eq(did_id.clone()))
            .one(db)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;

        let did_row = match did_row {
            None => {
                return Err(ApiError {
                    status: StatusCode::BAD_REQUEST,
                    code: "did_not_found",
                    message: format!("DID '{}' not found", did_id),
                });
            }
            Some(r) => r,
        };

        // Cross-account check: DID's account_id must match caller's account
        if did_row.account_id != scope.account_id {
            return Err(ApiError {
                status: StatusCode::FORBIDDEN,
                code: "forbidden_cross_account",
                message: format!("DID '{}' belongs to a different account", did_id),
            });
        }

        // UNIQUE did_id: check if already attached to a DIFFERENT application
        let existing_link = NumEntity::find()
            .filter(NumColumn::DidId.eq(did_id.clone()))
            .one(db)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;

        if let Some(link) = existing_link {
            if link.application_id != id {
                return Err(ApiError {
                    status: StatusCode::CONFLICT,
                    code: "did_in_use",
                    message: format!(
                        "DID '{}' is already attached to application '{}'",
                        did_id, link.application_id
                    ),
                });
            }
            // Already attached to THIS application — idempotent, allow through
        }
    }

    // All validations passed — insert in a transaction
    let txn = db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let now = Utc::now();
    let mut results = Vec::new();

    for did_id in &req.did_ids {
        // Skip if already attached to this application (idempotent)
        let already = NumEntity::find()
            .filter(NumColumn::DidId.eq(did_id.clone()))
            .filter(NumColumn::ApplicationId.eq(id.clone()))
            .one(&txn)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;

        if already.is_some() {
            continue;
        }

        let am = NumActiveModel {
            application_id: Set(id.clone()),
            did_id: Set(did_id.clone()),
            account_id: Set(scope.account_id.clone()),
            created_at: Set(now),
        };
        let inserted = am
            .insert(&txn)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;

        results.push(ApplicationNumberView {
            application_id: inserted.application_id,
            did_id: inserted.did_id,
            account_id: inserted.account_id,
            created_at: inserted.created_at,
        });
    }

    txn.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((StatusCode::CREATED, Json(results)))
}

/// DELETE /applications/{id}/numbers/{did_id} — detach a DID from an application.
async fn detach_number(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path((id, did_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let _app = find_app(db, &scope.account_id, &id).await?;

    let link = NumEntity::find()
        .filter(NumColumn::ApplicationId.eq(id.clone()))
        .filter(NumColumn::DidId.eq(did_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "DID '{}' is not attached to application '{}'",
                did_id, id
            ))
        })?;

    NumEntity::delete_many()
        .filter(NumColumn::ApplicationId.eq(link.application_id))
        .filter(NumColumn::DidId.eq(link.did_id))
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

// ---- Shared helpers ---------------------------------------------------------

/// Fetch an application by id scoped to the caller's account, or 404.
async fn find_app(
    db: &sea_orm::DatabaseConnection,
    account_id: &str,
    id: &str,
) -> ApiResult<AppModel> {
    AppEntity::find()
        .filter(AppColumn::AccountId.eq(account_id))
        .filter(AppColumn::Id.eq(id))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("application '{}' not found", id)))
}
