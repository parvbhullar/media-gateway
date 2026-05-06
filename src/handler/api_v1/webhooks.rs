//! Phase 7 — Webhooks CRUD (WH-01 / WH-06).
//!
//! Plan 07-02 GREEN body. Replaces the 07-01 stub bodies with full CRUD
//! against `supersip_webhooks`. The `pub fn router() -> Router<AppState>`
//! signature is the Wave-1 invariant: this plan replaces handler bodies
//! WITHOUT touching `mod.rs` (Phase 5 / 6 file-ownership pattern).
//!
//! Endpoints (D-02):
//!   - GET    /webhooks         — list
//!   - POST   /webhooks         — create
//!   - GET    /webhooks/{id}    — fetch by id
//!   - PUT    /webhooks/{id}    — full replacement
//!   - DELETE /webhooks/{id}    — remove
//!
//! Validation (D-04, D-05/D-09, D-26, D-27):
//!   - name: lowercase letters/digits + dashes (URL-safe)
//!   - url: scheme http/https only; localhost / 127.0.0.0/8 / ::1 / fe80::/10
//!     denied. RFC1918 (10/8, 172.16/12, 192.168/16) is EXPLICITLY ALLOWED
//!     per D-27 — operators legitimately webhook to k8s/internal services.
//!   - events: must be subset of WEBHOOK_EVENT_NAMES (D-05). Empty vec
//!     means "subscribe-all" (D-08).
//!   - timeout_ms ∈ [100, 30000], retry_count ∈ [0, 10] (D-04).
//!
//! Deferred to 07-05:
//!   - Synchronous test-event firing on POST (D-28..D-30; needs processor)
//!   - DELETE-triggered cancel of in-flight retries (D-31)
//!   - PUT-triggered cancel of prior retries (D-34)

use std::net::IpAddr;

use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::webhooks::{
    self, Column as WhColumn, Entity as WhEntity, Model as WhModel,
};

// ─── Locked event-name set (D-05) ────────────────────────────────────────

/// The full set of event names operator webhooks can subscribe to. Locked
/// per D-05; D-09 requires that `events` POST/PUT input be a subset.
pub const WEBHOOK_EVENT_NAMES: &[&str] = &[
    "call.started",
    "call.completed",
    "call.failed",
    "recording.completed",
    "transcribe.requested",
    "webhook.test",
];

// ─── Constants (D-04) ────────────────────────────────────────────────────

const MAX_NAME_LEN: usize = 128;
const MIN_TIMEOUT_MS: i32 = 100;
const MAX_TIMEOUT_MS: i32 = 30_000;
const MIN_RETRY_COUNT: i32 = 0;
const MAX_RETRY_COUNT: i32 = 10;
const DEFAULT_TIMEOUT_MS: i32 = 5_000;
const DEFAULT_RETRY_COUNT: i32 = 3;

// ─── Wire types (D-02) ───────────────────────────────────────────────────

/// Operator-facing view of one webhook row. Plaintext secret per D-35
/// (HMAC verification is server-side; whole-DB encryption is a v2.1
/// operator-deploy concern). T-07-02-03 / T-07-02-07: accepted residual.
#[derive(Debug, Serialize)]
pub struct WebhookView {
    pub id: String,
    pub name: String,
    pub url: String,
    pub secret: String,
    pub events: Vec<String>,
    pub description: Option<String>,
    pub is_active: bool,
    pub retry_count: i32,
    pub timeout_ms: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Phase 7 D-29: POST /webhooks response — WebhookView fields PLUS the
/// test-event delivery outcome. `test_delivery` is "succeeded" on 2xx,
/// "failed" otherwise; `test_error` carries the failure message when
/// applicable. WH-05: test failure does NOT roll back the row insert.
#[derive(Debug, Serialize)]
pub struct CreateWebhookResponse {
    #[serde(flatten)]
    pub webhook: WebhookView,
    pub test_delivery: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_error: Option<String>,
}

impl From<&WhModel> for WebhookView {
    fn from(m: &WhModel) -> Self {
        let events = m
            .events
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
            url: m.url.clone(),
            secret: m.secret.clone(),
            events,
            description: m.description.clone(),
            is_active: m.is_active,
            retry_count: m.retry_count,
            timeout_ms: m.timeout_ms,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl From<WhModel> for WebhookView {
    fn from(m: WhModel) -> Self {
        Self::from(&m)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWebhookRequest {
    pub name: String,
    pub url: String,
    pub secret: String,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub retry_count: Option<i32>,
    #[serde(default)]
    pub timeout_ms: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateWebhookRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub retry_count: Option<i32>,
    #[serde(default)]
    pub timeout_ms: Option<i32>,
}

// ─── Validators (pub: re-used by 07-04 / 07-05) ──────────────────────────

/// SSRF write-time URL validator (D-26, D-27).
///
/// Allowed: any http/https URL whose host is NOT in the local-loopback /
/// link-local denylist. RFC1918 ranges (10/8, 172.16/12, 192.168/16) are
/// EXPLICITLY ALLOWED — operators legitimately webhook to internal
/// k8s / service-mesh / corp-network endpoints, and the webhook URL is
/// configured at setup time (not call-time input), so the trust model
/// differs from Phase 6 HttpQuery (which had stricter runtime checks).
///
/// Denied (D-27):
///   - Non-http/https schemes (file:, javascript:, ssh:, ...)
///   - Hostname `localhost` (case-insensitive)
///   - 127.0.0.0/8 IPv4 loopback
///   - ::1 IPv6 loopback
///   - fe80::/10 IPv6 link-local
///   - 0.0.0.0 / :: unspecified
pub fn validate_webhook_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url)
        .map_err(|e| format!("invalid url: {}", e))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "url scheme must be http or https, got '{}'",
            scheme
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "url has no host".to_string())?;

    if host.eq_ignore_ascii_case("localhost") {
        return Err("url host 'localhost' is denied (D-27 SSRF denylist)".to_string());
    }

    // Strip IPv6 brackets if any (host_str usually returns bare host).
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        if is_denylisted_ip(&ip) {
            return Err(format!(
                "url host '{}' is loopback or link-local (D-27 SSRF denylist; \
                 RFC1918 ranges are allowed)",
                host
            ));
        }
    }
    Ok(())
}

/// Per D-27: only loopback + link-local + unspecified are denied.
/// RFC1918 private ranges (10/8, 172.16/12, 192.168/16) are intentionally
/// NOT denied — operators legitimately webhook to internal services.
fn is_denylisted_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fe80::/10 link-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Validate that every name in `events` is in the locked WEBHOOK_EVENT_NAMES
/// set (D-05/D-09). Empty list = subscribe-all (D-08), which is allowed.
pub fn validate_event_names(events: &[String]) -> Result<(), String> {
    for ev in events {
        if !WEBHOOK_EVENT_NAMES.contains(&ev.as_str()) {
            return Err(format!(
                "invalid event name '{}': must be one of {}",
                ev,
                WEBHOOK_EVENT_NAMES.join(", ")
            ));
        }
    }
    Ok(())
}

/// Lowercase letters/digits + dashes (URL-safe). 1..=MAX_NAME_LEN.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(format!(
            "webhook name must be 1-{} chars",
            MAX_NAME_LEN
        ));
    }
    for &b in name.as_bytes() {
        let ok = b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-';
        if !ok {
            return Err(
                "webhook name must contain only lowercase letters, \
                 digits, and dashes"
                    .to_string(),
            );
        }
    }
    Ok(())
}

pub fn validate_timeout_ms(t: i32) -> Result<(), String> {
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&t) {
        return Err(format!(
            "timeout_ms {} out of range [{}, {}]",
            t, MIN_TIMEOUT_MS, MAX_TIMEOUT_MS
        ));
    }
    Ok(())
}

pub fn validate_retry_count(n: i32) -> Result<(), String> {
    if !(MIN_RETRY_COUNT..=MAX_RETRY_COUNT).contains(&n) {
        return Err(format!(
            "retry_count {} out of range [{}, {}]",
            n, MIN_RETRY_COUNT, MAX_RETRY_COUNT
        ));
    }
    Ok(())
}

// ─── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/webhooks", get(list_webhooks).post(create_webhook))
        .route(
            "/webhooks/{id}",
            get(get_webhook).put(update_webhook).delete(delete_webhook),
        )
}

// ─── Handlers ────────────────────────────────────────────────────────────

async fn list_webhooks(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
) -> ApiResult<Json<Vec<WebhookView>>> {
    let db = state.db();
    let cond = Condition::all().add(WhColumn::AccountId.eq(scope.account_id.clone()));
    let rows = WhEntity::find()
        .filter(cond)
        .order_by_asc(WhColumn::Name)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(rows.iter().map(WebhookView::from).collect()))
}

async fn create_webhook(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Json(req): Json<CreateWebhookRequest>,
) -> ApiResult<(StatusCode, Json<CreateWebhookResponse>)> {
    let db = state.db();

    // Field validation (D-04, D-26, D-27, D-09).
    validate_name(&req.name).map_err(ApiError::bad_request)?;
    validate_webhook_url(&req.url).map_err(ApiError::bad_request)?;
    let events = req.events.unwrap_or_default();
    validate_event_names(&events).map_err(ApiError::bad_request)?;
    let retry_count = req.retry_count.unwrap_or(DEFAULT_RETRY_COUNT);
    validate_retry_count(retry_count).map_err(ApiError::bad_request)?;
    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    validate_timeout_ms(timeout_ms).map_err(ApiError::bad_request)?;

    // Pre-check duplicate name → 409.
    let dup = WhEntity::find()
        .filter(WhColumn::Name.eq(req.name.clone()))
        .filter(WhColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "webhook '{}' already exists",
            req.name
        )));
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let events_json: Value = events
        .iter()
        .map(|e| Value::String(e.clone()))
        .collect::<Vec<_>>()
        .into();
    let am = webhooks::ActiveModel {
        id: Set(id),
        name: Set(req.name),
        url: Set(req.url),
        secret: Set(req.secret),
        events: Set(events_json),
        description: Set(req.description),
        is_active: Set(req.is_active.unwrap_or(true)),
        retry_count: Set(retry_count),
        timeout_ms: Set(timeout_ms),
        created_at: Set(now),
        updated_at: Set(now),
        account_id: Set(scope.account_id.clone()),
    };
    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Phase 7 D-28..D-30: synchronous webhook.test fire after row commit.
    // Single-attempt (no retry, no disk fallback). Failure is non-fatal
    // per WH-05: row persists, response carries test_delivery=failed.
    let test_event = crate::proxy::webhook::WebhookEvent {
        event_id: crate::proxy::webhook::new_event_id(),
        event: "webhook.test".to_string(),
        timestamp: crate::proxy::webhook::current_unix_timestamp(),
        data: serde_json::json!({
            "webhook_id": inserted.id,
            "message": "Test event from supersip",
        }),
    };
    let envelope = serde_json::json!({
        "event_id": test_event.event_id,
        "event": test_event.event,
        "timestamp": test_event.timestamp,
        "data": test_event.data,
    });
    let envelope_body = serde_json::to_string(&envelope).unwrap_or_default();
    let client = reqwest::Client::builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    // Cap the inline call by the webhook's own timeout_ms (D-04, T-07-05-03);
    // perform_attempt also enforces this on the request, but we add an outer
    // tokio::time::timeout for defense in depth.
    let timeout = std::time::Duration::from_millis(inserted.timeout_ms.max(1) as u64);
    let outcome = tokio::time::timeout(
        timeout,
        crate::proxy::webhook::deliver_test_event(
            &inserted,
            &test_event,
            &envelope_body,
            &client,
        ),
    )
    .await;
    let (test_delivery, test_error) = match outcome {
        Ok(Ok(())) => ("succeeded".to_string(), None),
        Ok(Err(e)) => ("failed".to_string(), Some(e)),
        Err(_) => ("failed".to_string(), Some("timeout".to_string())),
    };

    let response = CreateWebhookResponse {
        webhook: WebhookView::from(inserted),
        test_delivery,
        test_error,
    };
    Ok((StatusCode::CREATED, Json(response)))
}

async fn get_webhook(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
) -> ApiResult<Json<WebhookView>> {
    let db = state.db();
    let row = WhEntity::find()
        .filter(WhColumn::Id.eq(id.clone()))
        .filter(WhColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("webhook '{}' not found", id))
        })?;
    Ok(Json(WebhookView::from(row)))
}

async fn update_webhook(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
    Json(req): Json<UpdateWebhookRequest>,
) -> ApiResult<Json<WebhookView>> {
    let db = state.db();

    // Phase 7 D-34: cancel any in-flight retries BEFORE applying changes.
    // URL/secret/events may have changed; we MUST NOT continue retrying the
    // old delivery against the new row state (T-07-05-06).
    state.webhook_cancel_registry().cancel(&id);

    let existing = WhEntity::find()
        .filter(WhColumn::Id.eq(id.clone()))
        .filter(WhColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("webhook '{}' not found", id))
        })?;

    // Validate any provided fields (full-replacement semantics: caller
    // can update any subset; per-field range checks always run).
    if let Some(ref n) = req.name {
        validate_name(n).map_err(ApiError::bad_request)?;
    }
    if let Some(ref u) = req.url {
        validate_webhook_url(u).map_err(ApiError::bad_request)?;
    }
    if let Some(ref ev) = req.events {
        validate_event_names(ev).map_err(ApiError::bad_request)?;
    }
    if let Some(rc) = req.retry_count {
        validate_retry_count(rc).map_err(ApiError::bad_request)?;
    }
    if let Some(t) = req.timeout_ms {
        validate_timeout_ms(t).map_err(ApiError::bad_request)?;
    }

    // Duplicate-name check on rename.
    if let Some(ref new_name) = req.name {
        if new_name != &existing.name {
            let dup = WhEntity::find()
                .filter(WhColumn::Name.eq(new_name.clone()))
                .filter(WhColumn::AccountId.eq(scope.account_id.clone()))
                .one(db)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;
            if dup.is_some() {
                return Err(ApiError::conflict(format!(
                    "webhook '{}' already exists",
                    new_name
                )));
            }
        }
    }

    let mut am: webhooks::ActiveModel = existing.into();
    if let Some(n) = req.name {
        am.name = Set(n);
    }
    if let Some(u) = req.url {
        am.url = Set(u);
    }
    if let Some(s) = req.secret {
        am.secret = Set(s);
    }
    if let Some(ev) = req.events {
        let events_json: Value = ev
            .iter()
            .map(|e| Value::String(e.clone()))
            .collect::<Vec<_>>()
            .into();
        am.events = Set(events_json);
    }
    if let Some(d) = req.description {
        am.description = Set(Some(d));
    }
    if let Some(act) = req.is_active {
        am.is_active = Set(act);
    }
    if let Some(rc) = req.retry_count {
        am.retry_count = Set(rc);
    }
    if let Some(t) = req.timeout_ms {
        am.timeout_ms = Set(t);
    }
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // 07-05 wires WebhookCancelRegistry::cancel(id) here per D-34 to
    // cancel any prior in-flight retry for this webhook; 07-02 only
    // updates the row.
    Ok(Json(WebhookView::from(updated)))
}

async fn delete_webhook(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();

    // Phase 7 D-31: cancel any in-flight retries BEFORE deleting the row so
    // the delivery loop's pre-flight DB recheck doesn't race with the row's
    // disappearance.
    state.webhook_cancel_registry().cancel(&id);

    let existing = WhEntity::find()
        .filter(WhColumn::Id.eq(id.clone()))
        .filter(WhColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("webhook '{}' not found", id))
        })?;

    WhEntity::delete_by_id(existing.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // 07-05 wires WebhookCancelRegistry::cancel(id) here per D-31 to
    // abort any in-flight retries for the deleted webhook; 07-02 only
    // deletes the row.
    Ok(StatusCode::NO_CONTENT)
}

// ─── Validator unit tests ────────────────────────────────────────────────

#[cfg(test)]
mod validators {
    use super::*;

    // URL validator
    #[test]
    fn url_https_ok() {
        assert!(validate_webhook_url("https://example.com").is_ok());
    }

    #[test]
    fn url_rfc1918_10_allowed_d27() {
        // D-27 explicit allow.
        assert!(validate_webhook_url("http://10.0.0.5/hook").is_ok());
    }

    #[test]
    fn url_rfc1918_192_168_allowed_d27() {
        assert!(validate_webhook_url("http://192.168.1.1/hook").is_ok());
    }

    #[test]
    fn url_rfc1918_172_16_allowed_d27() {
        assert!(validate_webhook_url("http://172.16.5.5/hook").is_ok());
    }

    #[test]
    fn url_localhost_denied() {
        assert!(validate_webhook_url("http://localhost:8080").is_err());
    }

    #[test]
    fn url_loopback_v4_denied() {
        assert!(validate_webhook_url("http://127.0.0.1").is_err());
    }

    #[test]
    fn url_loopback_v6_denied() {
        assert!(validate_webhook_url("http://[::1]").is_err());
    }

    #[test]
    fn url_link_local_v6_denied() {
        assert!(validate_webhook_url("http://[fe80::1]").is_err());
    }

    #[test]
    fn url_file_scheme_denied() {
        assert!(validate_webhook_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn url_javascript_scheme_denied() {
        assert!(validate_webhook_url("javascript:alert(1)").is_err());
    }

    // Event-name validator
    #[test]
    fn events_valid_subset_ok() {
        assert!(validate_event_names(&["call.completed".to_string()]).is_ok());
    }

    #[test]
    fn events_empty_ok_subscribe_all() {
        assert!(validate_event_names(&[]).is_ok());
    }

    #[test]
    fn events_unknown_rejected_with_list() {
        let err = validate_event_names(&["bogus.event".to_string()]).unwrap_err();
        assert!(err.contains("call.started"), "msg should list valid: {}", err);
        assert!(err.contains("webhook.test"), "msg should list valid: {}", err);
    }

    // Name validator
    #[test]
    fn name_lowercase_dashes_ok() {
        assert!(validate_name("my-hook").is_ok());
    }

    #[test]
    fn name_uppercase_rejected() {
        assert!(validate_name("MY-HOOK").is_err());
    }

    #[test]
    fn name_underscore_rejected() {
        assert!(validate_name("my_hook").is_err());
    }

    // Numeric range validators
    #[test]
    fn timeout_below_min_rejected() {
        assert!(validate_timeout_ms(50).is_err());
    }

    #[test]
    fn timeout_above_max_rejected() {
        assert!(validate_timeout_ms(40_000).is_err());
    }

    #[test]
    fn timeout_at_min_ok() {
        assert!(validate_timeout_ms(MIN_TIMEOUT_MS).is_ok());
    }

    #[test]
    fn timeout_at_max_ok() {
        assert!(validate_timeout_ms(MAX_TIMEOUT_MS).is_ok());
    }

    #[test]
    fn retry_above_max_rejected() {
        assert!(validate_retry_count(11).is_err());
    }

    #[test]
    fn retry_at_zero_ok() {
        assert!(validate_retry_count(0).is_ok());
    }

    #[test]
    fn retry_at_max_ok() {
        assert!(validate_retry_count(10).is_ok());
    }
}
