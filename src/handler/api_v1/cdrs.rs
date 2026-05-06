//! `/api/v1/cdrs` — carrier-API call detail records (Phase 1, Plan 01-03).
//!
//! Thin JSON adapter over `models::call_record`. Recording and sip-flow
//! sub-routes return 501 per CARRIER-API.md — they are promoted to real
//! handlers in Phase 12 (Recordings first-class).

use std::collections::BTreeMap;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Extension, Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::Response,
    routing::get,
};
use chrono::{DateTime, Utc};
use futures::stream;
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::common::{CommonScopeQuery, PaginatedResponse, build_account_filter};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::call_record::{
    self, Column as CdrColumn, Entity as CdrEntity, Model as CdrModel,
};

/// Phase 11 D-17: FIXED CSV column list for `/cdrs/export`. Changes
/// require a `docs/CARRIER-API.md` update + minor version bump.
/// Derived from `CdrView` field order (no `id`, no DB internals).
const CDR_CSV_COLUMNS: &[&str] = &[
    "call_id",
    "direction",
    "status",
    "from_number",
    "to_number",
    "started_at",
    "ended_at",
    "duration_secs",
    "sip_gateway",
    "caller_uri",
    "callee_uri",
    "recording_url",
    "created_at",
];

/// Phase 11 D-20: hard upper bound on rows streamed by `/cdrs/export`.
/// Beyond this, the handler returns 400 — operator must narrow filters.
const CDR_EXPORT_HARD_CAP: u64 = 1_000_000;

/// Status values surfaced in the `/cdrs/search` `summary.by_status` map.
/// Fixed list — D-13 requires bounded query count (one COUNT per status).
const CDR_SUMMARY_STATUSES: &[&str] = &["answered", "no_answer", "busy", "failed"];

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
    pub trunk: Option<String>,
    #[serde(default)]
    pub from_number: Option<String>,
    #[serde(default)]
    pub to_number: Option<String>,
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_date: Option<DateTime<Utc>>,
}

/// Query for `/cdrs/recent` (D-14). Only `limit` — no filters, no date
/// range. `limit` defaults to 50 and is clamped to 500.
#[derive(Debug, Deserialize)]
pub struct CdrRecentQuery {
    #[serde(default)]
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct PageInfo {
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
}

#[derive(Debug, Serialize)]
pub struct DateRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct SearchSummary {
    pub total: u64,
    pub by_status: BTreeMap<String, u64>,
    pub date_range: Option<DateRange>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub items: Vec<CdrView>,
    pub pagination: PageInfo,
    pub summary: SearchSummary,
}

/// Row shape for CSV export. Field order MUST match [`CDR_CSV_COLUMNS`].
#[derive(Debug, Serialize)]
struct CdrCsvRow {
    call_id: String,
    direction: String,
    status: String,
    from_number: String,
    to_number: String,
    started_at: String,
    ended_at: String,
    duration_secs: i32,
    sip_gateway: String,
    caller_uri: String,
    callee_uri: String,
    recording_url: String,
    created_at: String,
}

impl From<CdrModel> for CdrCsvRow {
    fn from(m: CdrModel) -> Self {
        Self {
            call_id: m.call_id,
            direction: m.direction,
            status: m.status,
            from_number: m.from_number.unwrap_or_default(),
            to_number: m.to_number.unwrap_or_default(),
            started_at: m.started_at.to_rfc3339(),
            ended_at: m.ended_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
            duration_secs: m.duration_secs,
            sip_gateway: m.sip_gateway.unwrap_or_default(),
            caller_uri: m.caller_uri.unwrap_or_default(),
            callee_uri: m.callee_uri.unwrap_or_default(),
            recording_url: m.recording_url.unwrap_or_default(),
            created_at: m.created_at.to_rfc3339(),
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/cdrs", get(list_cdrs))
        // Phase 11 additions: literal segments BEFORE the `{id}` capture so
        // they are not mis-routed as numeric IDs.
        .route("/cdrs/search", get(search_cdrs))
        .route("/cdrs/recent", get(recent_cdrs))
        .route("/cdrs/export", get(handle_export))
        .route("/cdrs/{id}", get(get_cdr).delete(delete_cdr))
        .route("/cdrs/{id}/recording", get(cdr_recording_stub))
        .route("/cdrs/{id}/sip-flow", get(cdr_sip_flow_stub))
}

/// Build the SeaORM `Condition` for the shared CDR filter set. Used by
/// `list_cdrs`, `search_cdrs`, and `handle_export` (DRY).
/// Phase 12: widened to `pub(super)` so `handler::api_v1::recordings`
/// can reuse the same filter set (D-10).
/// Phase 13: account_id scope is applied separately via `build_account_filter`.
pub(super) fn build_cdr_filter(q: &CdrListQuery) -> Condition {
    let mut conds = Condition::all();
    if let Some(v) = q.direction.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::Direction.eq(v.clone()));
    }
    if let Some(v) = q.status.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::Status.eq(v.clone()));
    }
    if let Some(v) = q.trunk.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::SipGateway.eq(v.clone()));
    }
    if let Some(v) = q.from_number.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::FromNumber.eq(v.clone()));
    }
    if let Some(v) = q.to_number.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::ToNumber.eq(v.clone()));
    }
    if let Some(v) = q.start_date {
        conds = conds.add(CdrColumn::StartedAt.gte(v));
    }
    if let Some(v) = q.end_date {
        conds = conds.add(CdrColumn::StartedAt.lte(v));
    }
    conds
}

/// D-20 hard-cap helper. Pure function — testable without a DB.
fn check_export_cap(total: u64) -> ApiResult<()> {
    if total > CDR_EXPORT_HARD_CAP {
        Err(ApiError::bad_request(format!(
            "export exceeds {} row limit; narrow filters and retry",
            CDR_EXPORT_HARD_CAP
        )))
    } else {
        Ok(())
    }
}

async fn list_cdrs(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(scope_q): Query<CommonScopeQuery>,
    Query(q): Query<CdrListQuery>,
) -> ApiResult<Json<PaginatedResponse<CdrView>>> {
    let db = state.db();
    let page_no = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(20).clamp(1, 200);

    let conds = build_account_filter(
        &scope,
        CdrColumn::AccountId,
        &scope_q,
        build_cdr_filter(&q),
    )?;

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
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<i64>,
) -> ApiResult<Json<CdrView>> {
    let db = state.db();
    let row = CdrEntity::find_by_id(id)
        .filter(CdrColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("CDR {id} not found")))?;
    Ok(Json(CdrView::from(row)))
}

async fn delete_cdr(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let row = CdrEntity::find_by_id(id)
        .filter(CdrColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("CDR {id} not found")))?;
    call_record::Entity::delete_by_id(row.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// CDR-05: filter + paginated results + status breakdown summary.
async fn search_cdrs(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(scope_q): Query<CommonScopeQuery>,
    Query(q): Query<CdrListQuery>,
) -> ApiResult<Json<SearchResponse>> {
    let db = state.db();
    let page_no = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(50).clamp(1, 200);

    let conds = build_account_filter(
        &scope,
        CdrColumn::AccountId,
        &scope_q,
        build_cdr_filter(&q),
    )?;

    let paginator = CdrEntity::find()
        .filter(conds.clone())
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

    // D-13: bounded fan-out — one COUNT per fixed status. Run them in
    // parallel via try_join_all so total wall-time stays one round-trip.
    let counts = futures::future::try_join_all(CDR_SUMMARY_STATUSES.iter().map(|s| {
        let conds_for_status = conds.clone().add(CdrColumn::Status.eq((*s).to_string()));
        let db_ref = db;
        async move {
            CdrEntity::find()
                .filter(conds_for_status)
                .count(db_ref)
                .await
        }
    }))
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;

    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    for (status, count) in CDR_SUMMARY_STATUSES.iter().zip(counts.into_iter()) {
        by_status.insert((*status).to_string(), count);
    }

    let date_range = match (q.start_date, q.end_date) {
        (Some(from), Some(to)) => Some(DateRange { from, to }),
        _ => None,
    };

    let items: Vec<CdrView> = rows.into_iter().map(CdrView::from).collect();
    Ok(Json(SearchResponse {
        items,
        pagination: PageInfo {
            page: page_no,
            page_size,
            total,
        },
        summary: SearchSummary {
            total,
            by_status,
            date_range,
        },
    }))
}

/// CDR-06: most-recent CDRs ordered by `created_at DESC`. No filters.
async fn recent_cdrs(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(q): Query<CdrRecentQuery>,
) -> ApiResult<Json<PaginatedResponse<CdrView>>> {
    let db = state.db();
    let limit = q.limit.unwrap_or(50).clamp(1, 500);

    let rows = CdrEntity::find()
        .filter(CdrColumn::AccountId.eq(scope.account_id.clone()))
        .order_by_desc(CdrColumn::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let total = rows.len() as u64;
    let items: Vec<CdrView> = rows.into_iter().map(CdrView::from).collect();
    Ok(Json(PaginatedResponse::new(items, 1, limit, total)))
}

/// CDR-07: streaming CSV export with 1M-row guard. Bounded memory: at
/// most 500 rows resident at once via `paginate(db, 500).fetch_page(n)`.
async fn handle_export(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(scope_q): Query<CommonScopeQuery>,
    Query(q): Query<CdrListQuery>,
) -> Result<Response, ApiError> {
    let db = state.db().clone();
    let conds = build_account_filter(
        &scope,
        CdrColumn::AccountId,
        &scope_q,
        build_cdr_filter(&q),
    )?;

    // D-20: pre-flight count with the same filter set.
    let total = CdrEntity::find()
        .filter(conds.clone())
        .count(&db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    check_export_cap(total)?;

    // Async stream of CSV byte chunks. State tuple: (db, conds, page,
    // header_emitted). Header is yielded as the first chunk, then 500-row
    // pages until the paginator returns an empty page.
    let stream = stream::unfold(
        (db, conds, 0u64, false),
        |(db, conds, page, header_sent)| async move {
            if !header_sent {
                let mut w = csv::Writer::from_writer(Vec::<u8>::new());
                if w.write_record(CDR_CSV_COLUMNS).is_err() {
                    return None;
                }
                let buf = match w.into_inner() {
                    Ok(b) => b,
                    Err(_) => return None,
                };
                let bytes = Bytes::from(buf);
                return Some((
                    Ok::<_, std::io::Error>(bytes),
                    (db, conds, 0u64, true),
                ));
            }

            let paginator = CdrEntity::find()
                .filter(conds.clone())
                .order_by_desc(CdrColumn::StartedAt)
                .paginate(&db, 500);
            let rows = match paginator.fetch_page(page).await {
                Ok(r) => r,
                Err(_) => return None,
            };
            if rows.is_empty() {
                return None;
            }

            let mut w = csv::Writer::from_writer(Vec::<u8>::new());
            // Skip the auto-header (we emitted it manually above).
            for row in rows {
                if w.serialize(CdrCsvRow::from(row)).is_err() {
                    return None;
                }
            }
            let buf = match w.into_inner() {
                Ok(b) => b,
                Err(_) => return None,
            };
            // Strip the per-chunk header line that csv::Writer prepends
            // when serializing structs. csv writes one header row per
            // Writer instance; we only want it once.
            let bytes = Bytes::from(strip_csv_header(&buf));
            Some((Ok(bytes), (db, conds, page + 1, true)))
        },
    );

    let filename = format!("cdrs-{}.csv", Utc::now().format("%Y-%m-%d"));
    let body = Body::from_stream(stream);
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", filename))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"cdrs.csv\"")),
    );
    Ok(resp)
}

/// Strip the leading header line that `csv::Writer::serialize` emits per
/// instance. The handler emits one header chunk explicitly; subsequent
/// chunks must contain only data rows.
fn strip_csv_header(buf: &[u8]) -> Vec<u8> {
    if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
        buf[pos + 1..].to_vec()
    } else {
        Vec::new()
    }
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
    fn check_export_cap_allows_under_limit() {
        assert!(check_export_cap(0).is_ok());
        assert!(check_export_cap(1).is_ok());
        assert!(check_export_cap(CDR_EXPORT_HARD_CAP).is_ok());
    }

    #[test]
    fn check_export_cap_rejects_above_limit() {
        let err = check_export_cap(CDR_EXPORT_HARD_CAP + 1).expect_err("must reject");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains("1000000"),
            "message should reference the hard cap, got: {}",
            err.message
        );
    }

    #[test]
    fn strip_csv_header_drops_first_line() {
        let buf = b"a,b\n1,2\n3,4\n";
        let stripped = strip_csv_header(buf);
        assert_eq!(stripped, b"1,2\n3,4\n".to_vec());
    }

    #[test]
    fn cdr_csv_columns_are_fixed_order() {
        // Lock the column list at build time. Changing this list requires
        // a docs/CARRIER-API.md update + minor version bump (D-17).
        assert_eq!(
            CDR_CSV_COLUMNS,
            &[
                "call_id",
                "direction",
                "status",
                "from_number",
                "to_number",
                "started_at",
                "ended_at",
                "duration_secs",
                "sip_gateway",
                "caller_uri",
                "callee_uri",
                "recording_url",
                "created_at",
            ]
        );
    }
}
