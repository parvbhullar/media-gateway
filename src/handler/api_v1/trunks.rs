//! `/api/v1/trunks` -- trunk group CRUD surface (Phase 2).
//!
//! Plan 02-01 shipped list + get. Plan 02-02 adds create/update/delete
//! with gateway-existence validation (TRK-03), parallel feature-gate,
//! transactional writes, and engagement-tracked delete (TRK-04).
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
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::common::{Pagination, PaginatedResponse};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::trunk_group::{
    self, Column as TrunkGroupColumn, Entity as TrunkGroupEntity,
    Model as TrunkGroupModel, TrunkGroupDistributionMode,
};
use crate::models::trunk_group_member::{
    self, Column as TrunkMemberColumn, Entity as TrunkMemberEntity,
    Model as TrunkMemberModel,
};
use crate::models::sip_trunk::{
    Column as SipTrunkColumn, Entity as SipTrunkEntity,
};

// ---------------------------------------------------------------------------
// Wire types -- kept decoupled from SeaORM Model per SHELL-04.
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

fn view_from(
    group: TrunkGroupModel,
    mut members: Vec<TrunkMemberModel>,
) -> TrunkView {
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
// Request DTOs
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTrunkRequest {
    pub name: String,
    pub members: Vec<CreateTrunkMember>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub distribution_mode: Option<String>,
    #[serde(default)]
    pub credentials: Option<serde_json::Value>,
    #[serde(default)]
    pub acl: Option<serde_json::Value>,
    #[serde(default)]
    pub nofailover_sip_codes: Option<serde_json::Value>,
    #[serde(default = "default_true")]
    pub is_active: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTrunkMember {
    pub gateway_name: String,
    #[serde(default)]
    pub weight: Option<i32>,
    #[serde(default)]
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTrunkRequest {
    pub members: Vec<CreateTrunkMember>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub distribution_mode: Option<String>,
    #[serde(default)]
    pub credentials: Option<serde_json::Value>,
    #[serde(default)]
    pub acl: Option<serde_json::Value>,
    #[serde(default)]
    pub nofailover_sip_codes: Option<serde_json::Value>,
    #[serde(default)]
    pub is_active: Option<bool>,
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
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_trunk_group_name(name: &str) -> ApiResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(ApiError::bad_request(
            "trunk group name must be 1-64 chars, \
             alphanumeric plus _ or -",
        ));
    }
    for ch in trimmed.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
            return Err(ApiError::bad_request(
                "trunk group name must be 1-64 chars, \
                 alphanumeric plus _ or -",
            ));
        }
    }
    Ok(())
}

fn parse_direction(
    raw: Option<&str>,
) -> ApiResult<crate::models::sip_trunk::SipTrunkDirection> {
    use crate::models::sip_trunk::SipTrunkDirection;
    match raw {
        None => Ok(SipTrunkDirection::default()),
        Some("inbound") => Ok(SipTrunkDirection::Inbound),
        Some("outbound") => Ok(SipTrunkDirection::Outbound),
        Some("bidirectional") => Ok(SipTrunkDirection::Bidirectional),
        Some(other) => Err(ApiError::bad_request(format!(
            "invalid direction '{}': must be inbound, outbound, \
             or bidirectional",
            other
        ))),
    }
}

fn parse_distribution_mode(
    raw: Option<&str>,
) -> ApiResult<TrunkGroupDistributionMode> {
    match raw {
        None => Ok(TrunkGroupDistributionMode::default()),
        Some("round_robin") => Ok(TrunkGroupDistributionMode::RoundRobin),
        Some("weight_based") => {
            Ok(TrunkGroupDistributionMode::WeightBased)
        }
        Some("hash_callid") => {
            Ok(TrunkGroupDistributionMode::HashCallid)
        }
        Some("hash_src_ip") => {
            Ok(TrunkGroupDistributionMode::HashSrcIp)
        }
        Some("hash_destination") => {
            Ok(TrunkGroupDistributionMode::HashDestination)
        }
        Some("parallel") => Ok(TrunkGroupDistributionMode::Parallel),
        Some(other) => Err(ApiError::bad_request(format!(
            "invalid distribution_mode '{}'",
            other
        ))),
    }
}

fn validate_distribution_mode(
    mode: TrunkGroupDistributionMode,
) -> ApiResult<()> {
    #[cfg(not(feature = "parallel-trunk-dial"))]
    if mode == TrunkGroupDistributionMode::Parallel {
        return Err(ApiError::bad_request(
            "parallel distribution requires the \
             parallel-trunk-dial feature",
        ));
    }
    // Silence unused-variable warning when feature IS enabled.
    let _ = mode;
    Ok(())
}

async fn validate_gateway_refs(
    db: &sea_orm::DatabaseConnection,
    members: &[CreateTrunkMember],
) -> ApiResult<()> {
    if members.is_empty() {
        return Err(ApiError::bad_request(
            "trunk group must have at least one member",
        ));
    }

    let names: Vec<String> =
        members.iter().map(|m| m.gateway_name.clone()).collect();

    let found = SipTrunkEntity::find()
        .filter(SipTrunkColumn::Name.is_in(names.clone()))
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let found_names: std::collections::HashSet<&str> =
        found.iter().map(|r| r.name.as_str()).collect();

    let missing: Vec<&str> = names
        .iter()
        .filter(|n| !found_names.contains(n.as_str()))
        .map(|n| n.as_str())
        .collect();

    if !missing.is_empty() {
        return Err(ApiError::bad_request(format!(
            "unknown gateway(s): {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

async fn assert_no_gateway_name_collision(
    db: &sea_orm::DatabaseConnection,
    name: &str,
) -> ApiResult<()> {
    let existing = SipTrunkEntity::find()
        .filter(SipTrunkColumn::Name.eq(name))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if existing.is_some() {
        return Err(ApiError::bad_request(format!(
            "trunk group name '{}' collides with existing gateway",
            name
        )));
    }
    Ok(())
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
// Read handlers (unchanged from Plan 02-01)
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
    if let Some(search) =
        q.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty())
    {
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

    Ok(Json(PaginatedResponse::new(
        views, page_no, page_size, total,
    )))
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
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "trunk group '{}' not found",
                name
            ))
        })?;

    let members = TrunkMemberEntity::find()
        .filter(TrunkMemberColumn::TrunkGroupId.eq(group.id))
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(view_from(group, members)))
}

// ---------------------------------------------------------------------------
// Write handlers
// ---------------------------------------------------------------------------

async fn create_trunk(
    State(state): State<AppState>,
    Json(req): Json<CreateTrunkRequest>,
) -> ApiResult<(StatusCode, Json<TrunkView>)> {
    let db = state.db();

    // 1. validate name
    validate_trunk_group_name(&req.name)?;

    // 2. derive direction
    let direction = parse_direction(req.direction.as_deref())?;

    // 3. derive + validate distribution_mode
    let mode =
        parse_distribution_mode(req.distribution_mode.as_deref())?;
    validate_distribution_mode(mode)?;

    // 4. validate gateway refs
    validate_gateway_refs(db, &req.members).await?;

    // 5. name collision with gateway namespace
    assert_no_gateway_name_collision(db, &req.name).await?;

    // 6. duplicate trunk_group check
    let dup = TrunkGroupEntity::find()
        .filter(TrunkGroupColumn::Name.eq(req.name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "trunk group '{}' already exists",
            req.name
        )));
    }

    // 7. transactional insert
    let now = Utc::now();
    let tx = db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let group_am = trunk_group::ActiveModel {
        name: Set(req.name.clone()),
        display_name: Set(req.display_name),
        direction: Set(direction),
        distribution_mode: Set(mode),
        credentials: Set(req.credentials),
        acl: Set(req.acl),
        nofailover_sip_codes: Set(req.nofailover_sip_codes),
        is_active: Set(req.is_active),
        metadata: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let inserted = group_am
        .insert(&tx)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // 8. insert members
    for (idx, member) in req.members.iter().enumerate() {
        let member_am = trunk_group_member::ActiveModel {
            trunk_group_id: Set(inserted.id),
            gateway_name: Set(member.gateway_name.clone()),
            weight: Set(member.weight.unwrap_or(100)),
            priority: Set(member.priority.unwrap_or(0)),
            position: Set(idx as i32),
            ..Default::default()
        };
        member_am
            .insert(&tx)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }

    // 9. commit
    tx.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // 10. reload members
    let members = TrunkMemberEntity::find()
        .filter(TrunkMemberColumn::TrunkGroupId.eq(inserted.id))
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // 11. return 201
    Ok((StatusCode::CREATED, Json(view_from(inserted, members))))
}

// ---------------------------------------------------------------------------
// Write stubs -- 501 pending Plan 02-02 Task 2
// ---------------------------------------------------------------------------

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
