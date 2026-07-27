//! `/api/v1/extensions` — SIP extension CRUD.
//!
//! Extensions are the built-in SIP user concept backed by `rustpbx_extensions`.
//! The `ExtensionUserBackend` reads this table during INVITE/REGISTER auth when
//! `type = "extension"` is configured in `proxy.user_backends`.
//!
//! `sip_password` is accepted on create/update but never returned in responses.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::common::{Pagination, PaginatedResponse};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::extension::{
    ActiveModel as ExtActive, Column as ExtColumn, Entity as ExtEntity, Model as ExtModel,
    MAX_FORWARDING_TIMEOUT, MIN_FORWARDING_TIMEOUT,
};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Public view — sip_password is intentionally omitted.
#[derive(Debug, Serialize)]
pub struct ExtensionView {
    pub id: i64,
    pub extension: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub status: Option<String>,
    pub login_disabled: bool,
    pub voicemail_disabled: bool,
    pub allow_guest_calls: bool,
    pub call_forwarding_mode: Option<String>,
    pub call_forwarding_destination: Option<String>,
    pub call_forwarding_timeout: Option<i32>,
    pub registered_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<ExtModel> for ExtensionView {
    fn from(m: ExtModel) -> Self {
        Self {
            id: m.id,
            extension: m.extension,
            display_name: m.display_name,
            email: m.email,
            status: m.status,
            login_disabled: m.login_disabled,
            voicemail_disabled: m.voicemail_disabled,
            allow_guest_calls: m.allow_guest_calls,
            call_forwarding_mode: m.call_forwarding_mode,
            call_forwarding_destination: m.call_forwarding_destination,
            call_forwarding_timeout: m.call_forwarding_timeout,
            registered_at: m.registered_at,
            notes: m.notes,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Request DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateExtensionRequest {
    pub extension: String,
    #[serde(default)]
    pub sip_password: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub login_disabled: Option<bool>,
    #[serde(default)]
    pub voicemail_disabled: Option<bool>,
    #[serde(default)]
    pub allow_guest_calls: Option<bool>,
    #[serde(default)]
    pub call_forwarding_mode: Option<String>,
    #[serde(default)]
    pub call_forwarding_destination: Option<String>,
    #[serde(default)]
    pub call_forwarding_timeout: Option<i32>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// PATCH-style: omitted fields keep their current value.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateExtensionRequest {
    #[serde(default)]
    pub extension: Option<String>,
    #[serde(default)]
    pub sip_password: Option<String>,
    #[serde(default)]
    pub display_name: Option<Option<String>>,
    #[serde(default)]
    pub email: Option<Option<String>>,
    #[serde(default)]
    pub status: Option<Option<String>>,
    #[serde(default)]
    pub login_disabled: Option<bool>,
    #[serde(default)]
    pub voicemail_disabled: Option<bool>,
    #[serde(default)]
    pub allow_guest_calls: Option<bool>,
    #[serde(default)]
    pub call_forwarding_mode: Option<Option<String>>,
    #[serde(default)]
    pub call_forwarding_destination: Option<Option<String>>,
    #[serde(default)]
    pub call_forwarding_timeout: Option<Option<i32>>,
    #[serde(default)]
    pub notes: Option<Option<String>>,
}

// ---------------------------------------------------------------------------
// List query
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ExtensionListQuery {
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub page_size: Option<u64>,
    /// Substring match on extension number or display_name.
    #[serde(default)]
    pub q: Option<String>,
    /// Filter by login_disabled flag.
    #[serde(default)]
    pub login_disabled: Option<bool>,
}

impl ExtensionListQuery {
    fn pagination(&self) -> Pagination {
        Pagination {
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_extension_number(ext: &str) -> ApiResult<()> {
    let t = ext.trim();
    if t.is_empty() || t.len() > 32 {
        return Err(ApiError::bad_request(
            "extension must be 1-32 characters",
        ));
    }
    if !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+') {
        return Err(ApiError::bad_request(
            "extension may only contain alphanumeric characters, +, -, or _",
        ));
    }
    Ok(())
}

fn validate_forwarding_timeout(timeout: i32) -> ApiResult<()> {
    if !(MIN_FORWARDING_TIMEOUT..=MAX_FORWARDING_TIMEOUT).contains(&timeout) {
        return Err(ApiError::bad_request(format!(
            "call_forwarding_timeout must be between {} and {} seconds",
            MIN_FORWARDING_TIMEOUT, MAX_FORWARDING_TIMEOUT
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/extensions", get(list_extensions).post(create_extension))
        .route(
            "/extensions/{id}",
            get(get_extension).patch(update_extension).delete(delete_extension),
        )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_extensions(
    State(state): State<AppState>,
    Query(q): Query<ExtensionListQuery>,
) -> ApiResult<Json<PaginatedResponse<ExtensionView>>> {
    let db = state.db();
    let pagination = q.pagination();
    let page_no = pagination.page.max(1);
    let page_size = pagination.limit();

    let mut conds = Condition::all();
    if let Some(disabled) = q.login_disabled {
        conds = conds.add(ExtColumn::LoginDisabled.eq(disabled));
    }
    if let Some(needle) = q.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let pat = format!("%{}%", needle);
        conds = conds.add(
            Condition::any()
                .add(ExtColumn::Extension.like(pat.clone()))
                .add(ExtColumn::DisplayName.like(pat)),
        );
    }

    let paginator = ExtEntity::find()
        .filter(conds)
        .order_by_asc(ExtColumn::Extension)
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
        rows.into_iter().map(ExtensionView::from).collect(),
        page_no,
        page_size,
        total,
    )))
}

async fn get_extension(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<ExtensionView>> {
    let db = state.db();
    let row = ExtEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("Extension {id} not found")))?;
    Ok(Json(ExtensionView::from(row)))
}

async fn create_extension(
    State(state): State<AppState>,
    Json(req): Json<CreateExtensionRequest>,
) -> ApiResult<(StatusCode, Json<ExtensionView>)> {
    let db = state.db();
    let ext = req.extension.trim().to_string();

    validate_extension_number(&ext)?;

    if let Some(t) = req.call_forwarding_timeout {
        validate_forwarding_timeout(t)?;
    }

    // Duplicate check → clean 409 instead of a DB UNIQUE violation.
    if ExtEntity::find()
        .filter(ExtColumn::Extension.eq(&ext))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .is_some()
    {
        return Err(ApiError::conflict(format!(
            "extension '{}' already exists",
            ext
        )));
    }

    let now = Utc::now();
    let active = ExtActive {
        id: ActiveValue::NotSet,
        // org_id left unset → DB default ('default') until 3.1b threads the real org_id
        org_id: ActiveValue::NotSet,
        extension: Set(ext),
        display_name: Set(req.display_name),
        email: Set(req.email),
        status: Set(req.status),
        login_disabled: Set(req.login_disabled.unwrap_or(false)),
        voicemail_disabled: Set(req.voicemail_disabled.unwrap_or(false)),
        allow_guest_calls: Set(req.allow_guest_calls.unwrap_or(false)),
        sip_password: Set(req.sip_password),
        call_forwarding_mode: Set(req.call_forwarding_mode),
        call_forwarding_destination: Set(req.call_forwarding_destination),
        call_forwarding_timeout: Set(req.call_forwarding_timeout),
        registered_at: Set(None),
        notes: Set(req.notes),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let inserted = active
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((StatusCode::CREATED, Json(ExtensionView::from(inserted))))
}

async fn update_extension(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateExtensionRequest>,
) -> ApiResult<Json<ExtensionView>> {
    let db = state.db();

    let existing = ExtEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("Extension {id} not found")))?;

    // If renaming, validate new number and check for duplicates.
    if let Some(ref new_ext) = req.extension {
        let trimmed = new_ext.trim();
        validate_extension_number(trimmed)?;
        if trimmed != existing.extension {
            if ExtEntity::find()
                .filter(ExtColumn::Extension.eq(trimmed))
                .one(db)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?
                .is_some()
            {
                return Err(ApiError::conflict(format!(
                    "extension '{}' already exists",
                    trimmed
                )));
            }
        }
    }

    if let Some(Some(t)) = req.call_forwarding_timeout {
        validate_forwarding_timeout(t)?;
    }

    let mut active: ExtActive = existing.into();

    if let Some(ext) = req.extension {
        active.extension = Set(ext.trim().to_string());
    }
    if let Some(pw) = req.sip_password {
        active.sip_password = Set(Some(pw));
    }
    if let Some(v) = req.display_name {
        active.display_name = Set(v);
    }
    if let Some(v) = req.email {
        active.email = Set(v);
    }
    if let Some(v) = req.status {
        active.status = Set(v);
    }
    if let Some(v) = req.login_disabled {
        active.login_disabled = Set(v);
    }
    if let Some(v) = req.voicemail_disabled {
        active.voicemail_disabled = Set(v);
    }
    if let Some(v) = req.allow_guest_calls {
        active.allow_guest_calls = Set(v);
    }
    if let Some(v) = req.call_forwarding_mode {
        active.call_forwarding_mode = Set(v);
    }
    if let Some(v) = req.call_forwarding_destination {
        active.call_forwarding_destination = Set(v);
    }
    if let Some(v) = req.call_forwarding_timeout {
        active.call_forwarding_timeout = Set(v);
    }
    if let Some(v) = req.notes {
        active.notes = Set(v);
    }
    active.updated_at = Set(Utc::now());

    let updated = active
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(ExtensionView::from(updated)))
}

async fn delete_extension(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let db = state.db();

    let existing = ExtEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("Extension {id} not found")))?;

    ExtEntity::delete_by_id(existing.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
