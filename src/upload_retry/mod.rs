//! Background scheduler that retries failed S3 uploads.
//!
//! On a fixed tick (default 60s) it pulls a batch of `pending_uploads`
//! rows whose per-row backoff has elapsed and attempts to re-upload them
//! using the current `[callrecord]` S3 configuration. On success the row
//! is deleted (and, when `keep_media_copy = false` and the row is a
//! media file, the local file is deleted too). On failure the attempts
//! counter is incremented and the row stays in the table; after
//! `max_attempts` the row is marked `failed_permanent` and skipped.
//!
//! If the local source file no longer exists when retry runs, the row
//! is marked `failed_missing_source` and never retried again.
//!
//! Local files are NEVER deleted on a permanent failure — operators
//! always have a chance to recover the data manually.

use crate::app::AppState;
use crate::config::CallRecordConfig;
use crate::models::pending_upload;
use crate::storage::{Storage, StorageConfig};
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use std::path::Path;
use std::time::Duration;
use tokio::time;
use tracing::{debug, error, info, warn};

/// Number of attempts before a row transitions to `failed_permanent`.
const DEFAULT_MAX_ATTEMPTS: i32 = 10;

/// How many rows to process per tick. Caps blast radius if the queue is huge.
const BATCH_SIZE: u64 = 50;

/// Tick interval. Each tick scans the pending queue and retries any rows
/// whose backoff window has elapsed.
const TICK_SECS: u64 = 60;

/// Spawn the retry scheduler. Always safe to call — the loop is a no-op
/// when callrecord is not configured for S3.
pub fn spawn(state: AppState) {
    crate::utils::spawn(async move {
        run(state).await;
    });
}

async fn run(state: AppState) {
    let mut interval = time::interval(Duration::from_secs(TICK_SECS));
    info!("upload_retry: scheduler started (tick = {}s)", TICK_SECS);
    loop {
        interval.tick().await;
        if let Err(e) = sweep(&state, DEFAULT_MAX_ATTEMPTS).await {
            error!("upload_retry: sweep error: {}", e);
        }
    }
}

/// Pull pending rows and retry them. Public so a future "Retry now"
/// endpoint can invoke it on demand.
pub async fn sweep(state: &AppState, max_attempts: i32) -> anyhow::Result<()> {
    // We need an S3 callrecord config to do anything useful.
    let s3_config = match state.config().callrecord.as_ref() {
        Some(cfg @ CallRecordConfig::S3 { .. }) => cfg.clone(),
        _ => return Ok(()),
    };

    let db = state.db().clone();
    let rows = pending_upload::Model::list_pending(&db, BATCH_SIZE).await?;
    if rows.is_empty() {
        return Ok(());
    }

    // Build the storage client once per sweep.
    let storage = match build_storage(&s3_config) {
        Some(s) => s,
        None => {
            warn!("upload_retry: failed to build storage client; skipping sweep");
            return Ok(());
        }
    };

    let now = Utc::now();
    for row in rows {
        if !ready_for_retry(&row, now) {
            continue;
        }
        retry_one(&db, &storage, &s3_config, &row, max_attempts).await;
    }
    Ok(())
}

/// Per-row exponential backoff. Doubles every attempt up to a 24h cap.
///
/// Schedule:
///   attempts=0  → retry immediately
///   attempts=1  → +1 minute
///   attempts=2  → +2 minutes
///   attempts=3  → +4 minutes
///   attempts=4  → +8 minutes
///   attempts=5  → +16 minutes
///   attempts=6  → +32 minutes
///   attempts=7  → +64 minutes
///   attempts=8  → +128 minutes
///   attempts=9  → +256 minutes (~4.3h)
///   attempts=10+ → +24h
fn ready_for_retry(row: &pending_upload::Model, now: DateTime<Utc>) -> bool {
    let Some(last) = row.last_attempt_at else {
        return true; // never tried, retry now
    };
    let backoff_secs: i64 = match row.attempts {
        0 => 0,
        n if n >= 10 => 86_400,
        n => 60i64.saturating_mul(1i64.checked_shl((n - 1) as u32).unwrap_or(60)),
    };
    let next = last + chrono::Duration::seconds(backoff_secs);
    now >= next
}

fn build_storage(callrecord: &CallRecordConfig) -> Option<Storage> {
    let CallRecordConfig::S3 {
        vendor,
        bucket,
        region,
        access_key,
        secret_key,
        endpoint,
        ..
    } = callrecord
    else {
        return None;
    };
    let cfg = StorageConfig::S3 {
        vendor: vendor.clone(),
        bucket: bucket.clone(),
        region: region.clone(),
        access_key: access_key.clone(),
        secret_key: secret_key.clone(),
        endpoint: Some(endpoint.clone()),
        prefix: None,
    };
    Storage::new(&cfg).ok()
}

/// Attempt one row. On success, delete the row (and the local file when
/// `keep_media_copy = false`). On failure, bump attempts; once attempts
/// reach `max_attempts` mark the row `failed_permanent`.
async fn retry_one(
    db: &DatabaseConnection,
    storage: &Storage,
    callrecord: &CallRecordConfig,
    row: &pending_upload::Model,
    max_attempts: i32,
) {
    let local_path = Path::new(&row.local_path);
    if !local_path.exists() {
        warn!(
            id = row.id,
            local = %row.local_path,
            "upload_retry: source file is missing, marking failed_missing_source"
        );
        let _ = pending_upload::Model::record_attempt(
            db,
            row.id,
            row.attempts,
            pending_upload::STATUS_FAILED_MISSING_SOURCE,
            "local source file no longer exists",
        )
        .await;
        return;
    }

    let bytes = match tokio::fs::read(local_path).await {
        Ok(b) => b,
        Err(e) => {
            warn!(id = row.id, local = %row.local_path, "upload_retry: read failed: {}", e);
            mark_attempt(db, row, max_attempts, &format!("read: {e}")).await;
            return;
        }
    };

    let buf_size = bytes.len();
    match storage.write(&row.target_key, bytes.into()).await {
        Ok(()) => {
            info!(
                id = row.id,
                kind = %row.kind,
                target = %row.target_key,
                buf_size,
                attempts = row.attempts + 1,
                "upload_retry: success"
            );
            // Update the call_record row's recording_url to point at the
            // S3 object so the playback handler can stream it. Only meaningful
            // for media kind — CDR JSON isn't what the UI plays back.
            if row.kind == pending_upload::KIND_MEDIA {
                if let CallRecordConfig::S3 { bucket, .. } = callrecord {
                    let s3_url = format!(
                        "s3://{}/{}",
                        bucket,
                        row.target_key.trim_start_matches('/')
                    );
                    if let Err(e) =
                        crate::models::call_record::update_recording_url_by_call_id(
                            db,
                            &row.call_id,
                            &s3_url,
                        )
                        .await
                    {
                        warn!(
                            id = row.id,
                            call_id = %row.call_id,
                            "upload_retry: failed to update call_record.recording_url after retry: {}",
                            e
                        );
                    }
                }
            }
            // Delete the row first so a crash mid-cleanup doesn't make
            // us re-upload an already-uploaded object.
            if let Err(e) = pending_upload::Model::delete_by_id(db, row.id).await {
                warn!(id = row.id, "upload_retry: delete row failed: {}", e);
                return;
            }
            // Optionally remove the local file. Always keep it for CDR
            // (the local CDR file is a side effect, not the source of
            // truth) and only remove media when keep_media_copy=false.
            if row.kind == pending_upload::KIND_MEDIA {
                let keep = matches!(
                    callrecord,
                    CallRecordConfig::S3 { keep_media_copy, .. } if keep_media_copy.unwrap_or(false)
                );
                if !keep {
                    if let Err(e) = tokio::fs::remove_file(local_path).await {
                        warn!(
                            local = %row.local_path,
                            "upload_retry: failed to delete local media after upload: {}",
                            e
                        );
                    }
                }
            }
        }
        Err(e) => {
            warn!(
                id = row.id,
                kind = %row.kind,
                target = %row.target_key,
                "upload_retry: upload failed: {}",
                e
            );
            mark_attempt(db, row, max_attempts, &format!("{e}")).await;
        }
    }
}

async fn mark_attempt(
    db: &DatabaseConnection,
    row: &pending_upload::Model,
    max_attempts: i32,
    error: &str,
) {
    let new_attempts = row.attempts + 1;
    let new_status = if new_attempts >= max_attempts {
        pending_upload::STATUS_FAILED_PERMANENT
    } else {
        pending_upload::STATUS_PENDING
    };
    if let Err(e) =
        pending_upload::Model::record_attempt(db, row.id, new_attempts, new_status, error).await
    {
        warn!(id = row.id, "upload_retry: record_attempt failed: {}", e);
    }
    if new_status == pending_upload::STATUS_FAILED_PERMANENT {
        error!(
            id = row.id,
            kind = %row.kind,
            local = %row.local_path,
            "upload_retry: row marked failed_permanent after {} attempts; manual intervention required (local file kept)",
            new_attempts
        );
    } else {
        debug!(
            id = row.id,
            attempts = new_attempts,
            "upload_retry: will retry later"
        );
    }
}
