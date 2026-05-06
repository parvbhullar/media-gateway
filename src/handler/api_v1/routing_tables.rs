//! `/api/v1/routing/tables[/{name}]` — RTE-01 routing-table CRUD surface.
//!
//! Phase 6 Plan 06-02 — GREEN implementation. Replaces the Plan 06-01
//! stub bodies with full CRUD against `supersip_routing_tables`
//! (mongo-style embedded-records column). The `pub fn router()`
//! signature is preserved (Plan 06-01 invariant — no `mod.rs` edit).
//!
//! Endpoints (D-27):
//!   - GET    /routing/tables               — list tables
//!   - POST   /routing/tables               — create table (records optional)
//!   - GET    /routing/tables/{name}        — get one table
//!   - PUT    /routing/tables/{name}        — replace table metadata (NOT records, D-04)
//!   - DELETE /routing/tables/{name}        — delete table (cascade via JSON column)
//!
//! Validation (D-21..D-23, "Claude's Discretion"):
//!   - name: lowercase letters/digits + dashes, 1..=64 chars
//!   - direction: `inbound | outbound | both` (default `both`)
//!   - priority: i32 in `0..=10000` (default 100)
//!   - initial records: ≤ 1000 entries; at-most-one `is_default: true` (D-18)
//!
//! Per-record content-shape validation lives in 06-03; 06-02 only
//! enforces structural caps.

use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder,
    Set,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::common::{CommonScopeQuery, build_account_filter};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::routing_tables::{
    self, Column as RtColumn, Entity as RtEntity, Model as RtModel,
};

// ─── Wire types ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct RoutingTableView {
    pub name: String,
    pub description: Option<String>,
    pub direction: String,
    pub priority: i32,
    pub is_active: bool,
    pub record_count: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&RtModel> for RoutingTableView {
    fn from(m: &RtModel) -> Self {
        let record_count = m
            .records
            .as_array()
            .map(|a| a.len() as u32)
            .unwrap_or(0);
        Self {
            name: m.name.clone(),
            description: m.description.clone(),
            direction: m.direction.clone(),
            priority: m.priority,
            is_active: m.is_active,
            record_count,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl From<RtModel> for RoutingTableView {
    fn from(m: RtModel) -> Self {
        Self::from(&m)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRoutingTableRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub is_active: Option<bool>,
    /// Optional initial records. Per-record content-shape validation is
    /// performed in 06-03; here we only enforce ≤ 1000 entries and at
    /// most one `is_default: true` (D-18).
    #[serde(default)]
    pub records: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRoutingTableRequest {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub is_active: Option<bool>,
    // NO `records` field — records-only-via-records-endpoints (D-04, D-27).
    // `deny_unknown_fields` rejects any client that sends `records` here.
}

// ─── Validation helpers ──────────────────────────────────────────────────

const MAX_RECORDS_PER_TABLE: usize = 1000;
const MAX_NAME_LEN: usize = 64;
const MIN_PRIORITY: i32 = 0;
const MAX_PRIORITY: i32 = 10_000;

/// Lowercase letters/digits plus dashes. First and last char must be
/// alphanumeric. 1..=64 chars. Per "Claude's Discretion" in 06-CONTEXT.md.
fn validate_table_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(format!(
            "routing table name must be 1-{} chars",
            MAX_NAME_LEN
        ));
    }
    let bytes = name.as_bytes();
    let is_alnum_lower = |b: u8| b.is_ascii_digit() || b.is_ascii_lowercase();
    if !is_alnum_lower(bytes[0]) || !is_alnum_lower(bytes[bytes.len() - 1]) {
        return Err(
            "routing table name must start and end with a lowercase \
             alphanumeric character"
                .to_string(),
        );
    }
    for &b in bytes {
        if !is_alnum_lower(b) && b != b'-' {
            return Err(
                "routing table name must contain only lowercase \
                 letters, digits, and dashes"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn validate_direction(d: &str) -> Result<(), String> {
    match d {
        "inbound" | "outbound" | "both" => Ok(()),
        other => Err(format!(
            "invalid direction '{}': must be inbound, outbound, or both",
            other
        )),
    }
}

fn validate_priority(p: i32) -> Result<(), String> {
    if !(MIN_PRIORITY..=MAX_PRIORITY).contains(&p) {
        return Err(format!(
            "priority {} out of range {}..={}",
            p, MIN_PRIORITY, MAX_PRIORITY
        ));
    }
    Ok(())
}

/// Structural validation for the initial records array (D-18 + cap).
/// Per-record content shape (match/target/etc.) is validated by 06-03.
fn validate_records_array(arr: &[Value]) -> Result<(), String> {
    if arr.len() > MAX_RECORDS_PER_TABLE {
        return Err(format!(
            "records array exceeds cap of {} (got {})",
            MAX_RECORDS_PER_TABLE,
            arr.len()
        ));
    }
    let default_count = arr
        .iter()
        .filter(|v| {
            v.get("is_default")
                .and_then(|d| d.as_bool())
                .unwrap_or(false)
        })
        .count();
    if default_count > 1 {
        return Err(
            "at most one record may have is_default = true (D-18)"
                .to_string(),
        );
    }
    Ok(())
}

// ─── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/routing/tables",
            get(list_tables).post(create_table),
        )
        .route(
            "/routing/tables/{name}",
            get(get_table).put(update_table).delete(delete_table),
        )
}

// ─── Handlers ────────────────────────────────────────────────────────────

async fn list_tables(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    axum::extract::Query(scope_q): axum::extract::Query<CommonScopeQuery>,
) -> ApiResult<Json<Vec<RoutingTableView>>> {
    let db = state.db();
    let cond = build_account_filter(&scope, RtColumn::AccountId, &scope_q, Condition::all())?;
    let rows = RtEntity::find()
        .filter(cond)
        .order_by_asc(RtColumn::Name)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let views = rows.iter().map(RoutingTableView::from).collect();
    Ok(Json(views))
}

async fn create_table(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Json(req): Json<CreateRoutingTableRequest>,
) -> ApiResult<(StatusCode, Json<RoutingTableView>)> {
    let db = state.db();

    validate_table_name(&req.name).map_err(ApiError::bad_request)?;

    let direction = req.direction.unwrap_or_else(|| "both".to_string());
    validate_direction(&direction).map_err(ApiError::bad_request)?;

    let priority = req.priority.unwrap_or(100);
    validate_priority(priority).map_err(ApiError::bad_request)?;

    let is_active = req.is_active.unwrap_or(true);

    let records_vec = req.records.unwrap_or_default();
    validate_records_array(&records_vec).map_err(ApiError::bad_request)?;
    let records_json = Value::Array(records_vec);

    // Pre-check duplicate name → 409. (UNIQUE index from Plan 06-01
    // would also catch this, but a pre-check gives a cleaner error and
    // avoids dialect-specific SQLSTATE parsing.)
    let dup = RtEntity::find()
        .filter(RtColumn::Name.eq(req.name.clone()))
        .filter(RtColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "routing table '{}' already exists",
            req.name
        )));
    }

    let now = Utc::now();
    let am = routing_tables::ActiveModel {
        name: Set(req.name.clone()),
        description: Set(req.description),
        direction: Set(direction),
        priority: Set(priority),
        is_active: Set(is_active),
        records: Set(records_json),
        account_id: Set(scope.account_id.clone()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((StatusCode::CREATED, Json(RoutingTableView::from(inserted))))
}

async fn get_table(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
) -> ApiResult<Json<RoutingTableView>> {
    let db = state.db();
    let row = RtEntity::find()
        .filter(RtColumn::Name.eq(name.clone()))
        .filter(RtColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "routing table '{}' not found",
                name
            ))
        })?;
    Ok(Json(RoutingTableView::from(row)))
}

async fn update_table(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
    Json(raw): Json<Value>,
) -> ApiResult<Json<RoutingTableView>> {
    // Parse manually so `deny_unknown_fields` (e.g., a stray `records`
    // key per D-04) maps to 400 rather than axum's default 422.
    let req: UpdateRoutingTableRequest =
        serde_json::from_value(raw).map_err(|e| {
            ApiError::bad_request(format!("invalid request body: {}", e))
        })?;

    let db = state.db();

    let existing = RtEntity::find()
        .filter(RtColumn::Name.eq(name.clone()))
        .filter(RtColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "routing table '{}' not found",
                name
            ))
        })?;

    if let Some(ref dir) = req.direction {
        validate_direction(dir).map_err(ApiError::bad_request)?;
    }
    if let Some(p) = req.priority {
        validate_priority(p).map_err(ApiError::bad_request)?;
    }

    let mut am: routing_tables::ActiveModel = existing.into();
    if let Some(desc) = req.description {
        am.description = Set(Some(desc));
    }
    if let Some(dir) = req.direction {
        am.direction = Set(dir);
    }
    if let Some(p) = req.priority {
        am.priority = Set(p);
    }
    if let Some(act) = req.is_active {
        am.is_active = Set(act);
    }
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(RoutingTableView::from(updated)))
}

async fn delete_table(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let existing = RtEntity::find()
        .filter(RtColumn::Name.eq(name.clone()))
        .filter(RtColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "routing table '{}' not found",
                name
            ))
        })?;

    RtEntity::delete_by_id(existing.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
