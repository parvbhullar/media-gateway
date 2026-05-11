//! `/api/v1/routes` — read access to the legacy `rustpbx_routes` table.
//!
//! These are the routes the SIP matcher actually consults at runtime
//! (loaded into `RoutesSnapshot` by `data_context.reload_routes`).
//! The Phase-6 `/api/v1/routing/tables` endpoints back a separate,
//! JSON-embedded `supersip_routing_tables` schema that is not yet wired
//! into the matcher — see `routing_tables.rs` for that surface.
//!
//! Read-only for now: list + get. Write operations stay in the console
//! handler (`/console/routing`) until we promote routes to first-class
//! v1 CRUD; doing so requires careful auto-reload semantics matching
//! the DID flow.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

use crate::app::AppState;
use crate::handler::api_v1::common::{Pagination, PaginatedResponse};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::routing::{
    ActiveModel as RouteActive, Column as RouteColumn, Entity as RouteEntity, Model as RouteModel,
    RoutingDirection, RoutingSelectionStrategy,
};

#[derive(Debug, Serialize)]
pub struct RouteView {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub direction: String,
    pub priority: i32,
    pub is_active: bool,
    pub selection_strategy: String,
    pub hash_key: Option<String>,
    pub source_trunk_id: Option<i64>,
    pub default_trunk_id: Option<i64>,
    pub source_pattern: Option<String>,
    pub destination_pattern: Option<String>,
    pub header_filters: Option<Value>,
    pub rewrite_rules: Option<Value>,
    pub target_trunks: Option<Value>,
    pub owner: Option<String>,
    pub notes: Option<Value>,
    pub metadata: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_deployed_at: Option<DateTime<Utc>>,
}

impl From<RouteModel> for RouteView {
    fn from(m: RouteModel) -> Self {
        Self {
            id: m.id,
            name: m.name,
            description: m.description,
            direction: serde_json::to_value(&m.direction)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default(),
            priority: m.priority,
            is_active: m.is_active,
            selection_strategy: serde_json::to_value(&m.selection_strategy)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default(),
            hash_key: m.hash_key,
            source_trunk_id: m.source_trunk_id,
            default_trunk_id: m.default_trunk_id,
            source_pattern: m.source_pattern,
            destination_pattern: m.destination_pattern,
            header_filters: m.header_filters,
            rewrite_rules: m.rewrite_rules,
            target_trunks: m.target_trunks,
            owner: m.owner,
            notes: m.notes,
            metadata: m.metadata,
            created_at: m.created_at,
            updated_at: m.updated_at,
            last_deployed_at: m.last_deployed_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RouteListQuery {
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub page_size: Option<u64>,
    /// Filter by direction string (e.g. `inbound`, `outbound`, `both`).
    #[serde(default)]
    pub direction: Option<String>,
    /// Filter by active flag (`true` / `false`).
    #[serde(default)]
    pub is_active: Option<bool>,
    /// Substring match on name OR description.
    #[serde(default)]
    pub q: Option<String>,
}

impl RouteListQuery {
    fn pagination(&self) -> Pagination {
        Pagination {
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRouteRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub direction: Option<RoutingDirection>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub selection_strategy: Option<RoutingSelectionStrategy>,
    #[serde(default)]
    pub hash_key: Option<String>,
    #[serde(default)]
    pub source_trunk_id: Option<i64>,
    #[serde(default)]
    pub default_trunk_id: Option<i64>,
    #[serde(default)]
    pub source_pattern: Option<String>,
    #[serde(default)]
    pub destination_pattern: Option<String>,
    #[serde(default)]
    pub header_filters: Option<Value>,
    #[serde(default)]
    pub rewrite_rules: Option<Value>,
    #[serde(default)]
    pub target_trunks: Option<Value>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub notes: Option<Value>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// PATCH-style update: fields left unset preserve existing values.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRouteRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub direction: Option<RoutingDirection>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub selection_strategy: Option<RoutingSelectionStrategy>,
    #[serde(default)]
    pub hash_key: Option<Option<String>>,
    #[serde(default)]
    pub source_trunk_id: Option<Option<i64>>,
    #[serde(default)]
    pub default_trunk_id: Option<Option<i64>>,
    #[serde(default)]
    pub source_pattern: Option<Option<String>>,
    #[serde(default)]
    pub destination_pattern: Option<Option<String>>,
    #[serde(default)]
    pub header_filters: Option<Option<Value>>,
    #[serde(default)]
    pub rewrite_rules: Option<Option<Value>>,
    #[serde(default)]
    pub target_trunks: Option<Option<Value>>,
    #[serde(default)]
    pub owner: Option<Option<String>>,
    #[serde(default)]
    pub notes: Option<Option<Value>>,
    #[serde(default)]
    pub metadata: Option<Option<Value>>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/routes", get(list_routes).post(create_route))
        .route(
            "/routes/{id}",
            get(get_route).patch(update_route).delete(delete_route),
        )
}

/// Refresh the in-memory routes snapshot after any mutation so the matcher
/// sees changes immediately. Mirrors the auto-reload behavior already used
/// by `/api/v1/dids`. Errors are logged but don't fail the parent request:
/// the DB write succeeded and the worst case is a stale snapshot until the
/// next manual reload.
async fn refresh_routes_index(state: &AppState) {
    let config_override = state.config_path.as_ref().and_then(|path| {
        crate::config::Config::load(path)
            .ok()
            .map(|cfg| std::sync::Arc::new(cfg.proxy))
    });
    if let Err(e) = state
        .sip_server()
        .inner
        .data_context
        .reload_routes(true, config_override)
        .await
    {
        warn!(error = %e, "auto-reload of routes failed after route mutation");
    }
}

async fn list_routes(
    State(state): State<AppState>,
    Query(q): Query<RouteListQuery>,
) -> ApiResult<Json<PaginatedResponse<RouteView>>> {
    let db = state.db();
    let pagination = q.pagination();
    let page_no = pagination.page.max(1);
    let page_size = pagination.limit();

    let mut conds = Condition::all();
    if let Some(v) = q.direction.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(RouteColumn::Direction.eq(v.clone()));
    }
    if let Some(active) = q.is_active {
        conds = conds.add(RouteColumn::IsActive.eq(active));
    }
    if let Some(needle) = q.q.as_ref().filter(|s| !s.is_empty()) {
        let pat = format!("%{}%", needle);
        conds = conds.add(
            Condition::any()
                .add(RouteColumn::Name.like(pat.clone()))
                .add(RouteColumn::Description.like(pat)),
        );
    }

    let paginator = RouteEntity::find()
        .filter(conds)
        .order_by_asc(RouteColumn::Priority)
        .order_by_asc(RouteColumn::Name)
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
        rows.into_iter().map(RouteView::from).collect(),
        page_no,
        page_size,
        total,
    )))
}

async fn get_route(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<RouteView>> {
    let db = state.db();
    let row = RouteEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("Route {id} not found")))?;
    Ok(Json(RouteView::from(row)))
}

async fn create_route(
    State(state): State<AppState>,
    Json(req): Json<CreateRouteRequest>,
) -> ApiResult<(StatusCode, Json<RouteView>)> {
    let db = state.db();
    let trimmed_name = req.name.trim();
    if trimmed_name.is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }

    // Pre-check duplicate → clean 409 instead of a generic DB UNIQUE violation.
    if let Some(_existing) = RouteEntity::find()
        .filter(RouteColumn::Name.eq(trimmed_name))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    {
        return Err(ApiError::conflict(format!(
            "route with name '{}' already exists",
            trimmed_name
        )));
    }

    let now = Utc::now();
    let active = RouteActive {
        id: ActiveValue::NotSet,
        name: ActiveValue::Set(trimmed_name.to_string()),
        description: ActiveValue::Set(req.description),
        direction: ActiveValue::Set(req.direction.unwrap_or_default()),
        priority: ActiveValue::Set(req.priority.unwrap_or(100)),
        is_active: ActiveValue::Set(req.is_active.unwrap_or(true)),
        selection_strategy: ActiveValue::Set(req.selection_strategy.unwrap_or_default()),
        hash_key: ActiveValue::Set(req.hash_key),
        source_trunk_id: ActiveValue::Set(req.source_trunk_id),
        default_trunk_id: ActiveValue::Set(req.default_trunk_id),
        source_pattern: ActiveValue::Set(req.source_pattern),
        destination_pattern: ActiveValue::Set(req.destination_pattern),
        header_filters: ActiveValue::Set(req.header_filters),
        rewrite_rules: ActiveValue::Set(req.rewrite_rules),
        target_trunks: ActiveValue::Set(req.target_trunks),
        owner: ActiveValue::Set(req.owner),
        notes: ActiveValue::Set(req.notes),
        metadata: ActiveValue::Set(req.metadata),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        last_deployed_at: ActiveValue::Set(None),
    };

    let inserted = active
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    refresh_routes_index(&state).await;

    Ok((StatusCode::CREATED, Json(RouteView::from(inserted))))
}

async fn update_route(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateRouteRequest>,
) -> ApiResult<Json<RouteView>> {
    let db = state.db();

    let existing = RouteEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("Route {id} not found")))?;

    // Renaming requires uniqueness pre-check
    if let Some(ref new_name) = req.name {
        let trimmed = new_name.trim();
        if trimmed.is_empty() {
            return Err(ApiError::bad_request("name must not be empty"));
        }
        if trimmed != existing.name {
            if let Some(_) = RouteEntity::find()
                .filter(RouteColumn::Name.eq(trimmed))
                .one(db)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?
            {
                return Err(ApiError::conflict(format!(
                    "route with name '{}' already exists",
                    trimmed
                )));
            }
        }
    }

    let mut active: RouteActive = existing.into();

    if let Some(name) = req.name {
        active.name = ActiveValue::Set(name.trim().to_string());
    }
    if let Some(description) = req.description {
        active.description = ActiveValue::Set(description);
    }
    if let Some(direction) = req.direction {
        active.direction = ActiveValue::Set(direction);
    }
    if let Some(priority) = req.priority {
        active.priority = ActiveValue::Set(priority);
    }
    if let Some(is_active) = req.is_active {
        active.is_active = ActiveValue::Set(is_active);
    }
    if let Some(strategy) = req.selection_strategy {
        active.selection_strategy = ActiveValue::Set(strategy);
    }
    if let Some(hash_key) = req.hash_key {
        active.hash_key = ActiveValue::Set(hash_key);
    }
    if let Some(v) = req.source_trunk_id {
        active.source_trunk_id = ActiveValue::Set(v);
    }
    if let Some(v) = req.default_trunk_id {
        active.default_trunk_id = ActiveValue::Set(v);
    }
    if let Some(v) = req.source_pattern {
        active.source_pattern = ActiveValue::Set(v);
    }
    if let Some(v) = req.destination_pattern {
        active.destination_pattern = ActiveValue::Set(v);
    }
    if let Some(v) = req.header_filters {
        active.header_filters = ActiveValue::Set(v);
    }
    if let Some(v) = req.rewrite_rules {
        active.rewrite_rules = ActiveValue::Set(v);
    }
    if let Some(v) = req.target_trunks {
        active.target_trunks = ActiveValue::Set(v);
    }
    if let Some(v) = req.owner {
        active.owner = ActiveValue::Set(v);
    }
    if let Some(v) = req.notes {
        active.notes = ActiveValue::Set(v);
    }
    if let Some(v) = req.metadata {
        active.metadata = ActiveValue::Set(v);
    }
    active.updated_at = ActiveValue::Set(Utc::now());

    let updated = active
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    refresh_routes_index(&state).await;

    Ok(Json(RouteView::from(updated)))
}

async fn delete_route(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let db = state.db();

    let existing = RouteEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("Route {id} not found")))?;

    RouteEntity::delete_by_id(existing.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    refresh_routes_index(&state).await;

    Ok(StatusCode::NO_CONTENT)
}
