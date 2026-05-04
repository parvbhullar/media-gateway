//! `/api/v1/recordings` -- first-class recordings surface (Phase 12,
//! REC-01..REC-07). Recordings are CDR rows (rustpbx_call_records) with a
//! non-null `recording_url`. No new storage layer -- REC-07 honored at the
//! leanest interpretation per CONTEXT.md D-07.
//!
//! Route ordering: literal segments (/export, /bulk) must be registered
//! BEFORE the {id} capture. /export and /bulk are added in 12-03.

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::Response,
    routing::get,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
};
use sea_orm::sea_query::Expr;
use serde::Serialize;
use tokio_util::io::ReaderStream;
use tracing::warn;

use crate::app::AppState;
use crate::callrecord::storage;
use crate::handler::api_v1::cdrs::{CdrListQuery, PageInfo, build_cdr_filter};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::call_record::{Column as CdrColumn, Entity as CdrEntity, Model as CdrModel};

// TODO(phase-13): add recording_size_bytes when column is added to
// rustpbx_call_records. It is absent from the schema in v2.0 so it is
// intentionally omitted here to avoid phantom null fields in the response.
#[derive(Debug, Serialize)]
pub struct RecordingView {
    pub id: i64,
    pub call_id: String,
    /// sip_gateway column -- D-09 names this field "trunk".
    pub trunk: Option<String>,
    pub direction: String,
    /// from_number column -- D-09 names this field "caller".
    pub caller: Option<String>,
    /// to_number column -- D-09 names this field "callee".
    pub callee: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_secs: i32,
    pub status: String,
    /// Always non-null in recordings responses (list filters NOT NULL, get validates).
    pub recording_url: String,
    /// "local" when url has no http/s3 prefix; "remote" otherwise.
    /// Derived per-row from url shape -- NOT from global CdrStorage::is_local().
    pub recording_storage: &'static str,
    pub recording_duration_secs: Option<i32>,
}

impl From<CdrModel> for RecordingView {
    fn from(m: CdrModel) -> Self {
        let url = m.recording_url.unwrap_or_default();
        let storage_kind = recording_storage_from_url(&url);
        Self {
            id: m.id,
            call_id: m.call_id,
            trunk: m.sip_gateway,
            direction: m.direction,
            caller: m.from_number,
            callee: m.to_number,
            started_at: m.started_at,
            ended_at: m.ended_at,
            duration_secs: m.duration_secs,
            status: m.status,
            recording_url: url,
            recording_storage: storage_kind,
            recording_duration_secs: m.recording_duration_secs,
        }
    }
}

/// Per-row storage classification. Derived from URL shape so it remains
/// accurate for mixed-storage deployments (D-09 / planner decision).
fn recording_storage_from_url(url: &str) -> &'static str {
    if url.starts_with("http") || url.starts_with("s3://") {
        "remote"
    } else {
        "local"
    }
}

/// Content-Type header for a local recording file (D-12 open question).
/// Extension matching is case-insensitive.
fn mime_for_ext(path: &std::path::Path) -> HeaderValue {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("wav")  => HeaderValue::from_static("audio/wav"),
        Some("opus") => HeaderValue::from_static("audio/opus"),
        Some("ogg")  => HeaderValue::from_static("audio/ogg"),
        Some("mp3")  => HeaderValue::from_static("audio/mpeg"),
        _            => HeaderValue::from_static("application/octet-stream"),
    }
}

#[derive(Debug, Serialize)]
pub struct RecordingListResponse {
    pub items: Vec<RecordingView>,
    pub pagination: PageInfo,
}

pub fn router() -> Router<AppState> {
    Router::new()
        // Literal segments first -- /export and /bulk added by 12-03.
        .route("/recordings", get(list_recordings))
        .route("/recordings/{id}", get(get_recording).delete(delete_recording))
        .route("/recordings/{id}/download", get(handle_download))
}

/// REC-01: paginated list filtered to recording_url IS NOT NULL.
/// Defaults: page=1, page_size=50, max 200. Shares CdrListQuery filter set (D-10).
async fn list_recordings(
    State(state): State<AppState>,
    Query(q): Query<CdrListQuery>,
) -> ApiResult<Json<RecordingListResponse>> {
    let db = state.db();
    let page_no = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(50).clamp(1, 200);

    let conds = build_cdr_filter(&q)
        .add(CdrColumn::RecordingUrl.is_not_null());

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

    Ok(Json(RecordingListResponse {
        items: rows.into_iter().map(RecordingView::from).collect(),
        pagination: PageInfo { page: page_no, page_size, total },
    }))
}

/// REC-02: single recording by CDR id (D-08: recording id == CDR id).
/// Returns 404 when row missing OR recording_url IS NULL.
async fn get_recording(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<RecordingView>> {
    let db = state.db();
    let row = CdrEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("recording {id} not found")))?;

    if row.recording_url.is_none() {
        return Err(ApiError::not_found(format!(
            "CDR {id} exists but has no recording"
        )));
    }
    Ok(Json(RecordingView::from(row)))
}

/// REC-03: download a recording.
///
/// - Remote (http/s3://): 302 Found with Location header. No proxying (D-12).
/// - Local: stream via ReaderStream with Content-Type and Content-Disposition (D-12).
/// - Missing local file: 410 Gone via ApiError::gone() (D-14).
/// - No Range support in v2.0 (D-13, deferred to v2.1).
async fn handle_download(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let db = state.db();
    let row = CdrEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("recording {id} not found")))?;

    let url = row
        .recording_url
        .ok_or_else(|| ApiError::not_found(format!("CDR {id} has no recording")))?;

    // Remote recordings: 302 Found redirect -- never proxy bandwidth (D-12).
    // axum 0.8 Redirect has no 302 constructor (to()=303, temporary()=307).
    // Build a raw 302 response so the Location header behaviour is transparent
    // to clients that distinguish 302 from 307.
    if url.starts_with("http") || url.starts_with("s3://") {
        let location = HeaderValue::try_from(url.as_str())
            .map_err(|e| ApiError::internal(format!("invalid redirect URL: {e}")))?;
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = StatusCode::FOUND;
        resp.headers_mut().insert(header::LOCATION, location);
        return Ok(resp);
    }

    // Local recordings: resolve storage and stream from disk.
    let cdr_storage = storage::resolve_storage(state.config().callrecord.as_ref())
        .map_err(|e| ApiError::internal(format!("storage config error: {e}")))?
        .ok_or_else(|| ApiError::internal("callrecord storage not configured"))?;

    let path = cdr_storage
        .local_full_path(&url)
        .ok_or_else(|| ApiError::internal("storage layout error: cannot resolve local path"))?;

    let file = tokio::fs::File::open(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            // CDR row exists, file does not -- 410 not 404 (D-14).
            ApiError::gone(format!(
                "recording file missing from disk (CDR {} exists): {}",
                id, path.display()
            ))
        } else {
            ApiError::internal(format!("cannot open recording: {e}"))
        }
    })?;

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("bin");
    let filename = format!("{}.{}", row.call_id, ext);
    let content_type = mime_for_ext(&path);
    let disposition =
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", filename))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment"));

    let body = Body::from_stream(ReaderStream::new(file));
    let mut resp = Response::new(body);
    resp.headers_mut().insert(header::CONTENT_TYPE, content_type);
    resp.headers_mut().insert(header::CONTENT_DISPOSITION, disposition);
    Ok(resp)
}

/// REC-04: hard delete.
///
/// Steps (D-15/D-16):
/// 1. Load row, verify it has recording_url.
/// 2. Remote URL: log WARN, skip file delete.
///    Local URL: best-effort tokio::fs::remove_file (log WARN on error, do not abort).
/// 3. update_many: set recording_url = NULL (CDR row REMAINS -- billing/audit trail).
/// 4. Return 204 No Content.
async fn delete_recording(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let row = CdrEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("recording {id} not found")))?;

    let url = row
        .recording_url
        .ok_or_else(|| ApiError::not_found(format!("CDR {id} has no recording")))?;

    if url.starts_with("http") || url.starts_with("s3://") {
        // Remote: out of scope to delete from S3 (D-16). Log for operator.
        warn!(
            recording_id = id,
            url = %url,
            "recording_url is remote; skipping file delete (D-16). \
             Operator must reconcile S3 object manually."
        );
    } else {
        // Local: best-effort removal. Continue to clear DB column even on error.
        match storage::resolve_storage(state.config().callrecord.as_ref()) {
            Ok(Some(cdr_storage)) => {
                if let Some(path) = cdr_storage.local_full_path(&url) {
                    if let Err(e) = tokio::fs::remove_file(&path).await {
                        warn!(
                            recording_id = id,
                            path = %path.display(),
                            "best-effort file removal failed: {e} \
                             (continuing to clear recording_url column)"
                        );
                    }
                }
            }
            Ok(None) => {
                warn!(
                    recording_id = id,
                    "no callrecord storage configured; skipping file delete"
                );
            }
            Err(e) => {
                warn!(
                    recording_id = id,
                    "storage error during delete: {e}; skipping file delete"
                );
            }
        }
    }

    // Clear recording_url column regardless of file-delete outcome (D-15).
    CdrEntity::update_many()
        .col_expr(CdrColumn::RecordingUrl, Expr::value(None::<String>))
        .col_expr(CdrColumn::UpdatedAt, Expr::value(chrono::Utc::now()))
        .filter(CdrColumn::Id.eq(id))
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn recording_storage_from_url_classifies_remote() {
        assert_eq!(
            recording_storage_from_url("https://s3.example.com/bucket/file.wav"),
            "remote"
        );
        assert_eq!(
            recording_storage_from_url("http://cdn.example.com/file.wav"),
            "remote"
        );
        assert_eq!(
            recording_storage_from_url("s3://mybucket/path/file.wav"),
            "remote"
        );
    }

    #[test]
    fn recording_storage_from_url_classifies_local() {
        assert_eq!(recording_storage_from_url("/var/cdr/20260101/abc.wav"), "local");
        assert_eq!(recording_storage_from_url("cdr/20260101/file.ogg"), "local");
        assert_eq!(recording_storage_from_url(""), "local");
    }

    #[test]
    fn mime_for_ext_maps_known_extensions() {
        assert_eq!(mime_for_ext(Path::new("call.wav")), "audio/wav");
        assert_eq!(mime_for_ext(Path::new("call.opus")), "audio/opus");
        assert_eq!(mime_for_ext(Path::new("call.ogg")), "audio/ogg");
        assert_eq!(mime_for_ext(Path::new("call.mp3")), "audio/mpeg");
        assert_eq!(
            mime_for_ext(Path::new("call.amr")),
            "application/octet-stream"
        );
        assert_eq!(
            mime_for_ext(Path::new("noextension")),
            "application/octet-stream"
        );
    }
}
