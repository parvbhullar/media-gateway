//! `/api/v1/recordings` -- first-class recordings surface (Phase 12,
//! REC-01..REC-07). Recordings are CDR rows (rustpbx_call_records) with a
//! non-null `recording_url`. No new storage layer -- REC-07 honored at the
//! leanest interpretation per CONTEXT.md D-07.
//!
//! Route ordering: literal segments (/export, /bulk) must be registered
//! BEFORE the {id} capture. /export and /bulk are added in 12-03.

use async_zip::base::write::ZipFileWriter;
use async_zip::{Compression, ZipEntryBuilder};
use axum::{
    Json, Router,
    body::Body,
    extract::{Extension, Json as ExtractJson, Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
};
use sea_orm::sea_query::Expr;
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;
use tracing::warn;

use crate::app::AppState;
use crate::callrecord::storage;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::cdrs::{CdrListQuery, PageInfo, build_cdr_filter};
use crate::handler::api_v1::common::{CommonScopeQuery, build_account_filter};
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
        // Literal segments registered before {id} capture (axum routing requirement).
        .route("/recordings", get(list_recordings))
        .route("/recordings/export", post(handle_export))       // REC-05, D-17
        .route("/recordings/bulk",   delete(handle_bulk_delete)) // REC-06, D-22
        .route("/recordings/{id}", get(get_recording).delete(delete_recording))
        .route("/recordings/{id}/download", get(handle_download))
}

/// REC-01: paginated list filtered to recording_url IS NOT NULL.
/// Defaults: page=1, page_size=50, max 200. Shares CdrListQuery filter set (D-10).
async fn list_recordings(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(scope_q): Query<CommonScopeQuery>,
    Query(q): Query<CdrListQuery>,
) -> ApiResult<Json<RecordingListResponse>> {
    let db = state.db();
    let page_no = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(50).clamp(1, 200);

    let account_cond = build_account_filter(&scope, CdrColumn::AccountId, &scope_q, Condition::all())?;
    let conds = build_cdr_filter(&q)
        .add(CdrColumn::RecordingUrl.is_not_null())
        .add(account_cond);

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
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<i64>,
) -> ApiResult<Json<RecordingView>> {
    let db = state.db();
    let row = CdrEntity::find()
        .filter(CdrColumn::Id.eq(id))
        .filter(CdrColumn::AccountId.eq(scope.account_id.clone()))
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
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let db = state.db();
    let row = CdrEntity::find()
        .filter(CdrColumn::Id.eq(id))
        .filter(CdrColumn::AccountId.eq(scope.account_id.clone()))
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

// ---------------------------------------------------------------------------
// Constants and structs for export (REC-05) and bulk delete (REC-06)
// ---------------------------------------------------------------------------

/// D-19: hard cap on recordings per export call. Exceeding this returns
/// 400 -- operator must narrow filters. Same DoS-guard pattern as CDR
/// export (CDR_EXPORT_HARD_CAP = 1_000_000 in cdrs.rs).
const RECORDINGS_EXPORT_HARD_CAP: u64 = 10_000;

/// D-24 bulk delete guardrail message. Wording is part of the operator UX contract.
const BULK_DELETE_CONFIRM_MSG: &str =
    "Add ?confirm=true to execute. Without it this is a dry-run preview. \
     WARNING: bulk delete with no filters + confirm=true will wipe ALL recordings.";

/// Optional JSON body for /recordings/export (D-18).
/// When `ids` is non-empty it overrides filter query params (mutually exclusive).
#[derive(Debug, Deserialize, Default)]
struct ExportBody {
    #[serde(default)]
    ids: Vec<i64>,
}

/// Query struct for DELETE /recordings/bulk (D-22/D-24).
#[derive(Debug, Deserialize)]
struct BulkDeleteQuery {
    #[serde(flatten)]
    filters: CdrListQuery,
    #[serde(default)]
    confirm: Option<bool>,
}

#[derive(Serialize)]
struct BulkDeletePreview {
    matched: u64,
    would_delete: u64,
}

#[derive(Serialize)]
struct BulkDeletePreviewResponse {
    preview: BulkDeletePreview,
    message: &'static str,
}

#[derive(Serialize)]
struct BulkDeleteResult {
    deleted: u64,
}

/// D-19: pre-flight cap check for export. Pure function -- testable without DB.
fn check_recordings_export_cap(total: u64) -> ApiResult<()> {
    if total > RECORDINGS_EXPORT_HARD_CAP {
        Err(ApiError::bad_request(format!(
            "export exceeds {} recording limit; narrow filters (date range, trunk, status) and retry",
            RECORDINGS_EXPORT_HARD_CAP
        )))
    } else {
        Ok(())
    }
}

/// REC-05: stream a ZIP archive of recordings matching the filter set.
///
/// D-17: each local file -> "<cdr_id>_<call_id>.<ext>" in the ZIP.
/// D-18: optional JSON body { "ids": [...] } overrides filter query params.
/// D-19: hard cap of 10,000 rows; returns 400 if exceeded.
/// D-20: Content-Type: application/zip, Content-Disposition with ISO date.
/// D-21: remote recordings are skipped; their ids/urls go in MANIFEST.json.
///
/// Implementation note: uses Vec<u8> accumulation (bounded by 10k cap).
/// async_zip 0.0.17 API: ZipFileWriter::with_tokio(cursor) where cursor is
/// std::io::Cursor<Vec<u8>> (implements tokio::io::AsyncWrite). close() returns
/// the inner cursor; use .into_inner() twice to recover the Vec<u8>.
async fn handle_export(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(scope_q): Query<CommonScopeQuery>,
    Query(q): Query<CdrListQuery>,
    body: Option<ExtractJson<ExportBody>>,
) -> Result<Response, ApiError> {
    let db = state.db();

    let account_cond = build_account_filter(&scope, CdrColumn::AccountId, &scope_q, Condition::all())?;

    // Build condition: explicit ids override filter params (D-18).
    let ids: Vec<i64> = body.map(|b| b.0.ids).unwrap_or_default();
    let conds = if !ids.is_empty() {
        Condition::all()
            .add(CdrColumn::Id.is_in(ids))
            .add(account_cond)
    } else {
        build_cdr_filter(&q)
            .add(CdrColumn::RecordingUrl.is_not_null())
            .add(account_cond)
    };

    // D-19: pre-flight count.
    let total = CdrEntity::find()
        .filter(conds.clone())
        .count(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    check_recordings_export_cap(total)?;

    // Fetch all matched rows (bounded by cap).
    let rows = CdrEntity::find()
        .filter(conds)
        .order_by_desc(CdrColumn::StartedAt)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Build ZIP in memory using std::io::Cursor<Vec<u8>> as the backing store.
    // async_zip 0.0.17: with_tokio() takes a T: tokio::io::AsyncWrite + Unpin;
    // Cursor<Vec<u8>> satisfies this. close() consumes the writer and returns
    // Result<Cursor<Vec<u8>>>; .into_inner() recovers the Vec<u8>.
    let cursor = std::io::Cursor::new(Vec::<u8>::new());
    let mut zip = ZipFileWriter::with_tokio(cursor);

    let cdr_storage = storage::resolve_storage(state.config().callrecord.as_ref())
        .map_err(|e| ApiError::internal(format!("storage config error: {e}")))?;

    let mut exported: Vec<String> = Vec::new();
    let mut skipped_remote: Vec<serde_json::Value> = Vec::new();

    for row in rows {
        let url = match &row.recording_url {
            Some(u) => u.clone(),
            None => continue,
        };

        // D-21: skip remote recordings -- add to manifest.
        if url.starts_with("http") || url.starts_with("s3://") {
            skipped_remote.push(serde_json::json!({
                "id": row.id,
                "url": url,
            }));
            continue;
        }

        // Local recording: read bytes and add to ZIP entry.
        let ext = std::path::Path::new(&url)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let zip_name = format!("{}_{}.{}", row.id, row.call_id, ext);

        let path = match cdr_storage.as_ref().and_then(|s| s.local_full_path(&url)) {
            Some(p) => p,
            None => {
                warn!(recording_id = row.id, "cannot resolve local path for export; skipping");
                continue;
            }
        };

        let file_bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    recording_id = row.id,
                    path = %path.display(),
                    "cannot read file for export: {e}; skipping"
                );
                continue;
            }
        };

        let entry = ZipEntryBuilder::new(zip_name.clone().into(), Compression::Deflate);
        zip.write_entry_whole(entry, &file_bytes)
            .await
            .map_err(|e| ApiError::internal(format!("zip write error: {e}")))?;
        exported.push(zip_name);
    }

    // D-21: MANIFEST.json as the final ZIP entry.
    let manifest = serde_json::json!({
        "exported": exported,
        "skipped_remote": skipped_remote,
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| ApiError::internal(format!("manifest serialize: {e}")))?;
    let manifest_entry = ZipEntryBuilder::new("MANIFEST.json".into(), Compression::Deflate);
    zip.write_entry_whole(manifest_entry, &manifest_bytes)
        .await
        .map_err(|e| ApiError::internal(format!("manifest zip write: {e}")))?;

    // close() consumes zip and returns Result<Compat<Cursor<Vec<u8>>>>.
    // with_tokio() wraps the writer in tokio_util::compat::Compat, so we need
    // two .into_inner() calls: first to unwrap Compat, then to unwrap Cursor.
    let compat_out = zip
        .close()
        .await
        .map_err(|e| ApiError::internal(format!("zip close: {e}")))?;
    let buf = compat_out.into_inner().into_inner();

    // D-20: response headers.
    let date_str = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("recordings-{}.zip", date_str);
    let body = Body::from(buf);
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", filename))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    Ok(resp)
}

/// REC-06: bulk delete recordings matching the filter set.
///
/// D-22: same filter params as /recordings list.
/// D-23: hard delete semantics (same as single delete D-15/D-16).
/// D-24: ?confirm=true required. Without it: return 400 with dry-run preview.
/// D-25: no row cap -- BULK_DELETE_CONFIRM_MSG warns operators of the risk.
/// WARNING: issuing this with no filters and ?confirm=true wipes ALL recordings.
async fn handle_bulk_delete(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(q): Query<BulkDeleteQuery>,
) -> Result<Response, ApiError> {
    let db = state.db();

    let account_cond = Condition::all().add(CdrColumn::AccountId.eq(scope.account_id.clone()));
    let conds = build_cdr_filter(&q.filters)
        .add(CdrColumn::RecordingUrl.is_not_null())
        .add(account_cond);

    let matched = CdrEntity::find()
        .filter(conds.clone())
        .count(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // D-24: dry-run guardrail.
    if q.confirm != Some(true) {
        let preview_resp = Json(BulkDeletePreviewResponse {
            preview: BulkDeletePreview {
                matched,
                would_delete: matched,
            },
            message: BULK_DELETE_CONFIRM_MSG,
        });
        return Ok((StatusCode::BAD_REQUEST, preview_resp).into_response());
    }

    // Confirmed: execute delete. Fetch all matched rows for file cleanup.
    let rows = CdrEntity::find()
        .filter(conds.clone())
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let cdr_storage = storage::resolve_storage(state.config().callrecord.as_ref())
        .map_err(|e| ApiError::internal(format!("storage error: {e}")))?;

    let mut deleted: u64 = 0;
    for row in &rows {
        let url = match &row.recording_url {
            Some(u) => u.clone(),
            None => continue,
        };

        if url.starts_with("http") || url.starts_with("s3://") {
            // Remote: log WARN, skip file delete (D-16).
            warn!(
                recording_id = row.id,
                url = %url,
                "bulk delete: remote recording; skipping file delete (D-16)"
            );
        } else if let Some(ref storage) = cdr_storage {
            if let Some(path) = storage.local_full_path(&url) {
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    warn!(
                        recording_id = row.id,
                        path = %path.display(),
                        "bulk delete: best-effort file removal failed: {e}"
                    );
                }
            }
        }
        deleted += 1;
    }

    // Clear recording_url in one UPDATE for all matched rows (D-23).
    CdrEntity::update_many()
        .col_expr(CdrColumn::RecordingUrl, Expr::value(None::<String>))
        .col_expr(CdrColumn::UpdatedAt, Expr::value(chrono::Utc::now()))
        .filter(conds)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(BulkDeleteResult { deleted }).into_response())
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
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let row = CdrEntity::find()
        .filter(CdrColumn::Id.eq(id))
        .filter(CdrColumn::AccountId.eq(scope.account_id.clone()))
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

    #[test]
    fn check_recordings_export_cap_allows_at_limit() {
        assert!(check_recordings_export_cap(0).is_ok());
        assert!(check_recordings_export_cap(10_000).is_ok());
    }

    #[test]
    fn check_recordings_export_cap_rejects_over_limit() {
        let err = check_recordings_export_cap(10_001).expect_err("must reject");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains("10000"),
            "message must reference cap, got: {}",
            err.message
        );
    }
}
