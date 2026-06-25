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
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
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
    pub status_code: Option<i16>,
    pub hangup_reason: Option<String>,
    /// Failure origin: "sbc" | "upstream" | "caller"; null for a successful or
    /// clean-hangup call. Lets a consumer tell an SBC-side 503 from a carrier
    /// 503 (503-attribution).
    pub failure_source: Option<String>,
    pub started_at: DateTime<Utc>,
    pub answer_time: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_secs: i32,
    /// Answered (billable) seconds; NULL for unanswered calls (task 2.3).
    pub billable_duration_secs: Option<i32>,
    pub from_number: Option<String>,
    pub to_number: Option<String>,
    pub sip_gateway: Option<String>,
    /// Terminating trunk id, for per-trunk attribution/summary.
    pub sip_trunk_id: Option<i64>,
    /// Matched route + terminating extension, for call attribution (task 2.4).
    pub route_id: Option<i64>,
    pub extension_id: Option<i64>,
    pub caller_uri: Option<String>,
    pub callee_uri: Option<String>,
    /// Per-leg SIP role map, lifted from the CDR metadata JSON (task 2.4).
    pub sip_leg_roles: Option<serde_json::Value>,
    pub recording_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Lift the per-leg SIP role map out of the CDR metadata JSON.
/// `persist_call_record` stores it under this reserved key (CDR-07); `None`
/// when absent or metadata is null (task 2.4).
fn leg_roles_from_metadata(metadata: &Option<serde_json::Value>) -> Option<serde_json::Value> {
    metadata
        .as_ref()
        .and_then(|md| md.get("sip_leg_roles").cloned())
}

impl From<CdrModel> for CdrView {
    fn from(m: CdrModel) -> Self {
        let sip_leg_roles = leg_roles_from_metadata(&m.metadata);
        Self {
            id: m.id,
            call_id: m.call_id,
            direction: m.direction,
            status: m.status,
            status_code: m.status_code,
            hangup_reason: m.hangup_reason,
            failure_source: m.failure_source,
            started_at: m.started_at,
            answer_time: m.answer_time,
            ended_at: m.ended_at,
            duration_secs: m.duration_secs,
            billable_duration_secs: m.billable_duration_secs,
            from_number: m.from_number,
            to_number: m.to_number,
            sip_gateway: m.sip_gateway,
            sip_trunk_id: m.sip_trunk_id,
            route_id: m.route_id,
            extension_id: m.extension_id,
            caller_uri: m.caller_uri,
            callee_uri: m.callee_uri,
            sip_leg_roles,
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
    /// Filter by final SIP response code (e.g. 486, 503).
    #[serde(default)]
    pub status_code: Option<i16>,
    /// Filter by failure origin ("sbc" | "upstream" | "caller"), e.g. to count
    /// SBC-side 503s vs carrier 503s (503-attribution).
    #[serde(default)]
    pub failure_source: Option<String>,
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
        .route("/cdrs/summary", get(cdr_summary))
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
    if let Some(v) = q.status_code {
        conds = conds.add(CdrColumn::StatusCode.eq(v));
    }
    if let Some(v) = q.failure_source.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::FailureSource.eq(v.clone()));
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

/// Filters for the aggregated CDR summary. Mirror the list filters plus a
/// per-trunk filter, so a caller gets a per-number summary via `number=` and a
/// per-trunk summary via `sip_trunk_id=`, both over an optional time window.
#[derive(Debug, Deserialize)]
pub struct CdrSummaryQuery {
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub from_number: Option<String>,
    #[serde(default)]
    pub to_number: Option<String>,
    /// Substring match against either from_number or to_number.
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub sip_trunk_id: Option<i64>,
}

/// Aggregated CDR counts: totals plus a per-SIP-code and per-failure-source
/// (sbc vs carrier) breakdown for the filtered window — the data behind a
/// "how many calls, how many failed, how many 503s and whose fault" summary.
#[derive(Debug, Serialize)]
pub struct CdrSummaryResponse {
    pub total: i64,
    pub answered: i64,
    pub failed: i64,
    pub by_status_code: std::collections::BTreeMap<String, i64>,
    pub by_failure_source: std::collections::BTreeMap<String, i64>,
}

#[derive(Debug, FromQueryResult)]
struct StatusCodeCount {
    status_code: Option<i16>,
    cnt: i64,
}

#[derive(Debug, FromQueryResult)]
struct FailureSourceCount {
    failure_source: Option<String>,
    cnt: i64,
}

fn summary_conditions(q: &CdrSummaryQuery) -> Condition {
    let mut conds = Condition::all();
    if let Some(v) = q.direction.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::Direction.eq(v.clone()));
    }
    if let Some(v) = q.from_number.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::FromNumber.like(format!("{}%", v)));
    }
    if let Some(v) = q.to_number.as_ref().filter(|s| !s.is_empty()) {
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
    if let Some(v) = q.sip_trunk_id {
        conds = conds.add(CdrColumn::SipTrunkId.eq(v));
    }
    if let Some(v) = q.start_date {
        conds = conds.add(CdrColumn::StartedAt.gte(v));
    }
    if let Some(v) = q.end_date {
        conds = conds.add(CdrColumn::StartedAt.lte(v));
    }
    conds
}

async fn cdr_summary(
    State(state): State<AppState>,
    Query(q): Query<CdrSummaryQuery>,
) -> ApiResult<Json<CdrSummaryResponse>> {
    let db = state.db();
    let conds = summary_conditions(&q);

    let to_internal = |e: sea_orm::DbErr| ApiError::internal(e.to_string());

    let total = CdrEntity::find()
        .filter(conds.clone())
        .count(db)
        .await
        .map_err(to_internal)? as i64;
    let answered = CdrEntity::find()
        .filter(conds.clone())
        .filter(CdrColumn::Status.is_in(["answered", "completed"]))
        .count(db)
        .await
        .map_err(to_internal)? as i64;

    let code_rows = CdrEntity::find()
        .filter(conds.clone())
        .select_only()
        .column(CdrColumn::StatusCode)
        .column_as(CdrColumn::Id.count(), "cnt")
        .group_by(CdrColumn::StatusCode)
        .into_model::<StatusCodeCount>()
        .all(db)
        .await
        .map_err(to_internal)?;
    let mut by_status_code = std::collections::BTreeMap::new();
    for row in code_rows {
        if let Some(code) = row.status_code {
            by_status_code.insert(code.to_string(), row.cnt);
        }
    }

    let src_rows = CdrEntity::find()
        .filter(conds.clone())
        .select_only()
        .column(CdrColumn::FailureSource)
        .column_as(CdrColumn::Id.count(), "cnt")
        .group_by(CdrColumn::FailureSource)
        .into_model::<FailureSourceCount>()
        .all(db)
        .await
        .map_err(to_internal)?;
    let mut by_failure_source = std::collections::BTreeMap::new();
    for row in src_rows {
        by_failure_source.insert(
            row.failure_source.unwrap_or_else(|| "none".to_string()),
            row.cnt,
        );
    }

    Ok(Json(CdrSummaryResponse {
        total,
        answered,
        failed: total - answered,
        by_status_code,
        by_failure_source,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leg_roles_extracted_from_metadata() {
        let meta = serde_json::json!({
            "sip_leg_roles": {"leg-a": "caller", "leg-b": "callee"},
            "other": "x",
        });
        let roles = leg_roles_from_metadata(&Some(meta)).expect("present");
        assert_eq!(roles["leg-a"], "caller");
        assert_eq!(roles["leg-b"], "callee");
    }

    #[test]
    fn leg_roles_none_when_absent_or_null() {
        assert!(leg_roles_from_metadata(&None).is_none());
        assert!(leg_roles_from_metadata(&Some(serde_json::json!({"other": "x"}))).is_none());
    }
}
