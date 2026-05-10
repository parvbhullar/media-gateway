//! `/api/v1/cdrs` — carrier-API call detail records (Phase 1, Plan 01-03).
//!
//! Thin JSON adapter over `models::call_record`. Recording and sip-flow
//! sub-routes return 501 per CARRIER-API.md — they are promoted to real
//! handlers in Phase 12 (Recordings first-class).

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
use crate::models::call_record::{
    self, Column as CdrColumn, Entity as CdrEntity, Model as CdrModel,
};

#[derive(Debug, Serialize)]
pub struct CdrView {
    pub id: i64,
    pub call_id: String,
    pub direction: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_secs: i32,
    pub from_number: Option<String>,
    pub to_number: Option<String>,
    pub sip_gateway: Option<String>,
    pub caller_uri: Option<String>,
    pub callee_uri: Option<String>,
    pub recording_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<CdrModel> for CdrView {
    fn from(m: CdrModel) -> Self {
        Self {
            id: m.id,
            call_id: m.call_id,
            direction: m.direction,
            status: m.status,
            started_at: m.started_at,
            ended_at: m.ended_at,
            duration_secs: m.duration_secs,
            from_number: m.from_number,
            to_number: m.to_number,
            sip_gateway: m.sip_gateway,
            caller_uri: m.caller_uri,
            callee_uri: m.callee_uri,
            recording_url: m.recording_url,
            created_at: m.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CdrListQuery {
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub page_size: Option<u64>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub from_number: Option<String>,
    #[serde(default)]
    pub to_number: Option<String>,
    /// Substring match against either from_number or to_number.
    /// Mutually inclusive with `from_number`/`to_number` — all provided
    /// filters AND together. Use this when you don't care which side.
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_date: Option<DateTime<Utc>>,
}

impl CdrListQuery {
    fn pagination(&self) -> Pagination {
        Pagination {
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/cdrs", get(list_cdrs))
        .route("/cdrs/{id}", get(get_cdr).delete(delete_cdr))
        .route("/cdrs/{id}/recording", get(cdr_recording_stub))
        .route("/cdrs/{id}/sip-flow", get(cdr_sip_flow_stub))
}

async fn list_cdrs(
    State(state): State<AppState>,
    Query(q): Query<CdrListQuery>,
) -> ApiResult<Json<PaginatedResponse<CdrView>>> {
    let db = state.db();
    let pagination = q.pagination();
    let page_no = pagination.page.max(1);
    let page_size = pagination.limit();

    let mut conds = Condition::all();
    if let Some(v) = q.direction.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::Direction.eq(v.clone()));
    }
    if let Some(v) = q.status.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::Status.eq(v.clone()));
    }
    if let Some(v) = q.from_number.as_ref().filter(|s| !s.is_empty()) {
        // Prefix match — matches Postman doc ("Filter by caller number prefix").
        conds = conds.add(CdrColumn::FromNumber.like(format!("{}%", v)));
    }
    if let Some(v) = q.to_number.as_ref().filter(|s| !s.is_empty()) {
        // Prefix match — matches Postman doc ("Filter by callee number prefix").
        conds = conds.add(CdrColumn::ToNumber.like(format!("{}%", v)));
    }
    if let Some(v) = q.number.as_ref().filter(|s| !s.is_empty()) {
        let pat = format!("%{}%", v);
        conds = conds.add(
            Condition::any()
                .add(CdrColumn::FromNumber.like(pat.clone()))
                .add(CdrColumn::ToNumber.like(pat)),
        );
    }
    if let Some(v) = q.start_date {
        conds = conds.add(CdrColumn::StartedAt.gte(v));
    }
    if let Some(v) = q.end_date {
        conds = conds.add(CdrColumn::StartedAt.lte(v));
    }

    let paginator = CdrEntity::find()
        .filter(conds)
        .order_by_desc(CdrColumn::StartedAt)
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
        rows.into_iter().map(CdrView::from).collect(),
        page_no,
        page_size,
        total,
    )))
}

async fn get_cdr(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<CdrView>> {
    let db = state.db();
    let row = CdrEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("CDR {id} not found")))?;
    Ok(Json(CdrView::from(row)))
}

async fn delete_cdr(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let outcome = call_record::Entity::delete_by_id(id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if outcome.rows_affected == 0 {
        return Err(ApiError::not_found(format!("CDR {id} not found")));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn cdr_recording_stub(Path(_id): Path<i64>) -> ApiResult<StatusCode> {
    Err(ApiError::not_implemented(
        "recording retrieval not implemented",
    ))
}

async fn cdr_sip_flow_stub(Path(_id): Path<i64>) -> ApiResult<StatusCode> {
    Err(ApiError::not_implemented(
        "sip flow retrieval not implemented",
    ))
}
