//! Background webhook delivery processor (WH-02..WH-04).
//!
//! Phase 7 Plan 07-04 implements the full body. The processor subscribes
//! to the `WebhookEventSender` broadcast channel constructed at server
//! boot (07-01) and, for each event, performs:
//!
//!   1. Fresh DB read of `supersip_webhooks WHERE is_active=true` (D-12)
//!   2. Per-event-name filter (D-10; empty events = subscribe-all)
//!   3. Per-matching-webhook fan-out via `tokio::spawn(deliver_webhook)` so
//!      one slow target cannot block siblings (D-12, T-07-04-04)
//!   4. HMAC-signed POST per D-15 with retry schedule [1s, 5s, 30s] ±25%
//!      jitter (D-19); per-attempt timeout (D-20); status policy (D-21);
//!      Retry-After honoring (D-22); pre-flight DB recheck (D-32);
//!      disk fallback to `{generated_dir}/webhooks/failed/...` mode 0600
//!      (D-23, D-24).
//!
//! ## Total-attempts semantics
//!
//! `webhook.retry_count` denotes the number of RETRIES after the initial
//! attempt. Therefore a `retry_count` of 3 (the default) yields up to
//! `1 (initial) + 3 (retries) = 4` total attempts. `retry_count = 0` means
//! exactly one attempt with no retries; on failure, an immediate disk
//! fallback is written.

use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::signer;
use super::{WebhookCancelRegistry, WebhookEvent, WebhookEventSender, current_unix_timestamp};
use crate::models::webhook_outbox::{
    ActiveModel as OutboxAm, Column as OutboxColumn, Entity as OutboxEntity, STATUS_DELIVERED,
    STATUS_FAILED, STATUS_PENDING,
};
use crate::models::webhooks::{Column as WhColumn, Entity as WhEntity, Model as WhModel};

/// Lease (seconds) stamped on a `pending` outbox row at enqueue. The redelivery
/// worker only re-drives a `pending` row after its lease expires — long enough
/// (300s ≫ the ~36s worst-case in-process retry span) that a live delivery
/// reaches its terminal state first, so the worker only ever recovers rows
/// orphaned by a process crash.
const OUTBOX_LEASE_SECS: i64 = 300;
/// How often the redelivery worker scans for orphaned `pending` rows.
const REDELIVERY_INTERVAL: Duration = Duration::from_secs(60);
/// Max worker re-drives before a perpetually-orphaned row is failed (repeated
/// crashes only; an endpoint that merely errors is failed in-process).
const OUTBOX_MAX_ATTEMPTS: i32 = 10;
/// Per-tick batch cap on the worker scan.
const OUTBOX_BATCH: u64 = 100;

/// Terminal classification returned by [`deliver_webhook`] so the caller can
/// finalize the durable outbox row (task 2.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DeliveryOutcome {
    /// A 2xx was received.
    Delivered,
    /// Permanent failure or retries exhausted (disk fallback written).
    Failed(String),
    /// Cancelled or the webhook was deactivated mid-flight — no terminal mark;
    /// the row stays `pending` for the worker / operator (D-31/D-32).
    Aborted,
}

// ─── Helpers (Task 1) ────────────────────────────────────────────────────

/// Default backoff schedule (D-19): [1s, 5s, 30s]. Slots beyond the schedule
/// length re-use the last slot.
pub(super) const DEFAULT_BACKOFF_SCHEDULE: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
];

/// Compute the backoff for retry attempt `attempt_idx` (0-based among
/// retries — i.e. `attempt_idx=0` is the FIRST retry, after the initial
/// attempt failed). Returns `None` once `attempt_idx >= retry_count`,
/// signaling the loop should stop retrying and proceed to disk fallback.
///
/// Jitter: ±25% applied via `rand::rng().random_range`. The jittered delay
/// is therefore in `[base * 0.75, base * 1.25]`.
pub(super) fn compute_backoff(
    attempt_idx: usize,
    retry_count: usize,
    schedule: &[Duration],
) -> Option<Duration> {
    if attempt_idx >= retry_count {
        return None;
    }
    let base = schedule
        .get(attempt_idx)
        .copied()
        .unwrap_or_else(|| schedule.last().copied().unwrap_or(Duration::from_secs(30)));
    use rand::RngExt;
    let jitter: f64 = rand::rng().random_range(-0.25..=0.25);
    let nanos = (base.as_nanos() as f64 * (1.0 + jitter)) as u64;
    Some(Duration::from_nanos(nanos))
}

/// Parse the `Retry-After` header. Per D-22 we accept integer-seconds only;
/// HTTP-date format is NOT supported (documented limitation in 07-04
/// SUMMARY).
pub(super) fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Build the outbound header set per D-15..D-18.
///
/// Returns 6 headers: `Content-Type`, `User-Agent`, `X-Webhook-Event`,
/// `X-Webhook-Secret`, `X-Webhook-Request-Id`, `X-Webhook-Signature`.
///
/// **D-16 KNOWN WEAKNESS** (T-07-04-01): `X-Webhook-Secret` carries the
/// plaintext per-webhook secret for literal WH-04 spec parity. Receivers
/// SHOULD prefer `X-Webhook-Signature` (Stripe-style HMAC) for
/// authentication. v2.1 will deprecate this header. This is documented in
/// the threat register of 07-04-PLAN.md.
pub(super) fn build_request_headers(
    webhook: &WhModel,
    event_name: &str,
    request_id: &str,
    body: &str,
    timestamp: i64,
) -> Vec<(&'static str, String)> {
    vec![
        ("Content-Type", "application/json; charset=utf-8".to_string()),
        ("User-Agent", crate::version::get_useragent()),
        ("X-Webhook-Event", event_name.to_string()),
        // D-16 known weakness (T-07-04-01): plaintext secret.
        ("X-Webhook-Secret", webhook.secret.clone()),
        ("X-Webhook-Request-Id", request_id.to_string()),
        (
            "X-Webhook-Signature",
            signer::signature_header(timestamp, body, &webhook.secret),
        ),
    ]
}

/// Per-attempt log entry (D-24 schema).
#[derive(Clone, Debug, Serialize)]
pub(super) struct AttemptLog {
    pub attempt: u32,
    pub started_at: i64,
    pub duration_ms: u64,
    pub status_code: Option<u16>,
    pub error: Option<String>,
}

/// Disk fallback write per D-23 + D-24. File mode is 0600 on unix to limit
/// readability (T-07-04-02 mitigation).
///
/// Returns the absolute path written.
pub(super) async fn write_disk_fallback(
    generated_dir: &str,
    webhook: &WhModel,
    event: &WebhookEvent,
    envelope_body: &str,
    attempts: &[AttemptLog],
    first_attempt_at: i64,
) -> std::io::Result<std::path::PathBuf> {
    let dir = std::path::Path::new(generated_dir)
        .join("webhooks")
        .join("failed");
    tokio::fs::create_dir_all(&dir).await?;
    let ts = current_unix_timestamp();
    let path = dir.join(format!("{}-{}-{}.json", ts, webhook.id, event.event_id));

    let envelope_value: serde_json::Value =
        serde_json::from_str(envelope_body).unwrap_or(serde_json::Value::Null);

    let body = serde_json::json!({
        "envelope": envelope_value,
        "webhook_id": webhook.id,
        "webhook_url": webhook.url,
        "attempts": attempts,
        "first_attempt_at": first_attempt_at,
        "final_failure_at": ts,
    });
    let body_bytes = serde_json::to_vec_pretty(&body).unwrap_or_default();

    use tokio::io::AsyncWriteExt;

    #[cfg(unix)]
    {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600) // D-24 defense in depth (T-07-04-02)
            .open(&path)
            .await?;
        file.write_all(&body_bytes).await?;
        file.flush().await?;
    }
    #[cfg(not(unix))]
    {
        // Non-unix platforms inherit umask defaults (documented in SUMMARY).
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .await?;
        file.write_all(&body_bytes).await?;
        file.flush().await?;
    }

    Ok(path)
}

// ─── Delivery (Task 2) ───────────────────────────────────────────────────

/// Per-attempt outcome classification (D-21).
#[derive(Clone, Debug)]
pub(super) struct AttemptOutcome {
    pub status: Option<u16>,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub retry_after: Option<Duration>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Success,
    Retry,
    PermanentFail,
}

impl AttemptOutcome {
    pub(super) fn verdict(&self) -> Verdict {
        match self.status {
            Some(s) => match s {
                200..=299 => Verdict::Success,
                408 | 429 => Verdict::Retry,
                400..=499 => Verdict::PermanentFail,
                500..=599 => Verdict::Retry,
                _ => Verdict::Retry,
            },
            None => Verdict::Retry, // network error
        }
    }

    pub(super) fn into_log(self, attempt: u32, started_at: i64) -> AttemptLog {
        AttemptLog {
            attempt,
            started_at,
            duration_ms: self.duration_ms,
            status_code: self.status,
            error: self.error,
        }
    }
}

/// Single HTTP attempt. Sets per-webhook timeout (D-20).
pub(super) async fn perform_attempt(
    client: &reqwest::Client,
    webhook: &WhModel,
    event: &WebhookEvent,
    envelope_body: &str,
    request_id: &str,
) -> AttemptOutcome {
    let started = std::time::Instant::now();
    let timestamp = current_unix_timestamp();
    let headers = build_request_headers(webhook, &event.event, request_id, envelope_body, timestamp);
    let mut req = client
        .post(&webhook.url)
        .timeout(Duration::from_millis(webhook.timeout_ms.max(1) as u64))
        .body(envelope_body.to_string());
    for (k, v) in &headers {
        req = req.header(*k, v);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok())
                .and_then(parse_retry_after);
            AttemptOutcome {
                status: Some(status),
                error: None,
                duration_ms: started.elapsed().as_millis() as u64,
                retry_after,
            }
        }
        Err(e) => AttemptOutcome {
            status: None,
            error: Some(e.to_string()),
            duration_ms: started.elapsed().as_millis() as u64,
            retry_after: None,
        },
    }
}

/// Per-webhook delivery driver.
///
/// Total attempts = 1 initial + `webhook.retry_count` retries (see module
/// docs). Cancellation observed at every await point; on cancel we exit
/// silently with NO disk fallback (D-31, D-32). Pre-flight DB recheck
/// before EACH retry: if webhook is missing or `is_active=false`, abort
/// without disk fallback (D-32).
pub(super) async fn deliver_webhook(
    webhook: WhModel,
    event: WebhookEvent,
    envelope_body: String,
    db: DatabaseConnection,
    cancel_registry: Arc<WebhookCancelRegistry>,
    generated_dir: String,
    client: reqwest::Client,
) -> DeliveryOutcome {
    deliver_webhook_with_schedule(
        webhook,
        event,
        envelope_body,
        db,
        cancel_registry,
        generated_dir,
        client,
        DEFAULT_BACKOFF_SCHEDULE,
    )
    .await
}

/// Test-friendly variant of [`deliver_webhook`] that allows overriding the
/// backoff schedule. Production callers always go through `deliver_webhook`,
/// which delegates here with [`DEFAULT_BACKOFF_SCHEDULE`].
pub(super) async fn deliver_webhook_with_schedule(
    webhook: WhModel,
    event: WebhookEvent,
    envelope_body: String,
    db: DatabaseConnection,
    cancel_registry: Arc<WebhookCancelRegistry>,
    generated_dir: String,
    client: reqwest::Client,
    schedule: &'static [Duration],
) -> DeliveryOutcome {
    let token = cancel_registry.insert(&webhook.id);
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut attempts: Vec<AttemptLog> = Vec::new();
    let first_attempt_at = current_unix_timestamp();
    let retry_count = webhook.retry_count.max(0) as usize;

    let mut current_webhook = webhook;
    // attempt_no counts attempts including the initial one (1-based).
    let mut attempt_no: u32 = 0;

    loop {
        attempt_no += 1;
        let started_at = current_unix_timestamp();
        let outcome = tokio::select! {
            _ = token.cancelled() => {
                // D-31 / D-34 cancel: exit silently, no disk fallback.
                return DeliveryOutcome::Aborted;
            }
            res = perform_attempt(&client, &current_webhook, &event, &envelope_body, &request_id) => res,
        };
        let verdict = outcome.verdict();
        let retry_after = outcome.retry_after;
        attempts.push(outcome.into_log(attempt_no, started_at));

        match verdict {
            Verdict::Success => {
                cancel_registry.remove(&current_webhook.id);
                return DeliveryOutcome::Delivered;
            }
            Verdict::PermanentFail => break,
            Verdict::Retry => {
                // attempt_no is 1-based total; retries already done = attempt_no - 1.
                let retries_done = (attempt_no - 1) as usize;
                let Some(mut sleep_for) =
                    compute_backoff(retries_done, retry_count, schedule)
                else {
                    break; // exhausted retries → disk fallback
                };
                if let Some(ra) = retry_after
                    && ra <= sleep_for
                {
                    sleep_for = ra; // D-22
                }
                tokio::select! {
                    _ = token.cancelled() => return DeliveryOutcome::Aborted,
                    _ = tokio::time::sleep(sleep_for) => {}
                }
                // D-32 pre-flight DB recheck.
                match WhEntity::find_by_id(current_webhook.id.clone()).one(&db).await {
                    Ok(Some(m)) if m.is_active => {
                        current_webhook = m;
                        continue;
                    }
                    _ => {
                        // missing or deactivated → abort, no fallback.
                        cancel_registry.remove(&current_webhook.id);
                        return DeliveryOutcome::Aborted;
                    }
                }
            }
        }
    }

    // Retries exhausted OR permanent fail → disk fallback.
    let _ = write_disk_fallback(
        &generated_dir,
        &current_webhook,
        &event,
        &envelope_body,
        &attempts,
        first_attempt_at,
    )
    .await;
    cancel_registry.remove(&current_webhook.id);
    DeliveryOutcome::Failed(failure_reason(&attempts))
}

/// Summarize the last attempt for the outbox `last_error` column.
fn failure_reason(attempts: &[AttemptLog]) -> String {
    match attempts.last() {
        Some(a) => match (a.status_code, &a.error) {
            (Some(s), _) => format!("status {s}"),
            (None, Some(e)) => e.clone(),
            (None, None) => "delivery failed".to_string(),
        },
        None => "delivery failed".to_string(),
    }
}

// ─── Durable outbox helpers (task 2.1) ───────────────────────────────────────

/// Insert a `pending` outbox row before delivery is attempted, returning its
/// id (or `None`, logged, on DB error — enqueue is best-effort and must not
/// break the fast path). `attempt_count` starts at 1 (the in-process delivery).
pub(super) async fn outbox_insert_pending(
    db: &DatabaseConnection,
    webhook_id: &str,
    event: &WebhookEvent,
    envelope_body: &str,
) -> Option<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();
    let am = OutboxAm {
        id: Set(id.clone()),
        webhook_id: Set(webhook_id.to_string()),
        event_id: Set(event.event_id.clone()),
        event_name: Set(event.event.clone()),
        envelope: Set(envelope_body.to_string()),
        status: Set(STATUS_PENDING.to_string()),
        attempt_count: Set(1),
        last_error: Set(None),
        next_retry_at: Set(now + ChronoDuration::seconds(OUTBOX_LEASE_SECS)),
        created_at: Set(now),
        updated_at: Set(now),
    };
    match am.insert(db).await {
        Ok(_) => Some(id),
        Err(e) => {
            tracing::error!("webhook outbox insert failed: {e}");
            None
        }
    }
}

/// Advance a row to its terminal state after delivery. `Aborted` leaves the row
/// `pending` (cancel/deactivate is operator-driven, not a delivery failure).
pub(super) async fn outbox_finalize(
    db: &DatabaseConnection,
    outbox_id: &str,
    outcome: &DeliveryOutcome,
) {
    let (status, err) = match outcome {
        DeliveryOutcome::Delivered => (STATUS_DELIVERED, None),
        DeliveryOutcome::Failed(e) => (STATUS_FAILED, Some(e.clone())),
        DeliveryOutcome::Aborted => return,
    };
    let am = OutboxAm {
        id: Set(outbox_id.to_string()),
        status: Set(status.to_string()),
        last_error: Set(err),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    if let Err(e) = am.update(db).await {
        tracing::error!("webhook outbox finalize failed for {outbox_id}: {e}");
    }
}

// ─── Test-event helper (07-05 D-28..D-30) ────────────────────────────────

/// Synchronous single-attempt delivery used by POST /api/v1/webhooks to
/// fire a `webhook.test` event inline so the response can carry a
/// `test_delivery` field per D-29. NO retry, NO disk fallback (T-07-05-03 +
/// T-07-05-07): bounded in time by the webhook's `timeout_ms` (max 30s per
/// D-04). Returns `Ok(())` on 2xx; `Err(message)` on any non-2xx, network
/// error, or timeout. Reuses `perform_attempt` so signing + headers are
/// identical to real fires (D-30).
pub async fn deliver_test_event(
    webhook: &WhModel,
    event: &WebhookEvent,
    envelope_body: &str,
    client: &reqwest::Client,
) -> Result<(), String> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let outcome =
        perform_attempt(client, webhook, event, envelope_body, &request_id).await;
    match outcome.status {
        Some(s) if (200..=299).contains(&s) => Ok(()),
        Some(s) => Err(format!("status {}", s)),
        None => Err(outcome.error.unwrap_or_else(|| "network error".to_string())),
    }
}

// ─── Processor (Task 3) ──────────────────────────────────────────────────

/// Run the webhook processor until `cancel` fires. Spawned at server boot.
pub async fn run_webhook_processor(
    db: DatabaseConnection,
    sender: WebhookEventSender,
    cancel_registry: Arc<WebhookCancelRegistry>,
    generated_dir: String,
    cancel: CancellationToken,
) {
    let mut rx = sender.subscribe();
    let client = reqwest::Client::builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    loop {
        let event = tokio::select! {
            _ = cancel.cancelled() => break,
            res = rx.recv() => match res {
                Ok(e) => e,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("webhook processor lagged, missed {} events", n);
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        };

        // D-12 fresh DB read per event.
        let webhooks = match WhEntity::find()
            .filter(WhColumn::IsActive.eq(true))
            .all(&db)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("webhook processor DB query failed: {e}");
                continue;
            }
        };

        // D-07 envelope serialization (once per event).
        let envelope = serde_json::json!({
            "event_id": event.event_id,
            "event": event.event,
            "timestamp": event.timestamp,
            "data": event.data,
        });
        let envelope_body = serde_json::to_string(&envelope).unwrap_or_default();

        for webhook in webhooks {
            // D-10 event-name filter.
            let events: Vec<String> =
                serde_json::from_value(webhook.events.clone()).unwrap_or_default();
            if !events.is_empty() && !events.contains(&event.event) {
                continue;
            }

            let task_event = event.clone();
            let task_body = envelope_body.clone();
            let task_db = db.clone();
            let task_registry = cancel_registry.clone();
            let task_dir = generated_dir.clone();
            let task_client = client.clone();
            // task 2.1: durable record BEFORE delivery (synchronous insert +
            // spawn — broadcast stays the fast path). A crash between here and
            // a terminal mark leaves a `pending` row the worker recovers.
            let outbox_id =
                outbox_insert_pending(&db, &webhook.id, &event, &envelope_body).await;
            let finalize_db = db.clone();
            tokio::spawn(async move {
                let outcome = deliver_webhook(
                    webhook,
                    task_event,
                    task_body,
                    task_db,
                    task_registry,
                    task_dir,
                    task_client,
                )
                .await;
                if let Some(oid) = outbox_id {
                    outbox_finalize(&finalize_db, &oid, &outcome).await;
                }
            });
        }
    }
}

// ─── Redelivery worker (task 2.1) ────────────────────────────────────────────

/// Re-drive `pending` outbox rows whose lease has expired — i.e. rows orphaned
/// when the process crashed mid-delivery. Spawned at server boot beside
/// [`run_webhook_processor`]. Runs every [`REDELIVERY_INTERVAL`] until `cancel`
/// fires. Redelivery is safe to repeat: the stable `event_id` (2.2) lets the
/// receiver dedup, so the worker favours liveness over exactly-once.
pub async fn run_webhook_redelivery_worker(
    db: DatabaseConnection,
    cancel_registry: Arc<WebhookCancelRegistry>,
    generated_dir: String,
    cancel: CancellationToken,
) {
    let client = reqwest::Client::builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(REDELIVERY_INTERVAL) => {}
        }
        redeliver_due_rows(&db, &cancel_registry, &generated_dir, &client).await;
    }
}

/// One scan-and-redrive pass: claim each due `pending` row (re-lease + bump),
/// then spawn a delivery that finalizes the row. Extracted from the worker loop
/// so it is unit-testable without the [`REDELIVERY_INTERVAL`] sleep.
pub(super) async fn redeliver_due_rows(
    db: &DatabaseConnection,
    cancel_registry: &Arc<WebhookCancelRegistry>,
    generated_dir: &str,
    client: &reqwest::Client,
) {
    let now = Utc::now();
    let due = match OutboxEntity::find()
        .filter(OutboxColumn::Status.eq(STATUS_PENDING))
        .filter(OutboxColumn::NextRetryAt.lte(now))
        .order_by_asc(OutboxColumn::NextRetryAt)
        .limit(OUTBOX_BATCH)
        .all(db)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("webhook redelivery scan failed: {e}");
            return;
        }
    };

    for row in due {
        // Repeated-crash backstop: fail a row re-driven too many times.
        if row.attempt_count >= OUTBOX_MAX_ATTEMPTS {
            outbox_fail_row(db, &row.id, "max redelivery attempts exceeded").await;
            continue;
        }
        // Re-lease + bump BEFORE re-driving so the next tick can't double-grab.
        let leased = OutboxAm {
            id: Set(row.id.clone()),
            attempt_count: Set(row.attempt_count + 1),
            next_retry_at: Set(now + ChronoDuration::seconds(OUTBOX_LEASE_SECS)),
            updated_at: Set(now),
            ..Default::default()
        };
        if let Err(e) = leased.update(db).await {
            tracing::error!("webhook redelivery re-lease failed for {}: {e}", row.id);
            continue;
        }

        // Webhook must still exist + be active, else the row is terminal.
        let webhook = match WhEntity::find_by_id(row.webhook_id.clone()).one(db).await {
            Ok(Some(w)) if w.is_active => w,
            _ => {
                outbox_fail_row(db, &row.id, "webhook missing or inactive").await;
                continue;
            }
        };

        // The POST body is the stored envelope verbatim; deliver_webhook only
        // reads `event.event` (header) + `event.event_id` (fallback filename),
        // so `data` is left Null (unused by delivery).
        let event = WebhookEvent {
            event_id: row.event_id.clone(),
            event: row.event_name.clone(),
            timestamp: current_unix_timestamp(),
            data: serde_json::Value::Null,
        };
        let envelope_body = row.envelope.clone();
        let task_db = db.clone();
        let finalize_db = db.clone();
        let task_registry = cancel_registry.clone();
        let task_dir = generated_dir.to_string();
        let task_client = client.clone();
        let outbox_id = row.id.clone();
        tokio::spawn(async move {
            let outcome = deliver_webhook(
                webhook,
                event,
                envelope_body,
                task_db,
                task_registry,
                task_dir,
                task_client,
            )
            .await;
            outbox_finalize(&finalize_db, &outbox_id, &outcome).await;
        });
    }
}

/// Mark a row `failed` with a reason (worker-side terminal: missing webhook,
/// max attempts). Best-effort; logs on DB error.
async fn outbox_fail_row(db: &DatabaseConnection, outbox_id: &str, reason: &str) {
    let am = OutboxAm {
        id: Set(outbox_id.to_string()),
        status: Set(STATUS_FAILED.to_string()),
        last_error: Set(Some(reason.to_string())),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    if let Err(e) = am.update(db).await {
        tracing::error!("webhook outbox fail-mark failed for {outbox_id}: {e}");
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, Database, Set};
    use sea_orm_migration::{MigrationTrait, MigratorTrait};

    fn fixture_webhook(id: &str, url: &str, events: serde_json::Value) -> WhModel {
        let now = Utc::now();
        WhModel {
            id: id.to_string(),
            name: format!("wh-{id}"),
            url: url.to_string(),
            secret: "test-secret".to_string(),
            events,
            description: None,
            is_active: true,
            retry_count: 3,
            timeout_ms: 5000,
            created_at: now,
            updated_at: now,
        }
    }

    fn fixture_event() -> WebhookEvent {
        WebhookEvent {
            event_id: "evt_test".to_string(),
            event: "call.completed".to_string(),
            timestamp: 1714060800,
            data: serde_json::json!({"call_id": "abc"}),
        }
    }

    // ─── compute_backoff ────────────────────────────────────────────────

    #[test]
    fn compute_backoff_first_attempt_within_jitter_band() {
        for _ in 0..200 {
            let d =
                compute_backoff(0, 3, DEFAULT_BACKOFF_SCHEDULE).expect("retry within range");
            assert!(d >= Duration::from_millis(750), "lower bound: {d:?}");
            assert!(d <= Duration::from_millis(1250), "upper bound: {d:?}");
        }
    }

    #[test]
    fn compute_backoff_third_attempt_uses_30s_with_jitter() {
        for _ in 0..200 {
            let d =
                compute_backoff(2, 3, DEFAULT_BACKOFF_SCHEDULE).expect("retry within range");
            assert!(d >= Duration::from_millis(22_500), "lower: {d:?}");
            assert!(d <= Duration::from_millis(37_500), "upper: {d:?}");
        }
    }

    #[test]
    fn compute_backoff_zero_retry_count_returns_none() {
        assert!(compute_backoff(0, 0, DEFAULT_BACKOFF_SCHEDULE).is_none());
    }

    #[test]
    fn compute_backoff_attempt_at_or_past_retry_count_returns_none() {
        assert!(compute_backoff(3, 3, DEFAULT_BACKOFF_SCHEDULE).is_none());
        assert!(compute_backoff(5, 3, DEFAULT_BACKOFF_SCHEDULE).is_none());
    }

    #[test]
    fn compute_backoff_beyond_schedule_reuses_last_slot() {
        // retry_count=10 means slots 3..9 reuse the last (30s) slot.
        for _ in 0..200 {
            let d = compute_backoff(7, 10, DEFAULT_BACKOFF_SCHEDULE).expect("present");
            assert!(d >= Duration::from_millis(22_500));
            assert!(d <= Duration::from_millis(37_500));
        }
    }

    // ─── parse_retry_after ──────────────────────────────────────────────

    #[test]
    fn parse_retry_after_integer_seconds_supported() {
        assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after(" 12 "), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(0)));
    }

    #[test]
    fn parse_retry_after_invalid_returns_none() {
        assert_eq!(parse_retry_after("invalid"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    #[test]
    fn parse_retry_after_http_date_not_supported() {
        // Documented limitation: HTTP-date format is not parsed.
        assert_eq!(parse_retry_after("Wed, 01 Jan 2026 00:00:00 GMT"), None);
    }

    // ─── build_request_headers ──────────────────────────────────────────

    #[test]
    fn build_request_headers_sets_six_expected_headers() {
        let webhook = fixture_webhook("w1", "https://example.test", serde_json::json!([]));
        let headers = build_request_headers(
            &webhook,
            "call.completed",
            "req-uuid-1",
            r#"{"a":1}"#,
            1714060800,
        );

        let map: std::collections::HashMap<_, _> = headers
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();

        assert_eq!(
            map.get("Content-Type").map(String::as_str),
            Some("application/json; charset=utf-8")
        );
        // Webhook UA must match the SIP endpoint UA exactly (one identity
        // across the project — see change add-supersbc-user-agent).
        let expected_ua = crate::version::get_useragent();
        assert_eq!(map.get("User-Agent"), Some(&expected_ua));
        assert_eq!(
            map.get("X-Webhook-Event").map(String::as_str),
            Some("call.completed")
        );
        // D-16 plaintext secret.
        assert_eq!(
            map.get("X-Webhook-Secret").map(String::as_str),
            Some("test-secret")
        );
        assert_eq!(
            map.get("X-Webhook-Request-Id").map(String::as_str),
            Some("req-uuid-1")
        );
        let sig = map.get("X-Webhook-Signature").expect("signature header");
        assert!(sig.starts_with("t=1714060800,v1="), "got: {sig}");
    }

    // ─── write_disk_fallback ────────────────────────────────────────────

    #[tokio::test]
    async fn write_disk_fallback_creates_file_with_expected_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let webhook = fixture_webhook("wh-fb", "https://example.test", serde_json::json!([]));
        let event = fixture_event();
        let envelope = serde_json::to_string(&serde_json::json!({
            "event_id": event.event_id,
            "event": event.event,
            "timestamp": event.timestamp,
            "data": event.data,
        }))
        .unwrap();
        let attempts = vec![AttemptLog {
            attempt: 1,
            started_at: 1714060800,
            duration_ms: 12,
            status_code: Some(502),
            error: None,
        }];

        let path = write_disk_fallback(
            dir.path().to_str().unwrap(),
            &webhook,
            &event,
            &envelope,
            &attempts,
            1714060800,
        )
        .await
        .expect("disk fallback write");

        assert!(path.exists(), "fallback file must exist: {path:?}");
        let parent = path.parent().unwrap();
        assert!(parent.ends_with("webhooks/failed"));
        let fname = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(fname.ends_with(&format!("-{}-{}.json", webhook.id, event.event_id)));

        let parsed: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.expect("read")).expect("json");
        assert_eq!(parsed["webhook_id"], webhook.id);
        assert_eq!(parsed["webhook_url"], webhook.url);
        assert_eq!(parsed["envelope"]["event"], "call.completed");
        assert_eq!(parsed["attempts"].as_array().map(Vec::len), Some(1));
        assert_eq!(parsed["first_attempt_at"], 1714060800);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_disk_fallback_uses_mode_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let webhook = fixture_webhook("m6", "https://example.test", serde_json::json!([]));
        let event = fixture_event();
        let path = write_disk_fallback(
            dir.path().to_str().unwrap(),
            &webhook,
            &event,
            "{}",
            &[],
            1,
        )
        .await
        .expect("write");
        let meta = tokio::fs::metadata(&path).await.expect("metadata");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    // ─── verdict (status policy D-21) ───────────────────────────────────

    #[test]
    fn verdict_classifies_status_codes_per_d21() {
        let mk = |s: Option<u16>| AttemptOutcome {
            status: s,
            error: None,
            duration_ms: 1,
            retry_after: None,
        };
        assert_eq!(mk(Some(200)).verdict(), Verdict::Success);
        assert_eq!(mk(Some(204)).verdict(), Verdict::Success);
        assert_eq!(mk(Some(299)).verdict(), Verdict::Success);
        assert_eq!(mk(Some(400)).verdict(), Verdict::PermanentFail);
        assert_eq!(mk(Some(401)).verdict(), Verdict::PermanentFail);
        assert_eq!(mk(Some(404)).verdict(), Verdict::PermanentFail);
        assert_eq!(mk(Some(408)).verdict(), Verdict::Retry);
        assert_eq!(mk(Some(429)).verdict(), Verdict::Retry);
        assert_eq!(mk(Some(500)).verdict(), Verdict::Retry);
        assert_eq!(mk(Some(502)).verdict(), Verdict::Retry);
        assert_eq!(mk(Some(599)).verdict(), Verdict::Retry);
        // Network error.
        assert_eq!(mk(None).verdict(), Verdict::Retry);
    }

    // ─── deliver_webhook integration tests (Task 2) ─────────────────────
    //
    // We use an axum-based mock server bound to 127.0.0.1:0 (mirrors the
    // pattern in src/proxy/routing/match_types.rs). Each test gets a fresh
    // mock + fresh sqlite + fresh registry to keep parallel tests isolated.

    use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    struct TestMigrator;
    #[async_trait::async_trait]
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![
                Box::new(crate::models::webhooks::Migration),
                // task 2.1: outbox table so the durable enqueue path runs in tests.
                Box::new(crate::models::webhook_outbox::Migration),
            ]
        }
    }

    async fn fresh_sqlite() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.expect("sqlite");
        TestMigrator::up(&db, None).await.expect("migrate");
        db
    }

    async fn insert_webhook(
        db: &DatabaseConnection,
        id: &str,
        url: &str,
        retry_count: i32,
        timeout_ms: i32,
        events: serde_json::Value,
        is_active: bool,
    ) -> WhModel {
        use crate::models::webhooks::ActiveModel;
        let now = Utc::now();
        let am = ActiveModel {
            id: Set(id.to_string()),
            name: Set(format!("name-{id}")),
            url: Set(url.to_string()),
            secret: Set("secret".to_string()),
            events: Set(events),
            description: Set(None),
            is_active: Set(is_active),
            retry_count: Set(retry_count),
            timeout_ms: Set(timeout_ms),
            created_at: Set(now),
            updated_at: Set(now),
        };
        am.insert(db).await.expect("insert webhook")
    }

    /// Spin up an axum mock server that responds with a sequence of status
    /// codes (cycling through the supplied vec). Returns (base_url,
    /// hits_counter, retry_after_for_429s).
    async fn spawn_mock(
        statuses: Vec<u16>,
        retry_after: Option<&'static str>,
    ) -> (String, StdArc<AtomicUsize>, StdArc<AtomicU16>) {
        use axum::extract::State;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;

        let hits = StdArc::new(AtomicUsize::new(0));
        let last_status = StdArc::new(AtomicU16::new(0));
        let statuses = StdArc::new(statuses);

        #[derive(Clone)]
        struct AppState {
            statuses: StdArc<Vec<u16>>,
            hits: StdArc<AtomicUsize>,
            last_status: StdArc<AtomicU16>,
            retry_after: Option<&'static str>,
        }
        let state = AppState {
            statuses: statuses.clone(),
            hits: hits.clone(),
            last_status: last_status.clone(),
            retry_after,
        };

        async fn handler(
            State(state): State<AppState>,
            _body: axum::body::Bytes,
        ) -> impl IntoResponse {
            let idx = state.hits.fetch_add(1, Ordering::SeqCst);
            let s = state.statuses[idx % state.statuses.len()];
            state.last_status.store(s, Ordering::SeqCst);
            let status = StatusCode::from_u16(s).unwrap_or(StatusCode::OK);
            let mut resp = (status, "ok").into_response();
            if s == 429
                && let Some(ra) = state.retry_after
            {
                resp.headers_mut()
                    .insert("Retry-After", ra.parse().unwrap());
            }
            resp
        }

        let router = Router::new().route("/", post(handler)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (format!("http://{}/", addr), hits, last_status)
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client")
    }

    #[tokio::test]
    async fn deliver_happy_200_one_attempt() {
        let (url, hits, _) = spawn_mock(vec![200], None).await;
        let db = fresh_sqlite().await;
        let webhook = insert_webhook(
            &db,
            "wh-ok",
            &url,
            3,
            5000,
            serde_json::json!([]),
            true,
        )
        .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();

        deliver_webhook(
            webhook,
            fixture_event(),
            r#"{"a":1}"#.to_string(),
            db.clone(),
            registry.clone(),
            dir.path().to_string_lossy().into_owned(),
            test_client(),
        )
        .await;

        assert_eq!(hits.load(Ordering::SeqCst), 1, "single attempt on 200");
        assert!(!registry.contains_key("wh-ok"), "registry cleaned");
        // No fallback file.
        let failed_dir = dir.path().join("webhooks").join("failed");
        if failed_dir.exists() {
            let entries: Vec<_> = std::fs::read_dir(&failed_dir).unwrap().collect();
            assert!(entries.is_empty(), "no fallback expected on success");
        }
    }

    #[tokio::test]
    async fn deliver_400_permanent_fail_writes_fallback() {
        let (url, hits, _) = spawn_mock(vec![400], None).await;
        let db = fresh_sqlite().await;
        let webhook = insert_webhook(
            &db,
            "wh-perm",
            &url,
            3,
            5000,
            serde_json::json!([]),
            true,
        )
        .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();

        deliver_webhook(
            webhook,
            fixture_event(),
            r#"{"a":1}"#.to_string(),
            db.clone(),
            registry,
            dir.path().to_string_lossy().into_owned(),
            test_client(),
        )
        .await;

        assert_eq!(hits.load(Ordering::SeqCst), 1, "permanent fail = 1 attempt");
        let failed_dir = dir.path().join("webhooks").join("failed");
        let entries: Vec<_> = std::fs::read_dir(&failed_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "fallback file written");
    }

    /// Tiny backoff schedule for tests so retry exhaustion completes in
    /// milliseconds instead of the production [1s, 5s, 30s] cadence.
    const TEST_BACKOFF_SCHEDULE: &[Duration] = &[
        Duration::from_millis(5),
        Duration::from_millis(5),
        Duration::from_millis(5),
    ];

    #[tokio::test(flavor = "current_thread")]
    async fn deliver_502_exhausts_retries_writes_fallback() {
        // retry_count=3 → 1 + 3 = 4 total attempts, all 502.
        let (url, hits, _) = spawn_mock(vec![502], None).await;
        let db = fresh_sqlite().await;
        let webhook = insert_webhook(
            &db,
            "wh-retry",
            &url,
            3,
            5000,
            serde_json::json!([]),
            true,
        )
        .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();

        deliver_webhook_with_schedule(
            webhook,
            fixture_event(),
            r#"{"a":1}"#.to_string(),
            db.clone(),
            registry,
            dir.path().to_string_lossy().into_owned(),
            test_client(),
            TEST_BACKOFF_SCHEDULE,
        )
        .await;

        assert_eq!(hits.load(Ordering::SeqCst), 4, "1 initial + 3 retries");
        let failed_dir = dir.path().join("webhooks").join("failed");
        let entries: Vec<_> = std::fs::read_dir(&failed_dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn deliver_zero_retries_writes_fallback_after_one_failure() {
        let (url, hits, _) = spawn_mock(vec![502], None).await;
        let db = fresh_sqlite().await;
        let webhook = insert_webhook(
            &db,
            "wh-zero",
            &url,
            0, // retry_count=0
            5000,
            serde_json::json!([]),
            true,
        )
        .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();

        deliver_webhook(
            webhook,
            fixture_event(),
            "{}".to_string(),
            db,
            registry,
            dir.path().to_string_lossy().into_owned(),
            test_client(),
        )
        .await;

        assert_eq!(hits.load(Ordering::SeqCst), 1, "no retries");
        let failed_dir = dir.path().join("webhooks").join("failed");
        let entries: Vec<_> = std::fs::read_dir(&failed_dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn deliver_db_deactivate_during_retry_aborts_no_fallback() {
        // First attempt 502 → during sleep we deactivate row → recheck
        // observes is_active=false → abort silently, NO fallback.
        let (url, hits, _) = spawn_mock(vec![502], None).await;
        let db = fresh_sqlite().await;
        let webhook = insert_webhook(
            &db,
            "wh-deact",
            &url,
            3,
            5000,
            serde_json::json!([]),
            true,
        )
        .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();

        // Spawn delivery in background.
        let db_clone = db.clone();
        let dir_str = dir.path().to_string_lossy().into_owned();
        let registry_clone = registry.clone();
        let task = tokio::spawn(deliver_webhook(
            webhook,
            fixture_event(),
            "{}".to_string(),
            db_clone.clone(),
            registry_clone,
            dir_str,
            test_client(),
        ));

        // Wait for the first attempt to land (poll hits).
        for _ in 0..200 {
            if hits.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(hits.load(Ordering::SeqCst) >= 1);

        // Deactivate the row before the retry recheck fires (~1s later).
        use sea_orm::ActiveModelTrait;
        let am = crate::models::webhooks::ActiveModel {
            id: Set("wh-deact".to_string()),
            is_active: Set(false),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        am.update(&db).await.expect("deactivate");

        // Wait for the delivery task to exit (it should abort after recheck).
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;

        // No fallback file expected.
        let failed_dir = dir.path().join("webhooks").join("failed");
        if failed_dir.exists() {
            let entries: Vec<_> = std::fs::read_dir(&failed_dir).unwrap().collect();
            assert!(entries.is_empty(), "no fallback when operator deactivated");
        }
    }

    #[tokio::test]
    async fn deliver_cancel_during_sleep_no_fallback() {
        // 502 → enters sleep → token cancelled → exits without fallback.
        let (url, hits, _) = spawn_mock(vec![502], None).await;
        let db = fresh_sqlite().await;
        let webhook = insert_webhook(
            &db,
            "wh-cancel",
            &url,
            3,
            5000,
            serde_json::json!([]),
            true,
        )
        .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();

        let registry_clone = registry.clone();
        let dir_str = dir.path().to_string_lossy().into_owned();
        let task = tokio::spawn(deliver_webhook(
            webhook,
            fixture_event(),
            "{}".to_string(),
            db.clone(),
            registry_clone,
            dir_str,
            test_client(),
        ));

        // Wait for first attempt, then cancel (D-31).
        for _ in 0..200 {
            if hits.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Give the task a moment to enter the sleep branch.
        tokio::time::sleep(Duration::from_millis(50)).await;
        registry.cancel("wh-cancel");

        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;

        let failed_dir = dir.path().join("webhooks").join("failed");
        if failed_dir.exists() {
            let entries: Vec<_> = std::fs::read_dir(&failed_dir).unwrap().collect();
            assert!(entries.is_empty(), "cancel = no fallback");
        }
    }

    // ─── run_webhook_processor end-to-end (Task 3) ──────────────────────

    #[tokio::test]
    async fn run_processor_dispatches_to_active_webhooks_only() {
        let (url, hits, _) = spawn_mock(vec![200], None).await;
        let db = fresh_sqlite().await;
        // Active + subscribed.
        insert_webhook(
            &db,
            "wh-active",
            &url,
            3,
            5000,
            serde_json::json!(["call.completed"]),
            true,
        )
        .await;
        // Inactive (must NOT be hit).
        insert_webhook(
            &db,
            "wh-inactive",
            &url,
            3,
            5000,
            serde_json::json!(["call.completed"]),
            false,
        )
        .await;
        // Active but subscribes to a different event (must NOT be hit).
        insert_webhook(
            &db,
            "wh-other-event",
            &url,
            3,
            5000,
            serde_json::json!(["call.started"]),
            true,
        )
        .await;

        let (sender, _) = tokio::sync::broadcast::channel::<WebhookEvent>(16);
        let registry = Arc::new(WebhookCancelRegistry::new());
        let cancel = CancellationToken::new();
        let dir = tempfile::tempdir().unwrap();

        let proc_db = db.clone();
        let proc_sender = sender.clone();
        let proc_registry = registry.clone();
        let proc_cancel = cancel.clone();
        let dir_str = dir.path().to_string_lossy().into_owned();
        let handle = tokio::spawn(async move {
            run_webhook_processor(proc_db, proc_sender, proc_registry, dir_str, proc_cancel)
                .await;
        });

        // Give the processor a moment to subscribe.
        tokio::time::sleep(Duration::from_millis(50)).await;
        sender.send(fixture_event()).expect("send");

        // Poll until we see the single expected hit.
        for _ in 0..200 {
            if hits.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Wait a bit longer to ensure no extra hits land.
        tokio::time::sleep(Duration::from_millis(150)).await;

        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "only the active+subscribed webhook should fire"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    // ─── outbox + redelivery (task 2.1) ─────────────────────────────────

    async fn insert_outbox_row(
        db: &DatabaseConnection,
        id: &str,
        webhook_id: &str,
        next_retry_at: chrono::DateTime<Utc>,
        attempt_count: i32,
    ) {
        let now = Utc::now();
        let am = OutboxAm {
            id: Set(id.to_string()),
            webhook_id: Set(webhook_id.to_string()),
            event_id: Set(format!("evt_{id}")),
            event_name: Set("call.completed".to_string()),
            envelope: Set(r#"{"event":"call.completed","data":{}}"#.to_string()),
            status: Set(STATUS_PENDING.to_string()),
            attempt_count: Set(attempt_count),
            last_error: Set(None),
            next_retry_at: Set(next_retry_at),
            created_at: Set(now),
            updated_at: Set(now),
        };
        am.insert(db).await.expect("insert outbox row");
    }

    async fn outbox_status(db: &DatabaseConnection, id: &str) -> Option<String> {
        OutboxEntity::find_by_id(id.to_string())
            .one(db)
            .await
            .expect("query outbox")
            .map(|r| r.status)
    }

    #[tokio::test]
    async fn outbox_insert_pending_persists_row() {
        let db = fresh_sqlite().await;
        let id = outbox_insert_pending(&db, "wh-x", &fixture_event(), r#"{"a":1}"#)
            .await
            .expect("insert returns id");
        let row = OutboxEntity::find_by_id(id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, STATUS_PENDING);
        assert_eq!(row.attempt_count, 1);
        assert_eq!(row.event_id, "evt_test");
        assert_eq!(row.webhook_id, "wh-x");
    }

    #[tokio::test]
    async fn outbox_finalize_marks_delivered_and_failed() {
        let db = fresh_sqlite().await;
        let id = outbox_insert_pending(&db, "wh-x", &fixture_event(), "{}")
            .await
            .unwrap();
        outbox_finalize(&db, &id, &DeliveryOutcome::Delivered).await;
        assert_eq!(outbox_status(&db, &id).await.as_deref(), Some(STATUS_DELIVERED));

        let id2 = outbox_insert_pending(&db, "wh-y", &fixture_event(), "{}")
            .await
            .unwrap();
        outbox_finalize(&db, &id2, &DeliveryOutcome::Failed("status 400".into())).await;
        let row = OutboxEntity::find_by_id(id2).one(&db).await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_FAILED);
        assert_eq!(row.last_error.as_deref(), Some("status 400"));
    }

    #[tokio::test]
    async fn outbox_finalize_aborted_leaves_pending() {
        // cancel/deactivate must NOT terminalize the row (D-31/D-32).
        let db = fresh_sqlite().await;
        let id = outbox_insert_pending(&db, "wh-x", &fixture_event(), "{}")
            .await
            .unwrap();
        outbox_finalize(&db, &id, &DeliveryOutcome::Aborted).await;
        assert_eq!(outbox_status(&db, &id).await.as_deref(), Some(STATUS_PENDING));
    }

    #[tokio::test]
    async fn redeliver_redrives_pending_past_lease() {
        let (url, hits, _) = spawn_mock(vec![200], None).await;
        let db = fresh_sqlite().await;
        insert_webhook(&db, "wh-rd", &url, 3, 5000, serde_json::json!([]), true).await;
        // Lease already expired (1h ago) → orphaned, must be re-driven.
        insert_outbox_row(&db, "ob-rd", "wh-rd", Utc::now() - ChronoDuration::hours(1), 1)
            .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_string_lossy().into_owned();
        let client = test_client();

        redeliver_due_rows(&db, &registry, &dir_str, &client).await;

        for _ in 0..200 {
            if hits.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1, "row past lease is re-driven");
        for _ in 0..200 {
            if outbox_status(&db, "ob-rd").await.as_deref() == Some(STATUS_DELIVERED) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            outbox_status(&db, "ob-rd").await.as_deref(),
            Some(STATUS_DELIVERED),
            "re-driven row finalizes delivered"
        );
    }

    #[tokio::test]
    async fn redeliver_skips_future_lease() {
        let (url, hits, _) = spawn_mock(vec![200], None).await;
        let db = fresh_sqlite().await;
        insert_webhook(&db, "wh-fut", &url, 3, 5000, serde_json::json!([]), true).await;
        // Lease in the future → a live delivery is presumed in-flight; skip.
        insert_outbox_row(&db, "ob-fut", "wh-fut", Utc::now() + ChronoDuration::hours(1), 1)
            .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_string_lossy().into_owned();
        let client = test_client();

        redeliver_due_rows(&db, &registry, &dir_str, &client).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(hits.load(Ordering::SeqCst), 0, "future-lease row untouched");
        assert_eq!(outbox_status(&db, "ob-fut").await.as_deref(), Some(STATUS_PENDING));
    }

    #[tokio::test]
    async fn redeliver_fails_row_when_webhook_missing() {
        let db = fresh_sqlite().await;
        // Pending row pointing at a webhook that no longer exists.
        insert_outbox_row(&db, "ob-orphan", "wh-gone", Utc::now() - ChronoDuration::hours(1), 1)
            .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_string_lossy().into_owned();
        let client = test_client();

        redeliver_due_rows(&db, &registry, &dir_str, &client).await;

        assert_eq!(
            outbox_status(&db, "ob-orphan").await.as_deref(),
            Some(STATUS_FAILED),
            "row whose webhook is gone is failed, not re-driven forever"
        );
    }

    #[tokio::test]
    async fn redeliver_fails_row_at_max_attempts() {
        let (url, hits, _) = spawn_mock(vec![200], None).await;
        let db = fresh_sqlite().await;
        insert_webhook(&db, "wh-max", &url, 3, 5000, serde_json::json!([]), true).await;
        // attempt_count at the cap → backstop fails it without re-driving.
        insert_outbox_row(
            &db,
            "ob-max",
            "wh-max",
            Utc::now() - ChronoDuration::hours(1),
            OUTBOX_MAX_ATTEMPTS,
        )
        .await;
        let registry = Arc::new(WebhookCancelRegistry::new());
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_string_lossy().into_owned();
        let client = test_client();

        redeliver_due_rows(&db, &registry, &dir_str, &client).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(hits.load(Ordering::SeqCst), 0, "capped row is not re-driven");
        assert_eq!(outbox_status(&db, "ob-max").await.as_deref(), Some(STATUS_FAILED));
    }
}
