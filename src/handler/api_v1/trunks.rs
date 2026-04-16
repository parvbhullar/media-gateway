//! `/api/v1/trunks` — trunk group read-only surface (Phase 2, Plan 02-01).
//!
//! This sub-router exposes list + get for trunk groups with their members.
//! Write handlers (create/update/delete) are deliberate 501 stubs pending
//! Plan 02-02.
//!
//! `TrunkView` is the wire type. `trunk_group::Model` (the SeaORM row) is
//! NEVER serialized directly, preserving SHELL-04.

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
use crate::handler::api_v1::common::{Pagination, PaginatedResponse};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::trunk_group::{
    Column as TrunkGroupColumn, Entity as TrunkGroupEntity,
    Model as TrunkGroupModel,
};
use crate::models::trunk_group_member::{
    Column as TrunkMemberColumn, Entity as TrunkMemberEntity,
    Model as TrunkMemberModel,
};

// ---------------------------------------------------------------------------
// Wire types — kept decoupled from SeaORM Model per SHELL-04.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct TrunkView {
    pub name: String,
    pub display_name: Option<String>,
    pub direction: String,
    pub distribution_mode: String,
    pub members: Vec<TrunkMemberView>,
    pub credentials: Option<serde_json::Value>,
    pub acl: Option<serde_json::Value>,
    pub nofailover_sip_codes: Option<serde_json::Value>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct TrunkMemberView {
    pub gateway_name: String,
    pub weight: i32,
    pub priority: i32,
    pub position: i32,
}

impl From<TrunkMemberModel> for TrunkMemberView {
    fn from(m: TrunkMemberModel) -> Self {
        Self {
            gateway_name: m.gateway_name,
            weight: m.weight,
            priority: m.priority,
            position: m.position,
        }
    }
}

fn view_from(group: TrunkGroupModel, mut members: Vec<TrunkMemberModel>) -> TrunkView {
    members.sort_by_key(|m| m.position);
    TrunkView {
        name: group.name,
        display_name: group.display_name,
        direction: group.direction.as_str().to_string(),
        distribution_mode: group.distribution_mode.as_str().to_string(),
        members: members.into_iter().map(TrunkMemberView::from).collect(),
        credentials: group.credentials,
        acl: group.acl,
        nofailover_sip_codes: group.nofailover_sip_codes,
        is_active: group.is_active,
        created_at: group.created_at,
        updated_at: group.updated_at,
    }
}

// ---------------------------------------------------------------------------
// Pagination query
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TrunkListQuery {
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub page_size: Option<u64>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
}

impl TrunkListQuery {
    fn pagination(&self) -> Pagination {
        Pagination {
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/trunks", get(list_trunks).post(create_trunk))
        .route(
            "/trunks/{name}",
            get(get_trunk).put(update_trunk).delete(delete_trunk),
        )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_trunks(
    State(state): State<AppState>,
    Query(q): Query<TrunkListQuery>,
) -> ApiResult<Json<PaginatedResponse<TrunkView>>> {
    let db = state.db();
    let pagination = q.pagination();
    let page_no = pagination.page.max(1);
    let page_size = pagination.limit();

    let mut conds = Condition::all();
    if let Some(dir) = q
        .direction
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        conds = conds.add(TrunkGroupColumn::Direction.eq(dir));
    }
    if let Some(search) = q.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let like = format!("%{}%", search);
        conds = conds.add(TrunkGroupColumn::Name.like(like));
    }

    let paginator = TrunkGroupEntity::find()
        .filter(conds)
        .order_by_asc(TrunkGroupColumn::Name)
        .paginate(db, page_size);

    let total = paginator
        .num_items()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let groups = paginator
        .fetch_page(page_no.saturating_sub(1))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // TODO(phase-3): batch-load members with a single IN query
    let mut views = Vec::with_capacity(groups.len());
    for group in groups {
        let members = TrunkMemberEntity::find()
            .filter(TrunkMemberColumn::TrunkGroupId.eq(group.id))
            .all(db)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        views.push(view_from(group, members));
    }

    Ok(Json(PaginatedResponse::new(views, page_no, page_size, total)))
}

async fn get_trunk(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<TrunkView>> {
    let db = state.db();
    let group = TrunkGroupEntity::find()
        .filter(TrunkGroupColumn::Name.eq(name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("trunk group '{}' not found", name)))?;

    let members = TrunkMemberEntity::find()
        .filter(TrunkMemberColumn::TrunkGroupId.eq(group.id))
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(view_from(group, members)))
}

// ---------------------------------------------------------------------------
// Write stubs — 501 pending Plan 02-02
// ---------------------------------------------------------------------------

async fn create_trunk(
    State(_): State<AppState>,
    Json(_): Json<serde_json::Value>,
) -> ApiResult<StatusCode> {
    Err(ApiError::not_implemented(
        "trunk group write endpoints land in Plan 02-02",
    ))
}

async fn update_trunk(
    State(_): State<AppState>,
    Path(_): Path<String>,
    Json(_): Json<serde_json::Value>,
) -> ApiResult<StatusCode> {
    Err(ApiError::not_implemented(
        "trunk group write endpoints land in Plan 02-02",
    ))
}

async fn delete_trunk(
    State(_): State<AppState>,
    Path(_): Path<String>,
) -> ApiResult<StatusCode> {
    Err(ApiError::not_implemented(
        "trunk group write endpoints land in Plan 02-02",
    ))
}
