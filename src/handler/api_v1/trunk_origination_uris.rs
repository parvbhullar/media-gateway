//! `/api/v1/trunks/{name}/origination_uris` — TSUB-02 full implementation.
//!
//! Phase 3 Plan 03-03. Backed by `supersip_trunk_origination_uris`
//! (Plan 03-01 schema). UNIQUE (trunk_group_id, uri) per D-06. Position
//! auto-assigned by the handler as MAX(position)+1 (starting at 0 for the
//! first row) per D-06/D-07. DELETE-by-uri strict 404 per D-07. URI must
//! parse as a valid SIP URI via the rsipstack parser (D-08) — this is the
//! same parser that consumes Request-URIs on the live INVITE path, so a
//! URI accepted here is dispatchable at runtime.
//!
//! `TrunkOriginationUriView` is the wire type — `trunk_origination_uris::Model`
//! is NEVER serialized directly (SHELL-04). The handler shape mirrors
//! `trunk_credentials.rs` (Plan 03-02); adjustments for the URI domain:
//!   - `validate_sip_uri` replaces `validate_realm`/`validate_credential_fields`.
//!   - POST computes `next_position` before insert (D-06).
//!   - GET orders by `position ASC` instead of `created_at` ASC.

use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::StatusCode,
    routing::{delete, get},
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::trunk_group::{
    Column as TrunkGroupColumn, Entity as TrunkGroupEntity,
};
use crate::models::trunk_origination_uris::{
    self, Column as TouColumn, Entity as TouEntity, Model as TouModel,
};

// ─── Wire types (SHELL-04) ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TrunkOriginationUriView {
    pub uri: String,
    pub position: i32,
}

impl From<TouModel> for TrunkOriginationUriView {
    fn from(m: TouModel) -> Self {
        Self {
            uri: m.uri,
            position: m.position,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddTrunkOriginationUriRequest {
    pub uri: String,
}

// ─── Validation helpers ──────────────────────────────────────────────────

/// Enforce D-08: the URI must parse as a valid SIP URI via rsipstack's
/// parser. We reuse the same parser the proxy uses for inbound INVITE
/// Request-URIs, which guarantees a URI that passes validation here can be
/// consumed by the live dispatch path without an extra translation layer.
///
/// Also enforces the DB column length bound (VARCHAR(500) — see the
/// `supersip_trunk_origination_uris.uri` column in Plan 03-01 schema).
fn validate_sip_uri(uri: &str) -> ApiResult<()> {
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("uri must be non-empty"));
    }
    if trimmed.len() > 500 {
        return Err(ApiError::bad_request(
            "uri must be 1-500 chars (DB column limit)",
        ));
    }
    // D-08: parse via rsipstack URI parser. `TryFrom<&str>` is the
    // canonical entry point per src/proxy/locator.rs (see usages around
    // line 508). A parse failure surfaces as 400 with the rsipstack error
    // message included so operators can correct the input.
    let parse_result: Result<rsipstack::sip::Uri, _> = trimmed.try_into();
    match parse_result {
        Ok(_) => Ok(()),
        Err(e) => Err(ApiError::bad_request(format!(
            "invalid SIP URI: {}",
            e
        ))),
    }
}

/// Resolve `{name}` to a `trunk_group_id`. Returns 404 if the parent trunk
/// group does not exist — every sub-resource handler calls this first so
/// that missing-parent precedes child lookup (consistent 404 contract
/// across sub-resources; same shape as `trunk_credentials.rs`).
async fn lookup_trunk_group_id(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    account_id: &str,
) -> ApiResult<i64> {
    let group = TrunkGroupEntity::find()
        .filter(TrunkGroupColumn::Name.eq(name))
        .filter(TrunkGroupColumn::AccountId.eq(account_id))
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
            "/trunks/{name}/origination_uris",
            get(list_uris).post(add_uri),
        )
        .route(
            "/trunks/{name}/origination_uris/{uri}",
            delete(delete_uri),
        )
}

// ─── Handlers ────────────────────────────────────────────────────────────

/// GET /trunks/{name}/origination_uris — list all origination URIs for a
/// trunk group, ordered by `position` ASC (stable position; deletes do NOT
/// renumber — gaps are acceptable per D-07). Empty trunk returns `[]`.
/// Missing trunk returns 404.
async fn list_uris(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<TrunkOriginationUriView>>> {
    let db = state.db();
    let trunk_group_id = lookup_trunk_group_id(db, &name, &scope.account_id).await?;

    let rows = TouEntity::find()
        .filter(TouColumn::TrunkGroupId.eq(trunk_group_id))
        .order_by_asc(TouColumn::Position)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(
        rows.into_iter().map(TrunkOriginationUriView::from).collect(),
    ))
}

/// POST /trunks/{name}/origination_uris — validate URI via rsipstack
/// parser, pre-check UNIQUE (trunk_group_id, uri) for a friendly 409,
/// compute next position as MAX(position)+1 (or 0 for first row), insert,
/// return 201 with the view. The DB UNIQUE index is the safety net for
/// concurrent writes (races surface as 500 — acceptable per T-03-URI-03).
async fn add_uri(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
    Json(req): Json<AddTrunkOriginationUriRequest>,
) -> ApiResult<(StatusCode, Json<TrunkOriginationUriView>)> {
    let db = state.db();
    validate_sip_uri(&req.uri)?;
    let trunk_group_id = lookup_trunk_group_id(db, &name, &scope.account_id).await?;

    // Pre-check duplicate (UNIQUE (trunk_group_id, uri) per D-06).
    let dup = TouEntity::find()
        .filter(TouColumn::TrunkGroupId.eq(trunk_group_id))
        .filter(TouColumn::Uri.eq(req.uri.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "uri '{}' already exists on trunk '{}'",
            req.uri, name
        )));
    }

    // D-06: auto-assign position = MAX(position) + 1, or 0 for first row.
    // Race: two concurrent POSTs could compute the same next_position; the
    // UNIQUE (trunk_group_id, uri) index prevents a duplicate insert, so
    // one of the two surfaces as 500 (accepted per T-03-URI-03 — operator
    // workflow, not hot-path).
    let next_position: i32 = TouEntity::find()
        .filter(TouColumn::TrunkGroupId.eq(trunk_group_id))
        .order_by_desc(TouColumn::Position)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map(|r| r.position + 1)
        .unwrap_or(0);

    let now = Utc::now();
    let am = trunk_origination_uris::ActiveModel {
        trunk_group_id: Set(trunk_group_id),
        uri: Set(req.uri.clone()),
        position: Set(next_position),
        created_at: Set(now),
        ..Default::default()
    };

    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(TrunkOriginationUriView::from(inserted)),
    ))
}

/// DELETE /trunks/{name}/origination_uris/{uri} — strict 404-on-miss per
/// D-07. `{uri}` is URL-decoded by axum's Path extractor before it reaches
/// this handler (clients must URL-encode `:` and other reserved chars).
///
/// Position is NOT renumbered on delete — per D-07, position is stable
/// and gaps are acceptable.
async fn delete_uri(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path((name, uri)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let trunk_group_id = lookup_trunk_group_id(db, &name, &scope.account_id).await?;

    let row = TouEntity::find()
        .filter(TouColumn::TrunkGroupId.eq(trunk_group_id))
        .filter(TouColumn::Uri.eq(uri.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "uri '{}' not found on trunk '{}'",
                uri, name
            ))
        })?;

    TouEntity::delete_by_id(row.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
