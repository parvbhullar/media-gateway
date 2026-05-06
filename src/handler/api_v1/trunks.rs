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
    extract::{Extension, Path, Query, State},
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
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::common::{CommonScopeQuery, Pagination, PaginatedResponse, build_account_filter};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::trunk_group::{
    self, Column as TrunkGroupColumn, Entity as TrunkGroupEntity,
    Model as TrunkGroupModel, TrunkGroupDistributionMode,
};
use crate::models::trunk_group_member::{
    self, Column as TrunkMemberColumn, Entity as TrunkMemberEntity,
    Model as TrunkMemberModel,
};
use crate::models::did::{
    Column as DidColumn, Entity as DidEntity,
};
use crate::models::routing::Entity as RouteEntity;
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
    // Phase 3 Plan 03-01 (D-02): `credentials` moved to a multi-row
    // sub-resource at /api/v1/trunks/{name}/credentials (Plan 03-02).
    // Phase 5 Plan 05-01 (D-10): `acl` moved to multi-row sub-resource at
    // /api/v1/trunks/{name}/acl (Plan 05-03).
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
    // Phase 3 Plan 03-01 (D-02): `credentials` removed — POST to
    // /api/v1/trunks/{name}/credentials (Plan 03-02) instead.
    // Phase 5 Plan 05-01 (D-10): `acl` removed — POST to
    // /api/v1/trunks/{name}/acl (Plan 05-03) instead.
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
    // Phase 3 Plan 03-01 (D-02): `credentials` removed — sub-resource.
    // Phase 5 Plan 05-01 (D-10): `acl` removed — sub-resource.
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
    Extension(scope): Extension<AccountScope>,
    Query(scope_q): Query<CommonScopeQuery>,
    Query(q): Query<TrunkListQuery>,
) -> ApiResult<Json<PaginatedResponse<TrunkView>>> {
    let db = state.db();
    let pagination = q.pagination();
    let page_no = pagination.page.max(1);
    let page_size = pagination.limit();

    let mut conds = build_account_filter(&scope, TrunkGroupColumn::AccountId, &scope_q, Condition::all())?;
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
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
) -> ApiResult<Json<TrunkView>> {
    let db = state.db();
    let group = TrunkGroupEntity::find()
        .filter(TrunkGroupColumn::Name.eq(name.clone()))
        .filter(TrunkGroupColumn::AccountId.eq(scope.account_id.clone()))
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
    Extension(scope): Extension<AccountScope>,
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
        .filter(TrunkGroupColumn::AccountId.eq(scope.account_id.clone()))
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
        nofailover_sip_codes: Set(req.nofailover_sip_codes),
        // Phase 3 Plan 03-01: media_config managed via
        // /api/v1/trunks/{name}/media (Plan 03-04).
        media_config: Set(None),
        is_active: Set(req.is_active),
        metadata: Set(None),
        account_id: Set(scope.account_id.clone()),
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
// Update handler (PUT -- full member replacement + scalar patch)
// ---------------------------------------------------------------------------

async fn update_trunk(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
    Json(req): Json<UpdateTrunkRequest>,
) -> ApiResult<Json<TrunkView>> {
    let db = state.db();

    // Load existing
    let existing = TrunkGroupEntity::find()
        .filter(TrunkGroupColumn::Name.eq(name.clone()))
        .filter(TrunkGroupColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "trunk group '{}' not found",
                name
            ))
        })?;

    // Validate distribution_mode if provided, else keep existing
    let mode = if let Some(ref dm) = req.distribution_mode {
        parse_distribution_mode(Some(dm.as_str()))?
    } else {
        existing.distribution_mode
    };
    validate_distribution_mode(mode)?;

    // Validate direction if provided
    let direction = if let Some(ref d) = req.direction {
        parse_direction(Some(d.as_str()))?
    } else {
        existing.direction
    };

    // Validate gateway refs
    validate_gateway_refs(db, &req.members).await?;

    // Begin transaction
    let tx = db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Build ActiveModel for scalar updates
    let mut am: trunk_group::ActiveModel = existing.clone().into();
    am.direction = Set(direction);
    am.distribution_mode = Set(mode);
    if let Some(v) = req.display_name {
        am.display_name = Set(Some(v));
    }
    // Phase 3 Plan 03-01 (D-02): `credentials` is no longer a
    // trunk_group column — managed via the credentials sub-resource.
    // Phase 5 Plan 05-01 (D-10): `acl` is no longer a trunk_group column —
    // managed via the acl sub-resource (Plan 05-03).
    if let Some(v) = req.nofailover_sip_codes {
        am.nofailover_sip_codes = Set(Some(v));
    }
    if let Some(v) = req.is_active {
        am.is_active = Set(v);
    }
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(&tx)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Delete all existing members
    TrunkMemberEntity::delete_many()
        .filter(TrunkMemberColumn::TrunkGroupId.eq(existing.id))
        .exec(&tx)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Re-insert from request
    for (idx, member) in req.members.iter().enumerate() {
        let member_am = trunk_group_member::ActiveModel {
            trunk_group_id: Set(existing.id),
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

    // Commit
    tx.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Reload members
    let members = TrunkMemberEntity::find()
        .filter(TrunkMemberColumn::TrunkGroupId.eq(updated.id))
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(view_from(updated, members)))
}

// ---------------------------------------------------------------------------
// Delete handler with engagement check (TRK-04)
// ---------------------------------------------------------------------------

/// Check whether any DID or routing record references this trunk group.
///
/// Returns `Err(ApiError::conflict(...))` if a reference is found,
/// preventing deletion.
async fn engagement_check_trunk_group(
    db: &sea_orm::DatabaseConnection,
    name: &str,
) -> ApiResult<()> {
    // TODO(phase-6): replace the routes scan below with an indexed
    // trunk_group_id FK check once RTE-01 lands.

    // Step 1: DIDs scan (indexed via trunk_group_name column)
    let did_ref = DidEntity::find()
        .filter(DidColumn::TrunkGroupName.eq(name))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some(did) = did_ref {
        return Err(ApiError::conflict(format!(
            "trunk group '{}' is referenced by DID '{}' \
             and cannot be deleted",
            name, did.number
        )));
    }

    // Step 2: Routes scan (best-effort JSON scan of target_trunks)
    let routes = RouteEntity::find()
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    for route in &routes {
        if let Some(ref target_trunks) = route.target_trunks {
            let has_ref = match target_trunks {
                serde_json::Value::Array(arr) => {
                    arr.iter().any(|v| v.as_str() == Some(name))
                }
                serde_json::Value::Object(obj) => {
                    obj.values()
                        .any(|v| v.as_str() == Some(name))
                }
                _ => false,
            };
            if has_ref {
                return Err(ApiError::conflict(format!(
                    "trunk group '{}' is referenced by route \
                     '{}' and cannot be deleted",
                    name, route.name
                )));
            }
        }
    }

    Ok(())
}

async fn delete_trunk(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();

    // Load existing
    let existing = TrunkGroupEntity::find()
        .filter(TrunkGroupColumn::Name.eq(name.clone()))
        .filter(TrunkGroupColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "trunk group '{}' not found",
                name
            ))
        })?;

    // Engagement check -- 409 if referenced
    engagement_check_trunk_group(db, &name).await?;

    // Transactional delete: members first, then group
    let tx = db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    TrunkMemberEntity::delete_many()
        .filter(TrunkMemberColumn::TrunkGroupId.eq(existing.id))
        .exec(&tx)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    TrunkGroupEntity::delete_by_id(existing.id)
        .exec(&tx)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
