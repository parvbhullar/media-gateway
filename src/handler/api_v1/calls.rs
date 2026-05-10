//! `/api/v1/calls` — active calls list + detail (Phase 4, Plan 04-01).
//!
//! Routes:
//! - `GET /api/v1/calls` — paginated, filtered snapshot of
//!   `ActiveProxyCallRegistry::list_recent` (CALL-01).
//! - `GET /api/v1/calls/{id}` — rich `ActiveCallView` with
//!   `SessionSnapshot` nested under `snapshot` (CALL-02).
//!
//! Command routes (`/hangup`, `/mute`, `/unmute`, `/transfer`, `/play`,
//! `/speak`, `/dtmf`, `/record`) land in plans 04-02..04-05 on top of this
//! router. CALL-10 holds by construction — those handlers route through
//! `dispatch_console_command` verbatim.
//!
//! ## Transcribe marker contract (Plan 04-05, D-18)
//!
//! When `/record` is called with `{"transcribe": true}`, the handler drops
//! an empty marker file at `<recording_path>.transcribe.marker`. Phase 7
//! (Webhooks) consumers of `callrecord/` completion events check for this
//! sibling marker and trigger transcription. If no STT infrastructure is
//! wired yet, the marker sits harmlessly until a future phase consumes it.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::app::AppState;
use crate::console::handlers::call_control::{
    ApiPlayOptions, CallCommandPayload, Leg, PlaySource,
};
use crate::call::runtime::{CommandResult, dispatch_console_command};
use crate::handler::api_v1::common::{PaginatedResponse, Pagination};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::proxy::active_call_registry::{
    ActiveProxyCallEntry, ActiveProxyCallRegistry, ActiveProxyCallStatus,
};
use crate::proxy::proxy_call::sip_session::{SessionSnapshot, SipSessionHandle};

// ── Wire types (SHELL-04) ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ActiveCallView {
    pub session_id: String,
    pub caller: Option<String>,
    pub callee: Option<String>,
    pub direction: String,
    pub started_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SessionSnapshot>,
}

/// Query for `GET /api/v1/calls`.
///
/// Pagination fields (`page`, `page_size`) are inlined here rather than
/// `#[serde(flatten)]`-ed from `Pagination` because `serde_urlencoded`
/// (the deserializer behind `axum::Query`) does not support `flatten`
/// across typed fields. This mirrors the pattern in
/// `src/handler/api_v1/dids.rs`.
#[derive(Debug, Deserialize)]
pub struct CallListQuery {
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub page_size: Option<u64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub caller: Option<String>,
    #[serde(default)]
    pub callee: Option<String>,
    /// Substring match against either caller or callee.
    /// ANDed with `caller`/`callee` if all are supplied.
    #[serde(default)]
    pub number: Option<String>,
    /// RFC-3339 timestamp parsed by the handler (not serde) so we can
    /// return a uniform `ApiError::bad_request` on unparseable values
    /// rather than a serde rejection with a different envelope shape.
    #[serde(default)]
    pub since: Option<String>,
}

impl CallListQuery {
    fn pagination(&self) -> Pagination {
        Pagination {
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

// ── Phase 4 Plan 04-02 — command request bodies ──────────────────────────

/// Body for `POST /api/v1/calls/{id}/hangup`. Both fields optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HangupRequest {
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub code: Option<u16>,
}

/// Body for `POST /api/v1/calls/{id}/{mute,unmute}`. `leg` required.
///
/// Uses `String` (not `Leg`) so the handler can reject invalid values with a
/// clean `ApiError::bad_request` message rather than serde's default shape.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegRequest {
    pub leg: String,
}

// ── Phase 4 Plan 04-03 — transfer request bodies (D-19, D-20) ───────────

/// Body for `POST /api/v1/calls/{id}/transfer`. Tagged enum per D-19:
///
/// ```json
/// {"type": "blind",    "target": "sip:1001@x.com"}          // leg defaults to callee
/// {"type": "attended", "target": "+14155551234", "leg": "caller"}
/// ```
///
/// `leg` is a `String` (not `Leg`) so the handler validates via
/// `validate_leg` and returns the branded `ApiError::bad_request` shape on
/// invalid values rather than serde's default rejection envelope.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum TransferRequest {
    Blind {
        target: String,
        #[serde(default)]
        leg: Option<String>,
    },
    Attended {
        target: String,
        #[serde(default)]
        leg: Option<String>,
    },
}

/// Body for `POST /api/v1/calls/{id}/transfer/{complete,cancel}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsultLegRequest {
    pub consult_leg: String,
}

// ── Phase 4 Plan 04-04 — media-command request bodies (D-12, D-13, D-14) ─

/// Body for `POST /api/v1/calls/{id}/play`. `source` is tagged:
///
/// ```json
/// {"source":{"type":"file","path":"/abs/path.wav"},"leg":"callee","loop":true}
/// {"source":{"type":"url","url":"https://..."}}   // 400 not_supported pre-dispatch
/// ```
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayRequest {
    pub source: PlaySource,
    #[serde(default)]
    pub leg: Option<String>,
    #[serde(default, rename = "loop")]
    pub loop_playback: Option<bool>,
    #[serde(default)]
    pub interrupt_on_dtmf: Option<bool>,
}

/// Body for `POST /api/v1/calls/{id}/speak`. Phase 4 always rejects
/// pre-dispatch per D-13 (TTS engine not wired).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakRequest {
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub leg: Option<String>,
}

/// Body for `POST /api/v1/calls/{id}/dtmf`. Digits validated by
/// `validate_dtmf_digits`; default leg resolved from `entry.direction`
/// when omitted.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DtmfRequest {
    pub digits: String,
    #[serde(default)]
    pub duration_ms: Option<u32>,
    #[serde(default)]
    pub inter_digit_ms: Option<u32>,
    #[serde(default)]
    pub leg: Option<String>,
}

// ── Phase 4 Plan 04-05 — record request body (D-18) ─────────────────────

/// Body for `POST /api/v1/calls/{id}/record`. All fields optional.
///
/// - `path`: absolute path inside the recorder tree; auto-generated when absent.
/// - `format`: `"wav"` (default) or `"mp3"`.
/// - `beep`: play a beep before recording starts (default `false`).
/// - `max_duration_secs`: hard cap on recording length.
/// - `transcribe`: when `true`, handler drops a `.transcribe.marker` sibling
///   file next to the recording for Phase 7 post-processing.
#[derive(Debug, Default, Deserialize)]
pub struct RecordRequest {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub beep: Option<bool>,
    #[serde(default)]
    pub max_duration_secs: Option<u32>,
    #[serde(default)]
    pub transcribe: Option<bool>,
}

// ── Router ───────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/calls", get(list_active_calls))
        .route("/calls/{id}", get(get_active_call))
        // Phase 4 Plan 04-02 — CALL-03, CALL-05, CALL-10
        .route("/calls/{id}/hangup", post(hangup_call))
        .route("/calls/{id}/mute", post(mute_call))
        .route("/calls/{id}/unmute", post(unmute_call))
        // Phase 4 Plan 04-03 — CALL-04, CALL-10
        .route("/calls/{id}/transfer", post(transfer_call))
        .route("/calls/{id}/transfer/complete", post(transfer_complete))
        .route("/calls/{id}/transfer/cancel", post(transfer_cancel))
        // Phase 4 Plan 04-04 — CALL-06, CALL-07, CALL-08, CALL-10
        .route("/calls/{id}/play", post(play_on_call))
        .route("/calls/{id}/speak", post(speak_on_call))
        .route("/calls/{id}/dtmf", post(dtmf_on_call))
        // Phase 4 Plan 04-05 — CALL-09, CALL-10
        .route("/calls/{id}/record", post(record_call))
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn status_to_str(s: ActiveProxyCallStatus) -> &'static str {
    match s {
        ActiveProxyCallStatus::Ringing => "ringing",
        ActiveProxyCallStatus::Talking => "talking",
    }
}

fn parse_status_filter(raw: &str) -> ApiResult<ActiveProxyCallStatus> {
    match raw.to_ascii_lowercase().as_str() {
        "ringing" => Ok(ActiveProxyCallStatus::Ringing),
        "talking" => Ok(ActiveProxyCallStatus::Talking),
        _ => Err(ApiError::bad_request(format!(
            "invalid status filter '{}' (expected 'ringing' or 'talking')",
            raw
        ))),
    }
}

fn parse_direction_filter(raw: &str) -> ApiResult<String> {
    match raw.to_ascii_lowercase().as_str() {
        "inbound" => Ok("inbound".to_string()),
        "outbound" => Ok("outbound".to_string()),
        "internal" => Ok("internal".to_string()),
        _ => Err(ApiError::bad_request(format!(
            "invalid direction filter '{}' (expected 'inbound', 'outbound', or 'internal')",
            raw
        ))),
    }
}

fn parse_since(raw: &str) -> ApiResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            ApiError::bad_request(format!(
                "invalid 'since' filter '{}': {} (expected RFC-3339)",
                raw, e
            ))
        })
}

fn substring_matches_ci(haystack_opt: &Option<String>, needle: &str) -> bool {
    haystack_opt
        .as_ref()
        .map(|h| h.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()))
        .unwrap_or(false)
}

fn entry_to_view(
    entry: ActiveProxyCallEntry,
    snapshot: Option<SessionSnapshot>,
) -> ActiveCallView {
    ActiveCallView {
        session_id: entry.session_id,
        caller: entry.caller,
        callee: entry.callee,
        direction: entry.direction,
        started_at: entry.started_at,
        answered_at: entry.answered_at,
        status: status_to_str(entry.status).to_string(),
        snapshot,
    }
}

// ── Phase 4 Plan 04-02 — command dispatch helpers (D-07, D-08, D-09) ─────

/// Validate a `leg` string (case-insensitive) into the typed `Leg` enum.
fn validate_leg(raw: &str) -> ApiResult<Leg> {
    match raw.to_ascii_lowercase().as_str() {
        "caller" => Ok(Leg::Caller),
        "callee" => Ok(Leg::Callee),
        _ => Err(ApiError::bad_request(format!(
            "invalid leg '{}' (expected 'caller' or 'callee')",
            raw
        ))),
    }
}

/// 409 pre-check per D-09: mute/unmute require a negotiated media session.
/// We look at the cached snapshot — if it's missing or reports fewer than two
/// legs, the media tracks aren't ready yet.
fn require_media_ready(handle: &SipSessionHandle) -> ApiResult<()> {
    match handle.snapshot() {
        Some(s) if s.leg_count >= 2 => Ok(()),
        _ => Err(ApiError::conflict("media tracks not yet established")),
    }
}

/// 404 pre-check per D-08: resolve the session handle before any dispatch
/// attempt so "unknown session" is always a clean 404 and never a dispatch-
/// level failure.
fn require_session(
    registry: &Arc<ActiveProxyCallRegistry>,
    session_id: &str,
) -> ApiResult<SipSessionHandle> {
    registry
        .get_handle(session_id)
        .ok_or_else(|| ApiError::not_found(format!("active call '{}' not found", session_id)))
}

/// Normalize a transfer target to a validated SIP URI string (Plan 04-03,
/// D-22).
///
/// Accepts:
/// - A SIP URI (`sip:...` or `sips:...`) — validated via
///   `rsipstack::sip::Uri::try_from` and passed through unchanged.
/// - A bare E.164 number (`+14155551234`) — normalized to
///   `sip:<number>@<external_ip>` where `external_ip` defaults to
///   `"127.0.0.1"` when `Config::external_ip` is `None`. Production
///   deployments MUST set `external_ip` so transfers reach the right host.
///
/// Everything else → 400 `bad_request`. This mirrors the validation in
/// `transfer_to_uri` (`src/proxy/proxy_call/session.rs:2410`) so invalid
/// URIs are rejected at the API boundary rather than blowing up at REFER
/// time with an opaque 500.
fn parse_target(raw: &str, external_ip: Option<&str>) -> ApiResult<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ApiError::bad_request("empty target"));
    }
    if raw.starts_with("sip:") || raw.starts_with("sips:") {
        rsipstack::sip::Uri::try_from(raw).map_err(|e| {
            ApiError::bad_request(format!("invalid target URI '{}': {:?}", raw, e))
        })?;
        return Ok(raw.to_string());
    }
    // Bare E.164: '+' prefix + all-digit body.
    if raw.starts_with('+')
        && raw.len() > 1
        && raw[1..].chars().all(|c| c.is_ascii_digit())
    {
        let host = external_ip.unwrap_or("127.0.0.1");
        let uri = format!("sip:{}@{}", raw, host);
        rsipstack::sip::Uri::try_from(uri.as_str()).map_err(|e| {
            ApiError::bad_request(format!(
                "invalid external_ip configuration '{}': {:?}",
                host, e
            ))
        })?;
        return Ok(uri);
    }
    Err(ApiError::bad_request(format!(
        "invalid target '{}': expected SIP URI (sip:/sips:) or E.164 (+...)",
        raw
    )))
}

/// Map `dispatch_console_command`'s `Result<CommandResult>` to an HTTP
/// response per D-07.
///
/// Successful dispatch → 200 `{"message":"dispatched"}` with optional `extra`
/// fields merged in (used by plan 04-03 for `consult_leg_id` and plan 04-05
/// for the recording `path`).
///
/// Plans 04-03/04/05 reuse this helper verbatim — it is the single entry
/// point that owns the dispatch → HTTP status mapping.
fn map_command_result(
    result: anyhow::Result<CommandResult>,
    extra: Option<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    let cr = result.map_err(|e| ApiError::internal(format!("{}", e)))?;
    if cr.success {
        let mut body = json!({"message": "dispatched"});
        if let Some(extra_val) = extra {
            if let (Some(obj), Some(extra_obj)) =
                (body.as_object_mut(), extra_val.as_object())
            {
                for (k, v) in extra_obj {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        return Ok(Json(body));
    }
    let msg = cr.message.unwrap_or_default();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("not found") {
        return Err(ApiError::not_found(msg));
    }
    if lower.contains("failed to dispatch") {
        return Err(ApiError::conflict(format!(
            "command dispatch failed: {}",
            msg
        )));
    }
    if lower.contains("not supported") {
        // Safety-net status mapping (per research fix option #1 — plan 04-04
        // ships pre-dispatch probes as the primary defense). Returns 400
        // (not 501) because the request is semantically malformed for our
        // current deployment — the feature is conceptually supported but
        // unwired.
        return Err(ApiError::bad_request(msg));
    }
    Err(ApiError::internal(msg))
}

// ── Handlers ─────────────────────────────────────────────────────────────

async fn list_active_calls(
    State(state): State<AppState>,
    Query(q): Query<CallListQuery>,
) -> ApiResult<Json<PaginatedResponse<ActiveCallView>>> {
    // Parse + validate filters up-front (D-04).
    let status_filter = q
        .status
        .as_deref()
        .map(parse_status_filter)
        .transpose()?;
    let direction_filter = q
        .direction
        .as_deref()
        .map(parse_direction_filter)
        .transpose()?;
    let since_filter = q.since.as_deref().map(parse_since).transpose()?;

    let pagination = q.pagination();
    let registry = state.sip_server().inner.active_call_registry.clone();

    // `list_recent(usize::MAX)` already sorts by `started_at desc`.
    let mut entries = registry.list_recent(usize::MAX);

    // Apply filters.
    entries.retain(|e| {
        if let Some(s) = status_filter {
            if e.status != s {
                return false;
            }
        }
        if let Some(ref d) = direction_filter {
            if !e.direction.eq_ignore_ascii_case(d) {
                return false;
            }
        }
        if let Some(ref needle) = q.caller {
            if !substring_matches_ci(&e.caller, needle) {
                return false;
            }
        }
        if let Some(ref needle) = q.callee {
            if !substring_matches_ci(&e.callee, needle) {
                return false;
            }
        }
        if let Some(ref needle) = q.number {
            if !substring_matches_ci(&e.caller, needle)
                && !substring_matches_ci(&e.callee, needle)
            {
                return false;
            }
        }
        if let Some(since) = since_filter {
            if e.started_at < since {
                return false;
            }
        }
        true
    });

    let total = entries.len() as u64;
    let offset = pagination.offset() as usize;
    let limit = pagination.limit() as usize;

    let items: Vec<ActiveCallView> = entries
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|e| {
            let snap = registry
                .get_handle(&e.session_id)
                .and_then(|h| h.snapshot());
            entry_to_view(e, snap)
        })
        .collect();

    Ok(Json(PaginatedResponse::new(
        items,
        pagination.page,
        pagination.limit(),
        total,
    )))
}

async fn get_active_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ActiveCallView>> {
    let registry = state.sip_server().inner.active_call_registry.clone();

    let entry = registry
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("active call '{}' not found", id)))?;

    let snap = registry
        .get_handle(&id)
        .and_then(|h| h.snapshot());

    Ok(Json(entry_to_view(entry, snap)))
}

// ── Phase 4 Plan 04-02 — command handlers (CALL-03, CALL-05, CALL-10) ────
//
// All three routes dispatch through `dispatch_console_command` verbatim so
// CALL-10's "existing dispatch path" property holds by construction.

async fn hangup_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<HangupRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let registry = state.sip_server().inner.active_call_registry.clone();
    // 404 pre-check before dispatch (D-08).
    let _ = require_session(&registry, &id)?;

    let payload = CallCommandPayload::ApiHangup {
        reason: req.reason,
        code: req.code,
    };
    map_command_result(dispatch_console_command(&registry, &id, payload), None)
}

async fn mute_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<LegRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let leg = validate_leg(&req.leg)?;
    let registry = state.sip_server().inner.active_call_registry.clone();
    let handle = require_session(&registry, &id)?;
    require_media_ready(&handle)?;

    let payload = CallCommandPayload::ApiMute { leg };
    map_command_result(dispatch_console_command(&registry, &id, payload), None)
}

async fn unmute_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<LegRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let leg = validate_leg(&req.leg)?;
    let registry = state.sip_server().inner.active_call_registry.clone();
    let handle = require_session(&registry, &id)?;
    require_media_ready(&handle)?;

    let payload = CallCommandPayload::ApiUnmute { leg };
    map_command_result(dispatch_console_command(&registry, &id, payload), None)
}

// ── Phase 4 Plan 04-03 — transfer handlers (CALL-04, CALL-10) ────────────
//
// All three routes dispatch through `dispatch_console_command` verbatim so
// CALL-10's "existing dispatch path" property holds by construction. Target
// normalization happens in `parse_target` before dispatch (D-22). Attended
// transfers read `pending_consult_leg_id` from the handle's snapshot
// post-dispatch and surface it via `map_command_result`'s `extra`
// parameter — the session-layer attended-transfer handler owns stamping
// the snapshot field BEFORE returning per D-20.

async fn transfer_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TransferRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let registry = state.sip_server().inner.active_call_registry.clone();
    let handle = require_session(&registry, &id)?;
    let external_ip = state.config().external_ip.as_deref();

    let (target_raw, leg_raw, attended) = match req {
        TransferRequest::Blind { target, leg } => (target, leg, false),
        TransferRequest::Attended { target, leg } => (target, leg, true),
    };

    let target = parse_target(&target_raw, external_ip)?;
    let leg = leg_raw
        .as_deref()
        .map(validate_leg)
        .transpose()?
        // D-21: default leg = callee when omitted.
        .unwrap_or(Leg::Callee);

    let payload = if attended {
        CallCommandPayload::AttendedTransferStart {
            target,
            leg: Some(leg),
        }
    } else {
        CallCommandPayload::BlindTransfer {
            target,
            leg: Some(leg),
        }
    };

    let dispatch_result = dispatch_console_command(&registry, &id, payload);

    // For attended transfers, read the snapshot post-dispatch and surface
    // `pending_consult_leg_id` in the response body. Best-effort per
    // threat register T-04-03-05: if the SIP session hasn't stamped the
    // snapshot yet, the client sees a 200 without `consult_leg_id` and
    // can retry or fall back to hangup-webhook-driven tracking.
    let extra = if attended {
        handle
            .snapshot()
            .and_then(|s| s.pending_consult_leg_id)
            .map(|consult| json!({ "consult_leg_id": consult }))
    } else {
        None
    };

    map_command_result(dispatch_result, extra)
}

async fn transfer_complete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ConsultLegRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let registry = state.sip_server().inner.active_call_registry.clone();
    // 404 pre-check per D-08.
    let _ = require_session(&registry, &id)?;

    let payload = CallCommandPayload::AttendedTransferComplete {
        consult_leg: req.consult_leg,
    };
    map_command_result(dispatch_console_command(&registry, &id, payload), None)
}

async fn transfer_cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ConsultLegRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let registry = state.sip_server().inner.active_call_registry.clone();
    // 404 pre-check per D-08.
    let _ = require_session(&registry, &id)?;

    let payload = CallCommandPayload::AttendedTransferCancel {
        consult_leg: req.consult_leg,
    };
    map_command_result(dispatch_console_command(&registry, &id, payload), None)
}

// ── Phase 4 Plan 04-04 — media-command helpers (D-12, D-13, D-14) ────────

/// Validate DTMF digits against the allow-list `[0-9A-Da-d*#]` (D-14).
///
/// Rejects empty strings and any character outside the RFC-4733 DTMF set.
fn validate_dtmf_digits(digits: &str) -> ApiResult<()> {
    if digits.is_empty() {
        return Err(ApiError::bad_request("empty dtmf digits"));
    }
    for c in digits.chars() {
        let ok = matches!(c, '0'..='9' | 'A'..='D' | 'a'..='d' | '*' | '#');
        if !ok {
            return Err(ApiError::bad_request(format!(
                "invalid dtmf char '{}' (allowed: 0-9, A-D, *, #)",
                c
            )));
        }
    }
    Ok(())
}

/// Resolve the default DTMF target leg from the call entry's direction
/// when the request body omits `leg` (D-14).
///
/// - `inbound` → DTMF targets the `caller` leg (the party that dialed in).
/// - everything else (including `outbound`) → `callee` leg.
fn default_leg_from_direction(direction: &str) -> Leg {
    if direction.eq_ignore_ascii_case("inbound") {
        Leg::Caller
    } else {
        Leg::Callee
    }
}

// ── Phase 4 Plan 04-04 — media-command handlers (CALL-06, 07, 08, 10) ────

async fn play_on_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PlayRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let registry = state.sip_server().inner.active_call_registry.clone();
    let _ = require_session(&registry, &id)?;

    // Pre-dispatch variant probe (RESEARCH §6 — primary defense).
    // `handle_play` in the session layer hard-rejects non-File variants
    // with a message that would otherwise leak through as 500. Rejecting
    // here keeps the 4xx clean.
    if let PlaySource::Url { .. } = req.source {
        return Err(ApiError::not_supported(
            "url playback not wired; see CALL-06 deferred item",
        ));
    }

    let leg = req
        .leg
        .as_deref()
        .map(validate_leg)
        .transpose()?
        // D-21 default leg = callee when omitted.
        .unwrap_or(Leg::Callee);

    // Build `ApiPlayOptions` only if the caller supplied at least one
    // option — otherwise leave `options: None` so the session layer can
    // use its own defaults (see `PlayOptions::default`).
    let options = if req.loop_playback.is_some() || req.interrupt_on_dtmf.is_some() {
        Some(ApiPlayOptions {
            loop_playback: req.loop_playback.unwrap_or(false),
            interrupt_on_dtmf: req.interrupt_on_dtmf.unwrap_or(true),
        })
    } else {
        None
    };

    let payload = CallCommandPayload::Play {
        source: req.source,
        leg: Some(leg),
        options,
    };
    map_command_result(dispatch_console_command(&registry, &id, payload), None)
}

async fn speak_on_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SpeakRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let registry = state.sip_server().inner.active_call_registry.clone();
    // 404 pre-check so unknown sessions still get the D-08-clean 404
    // response rather than a 400 that masks the missing session.
    let _ = require_session(&registry, &id)?;

    // D-13 + RESEARCH §6: TTS is not wired in `handle_play` (which
    // hard-rejects non-File variants). Phase 4 always rejects `/speak`
    // up-front rather than letting dispatch fall through to a confusing
    // 500. Consume the body so `deny_unknown_fields` still catches
    // malformed input before we short-circuit.
    let _ = (req.text, req.voice, req.leg);
    Err(ApiError::not_supported(
        "tts engine not wired; see CALL-07 deferred item",
    ))
}

async fn dtmf_on_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<DtmfRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    validate_dtmf_digits(&req.digits)?;

    let registry = state.sip_server().inner.active_call_registry.clone();
    // Resolve the session entry once — we need it for `direction` when
    // `leg` is omitted. This also serves as the D-08 404 pre-check.
    let entry = registry
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("active call '{}' not found", id)))?;

    let leg = match req.leg.as_deref() {
        Some(raw) => validate_leg(raw)?,
        None => default_leg_from_direction(&entry.direction),
    };

    let payload = CallCommandPayload::Dtmf {
        digits: req.digits,
        duration_ms: req.duration_ms,
        inter_digit_ms: req.inter_digit_ms,
        leg: Some(leg),
    };
    map_command_result(dispatch_console_command(&registry, &id, payload), None)
}

// ── Phase 4 Plan 04-05 — record helpers + handler (CALL-09, D-18) ────────

/// Validate and normalise the recording format string.
///
/// Returns `"wav"` when `raw` is `None`. Accepts `"wav"` and `"mp3"` (case-
/// insensitive). All other values → 400.
fn validate_recording_format(raw: Option<&str>) -> ApiResult<String> {
    match raw {
        None => Ok("wav".to_string()),
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "wav" | "mp3" => Ok(s.to_ascii_lowercase()),
            _ => Err(ApiError::bad_request(format!(
                "invalid format '{}' (expected 'wav' or 'mp3')",
                s
            ))),
        },
    }
}

/// Resolve the recording output path.
///
/// When `explicit` is `None` the path is auto-generated as
/// `<recorder_root>/<session_id>-<unix_ts>.<format>`.
///
/// When `explicit` is supplied the function verifies:
/// 1. Not empty / whitespace-only.
/// 2. Does not contain `".."` (path traversal guard).
/// 3. Is absolute.
/// 4. Starts with the `recorder_root` tree (confinement guard).
fn resolve_recording_path(
    explicit: Option<&str>,
    session_id: &str,
    format: &str,
    recorder_path: &str,
) -> ApiResult<String> {
    use std::path::{Path, PathBuf};
    let recorder_root = PathBuf::from(recorder_path);
    if let Some(raw) = explicit {
        if raw.trim().is_empty() {
            return Err(ApiError::bad_request("empty recording path"));
        }
        if raw.contains("..") {
            return Err(ApiError::bad_request(
                "recording path must not contain '..'",
            ));
        }
        let p = Path::new(raw);
        if !p.is_absolute() {
            return Err(ApiError::bad_request("recording path must be absolute"));
        }
        if !p.starts_with(&recorder_root) {
            return Err(ApiError::bad_request(format!(
                "recording path must be inside recorder tree '{}'",
                recorder_root.display()
            )));
        }
        return Ok(raw.to_string());
    }
    let ts = chrono::Utc::now().timestamp();
    let filename = format!("{}-{}.{}", session_id, ts, format);
    Ok(recorder_root.join(filename).to_string_lossy().to_string())
}

/// Best-effort: drop an empty marker file next to the recording so Phase 7
/// post-processing consumers know to transcribe this recording.
///
/// Failures are logged as warnings — a missing marker is never fatal.
///
/// Phase 4 D-18: transcribe marker → Phase 7 transcribe.requested event.
/// When the marker WRITE succeeds AND a webhook sender is wired, emit a
/// `transcribe.requested` event (D-07 envelope).
fn maybe_drop_transcribe_marker(
    path: &str,
    transcribe: bool,
    session_id: &str,
    webhook_sender: Option<&crate::proxy::webhook::WebhookEventSender>,
) {
    if !transcribe {
        return;
    }
    let marker = format!("{}.transcribe.marker", path);
    match std::fs::File::create(&marker) {
        Ok(_) => {
            tracing::info!("dropped transcribe marker: {}", marker);
            if let Some(sender) = webhook_sender {
                let event = crate::proxy::webhook::WebhookEvent {
                    event_id: crate::proxy::webhook::new_event_id(),
                    event: "transcribe.requested".to_string(),
                    timestamp: crate::proxy::webhook::current_unix_timestamp(),
                    data: serde_json::json!({
                        "session_id": session_id,
                        "recording_path": path,
                        "marker_path": marker,
                    }),
                };
                let _ = sender.send(event);
            }
        }
        Err(e) => tracing::warn!("failed to drop transcribe marker '{}': {}", marker, e),
    }
}

/// Build the `recording.completed` envelope (D-07). Best-effort metadata —
/// `size_bytes` reads the file metadata; failure → 0.
async fn build_recording_completed_event(
    session_id: &str,
    recording_path: &str,
    format: &str,
) -> crate::proxy::webhook::WebhookEvent {
    let size_bytes = tokio::fs::metadata(recording_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    crate::proxy::webhook::WebhookEvent {
        event_id: crate::proxy::webhook::new_event_id(),
        event: "recording.completed".to_string(),
        timestamp: crate::proxy::webhook::current_unix_timestamp(),
        data: serde_json::json!({
            "session_id": session_id,
            "recording_path": recording_path,
            "format": format,
            "duration_secs": 0,
            "size_bytes": size_bytes,
        }),
    }
}

async fn record_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RecordRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let registry = state.sip_server().inner.active_call_registry.clone();
    // 404 pre-check (D-08).
    let _ = require_session(&registry, &id)?;

    let format = validate_recording_format(req.format.as_deref())?;
    let recorder_path = state.config().recorder_path();
    let resolved =
        resolve_recording_path(req.path.as_deref(), &id, &format, &recorder_path)?;

    let webhook_sender = state.webhook_sender();
    maybe_drop_transcribe_marker(
        &resolved,
        req.transcribe.unwrap_or(false),
        &id,
        Some(&webhook_sender),
    );

    // Phase 7 D-06: emit recording.completed after dispatch returns success.
    let recording_event =
        build_recording_completed_event(&id, &resolved, &format).await;
    let _ = webhook_sender.send(recording_event);

    let payload = CallCommandPayload::Record {
        path: Some(resolved.clone()),
        format: Some(format),
        beep: req.beep,
        max_duration_secs: req.max_duration_secs,
        transcribe: req.transcribe,
    };
    let extra = Some(serde_json::json!({ "path": resolved }));
    map_command_result(dispatch_console_command(&registry, &id, payload), extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_filter_accepts_mixed_case() {
        assert!(matches!(
            parse_status_filter("Ringing"),
            Ok(ActiveProxyCallStatus::Ringing)
        ));
        assert!(matches!(
            parse_status_filter("TALKING"),
            Ok(ActiveProxyCallStatus::Talking)
        ));
    }

    #[test]
    fn parse_status_filter_rejects_garbage() {
        assert!(parse_status_filter("busy").is_err());
    }

    #[test]
    fn parse_direction_filter_accepts_mixed_case() {
        assert_eq!(parse_direction_filter("Inbound").unwrap(), "inbound");
        assert_eq!(parse_direction_filter("OUTBOUND").unwrap(), "outbound");
        assert_eq!(parse_direction_filter("Internal").unwrap(), "internal");
        assert!(parse_direction_filter("sideways").is_err());
    }

    #[test]
    fn parse_since_rfc3339_happy() {
        let dt = parse_since("2026-04-19T12:00:00Z").unwrap();
        // 2026-04-19T12:00:00Z == 1776600000 (Unix epoch seconds)
        assert_eq!(dt.timestamp(), 1776600000);
    }

    #[test]
    fn parse_since_rejects_garbage() {
        assert!(parse_since("not-a-date").is_err());
    }

    #[test]
    fn substring_matches_ci_works() {
        let h = Some("+14155551234".to_string());
        assert!(substring_matches_ci(&h, "415"));
        assert!(substring_matches_ci(&h, "1234"));
        assert!(!substring_matches_ci(&h, "999"));
        assert!(!substring_matches_ci(&None, "anything"));
    }

    // ── Phase 4 Plan 04-02 — validate_leg + map_command_result ────────────

    #[test]
    fn validate_leg_accepts_mixed_case() {
        assert_eq!(validate_leg("caller").unwrap(), Leg::Caller);
        assert_eq!(validate_leg("CALLER").unwrap(), Leg::Caller);
        assert_eq!(validate_leg("Callee").unwrap(), Leg::Callee);
        assert_eq!(validate_leg("callee").unwrap(), Leg::Callee);
    }

    #[test]
    fn validate_leg_rejects_garbage() {
        assert!(validate_leg("both").is_err());
        assert!(validate_leg("").is_err());
        assert!(validate_leg("CALLER ").is_err()); // trailing space
    }

    #[test]
    fn map_command_result_success_returns_dispatched() {
        let res = map_command_result(Ok(CommandResult::success()), None).unwrap();
        assert_eq!(res.0["message"], "dispatched");
    }

    #[test]
    fn map_command_result_not_found_returns_404() {
        let err = map_command_result(
            Ok(CommandResult::failure("session abc not found")),
            None,
        )
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn map_command_result_dispatch_failure_returns_409() {
        let err = map_command_result(
            Ok(CommandResult::failure("failed to dispatch: channel closed")),
            None,
        )
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        assert!(err.message.contains("command dispatch failed"));
    }

    #[test]
    fn map_command_result_merges_extra_fields() {
        let extra = json!({"consult_leg_id": "leg-consult-42"});
        let res = map_command_result(Ok(CommandResult::success()), Some(extra)).unwrap();
        assert_eq!(res.0["message"], "dispatched");
        assert_eq!(res.0["consult_leg_id"], "leg-consult-42");
    }

    // ── Phase 4 Plan 04-03 — parse_target (D-22) ──────────────────────────

    #[test]
    fn parse_target_sip_uri_passes_through() {
        let out = parse_target("sip:1001@example.com", None).unwrap();
        assert_eq!(out, "sip:1001@example.com");
    }

    #[test]
    fn parse_target_sips_uri_passes_through() {
        let out = parse_target("sips:alice@secure.example.com", None).unwrap();
        assert_eq!(out, "sips:alice@secure.example.com");
    }

    #[test]
    fn parse_target_e164_with_external_ip() {
        let out = parse_target("+14155551234", Some("1.2.3.4")).unwrap();
        assert_eq!(out, "sip:+14155551234@1.2.3.4");
    }

    #[test]
    fn parse_target_e164_without_external_ip_uses_localhost() {
        let out = parse_target("+14155551234", None).unwrap();
        assert_eq!(out, "sip:+14155551234@127.0.0.1");
    }

    #[test]
    fn parse_target_rejects_plain_number() {
        assert!(parse_target("4155551234", None).is_err());
    }

    #[test]
    fn parse_target_rejects_empty() {
        assert!(parse_target("", None).is_err());
        assert!(parse_target("   ", None).is_err());
    }

    #[test]
    fn parse_target_rejects_garbage() {
        assert!(parse_target("not-a-uri-or-e164", None).is_err());
    }

    // ── Phase 4 Plan 04-04 — validate_dtmf_digits / default_leg_from_direction

    #[test]
    fn validate_dtmf_digits_accepts_full_charset() {
        assert!(validate_dtmf_digits("0123456789").is_ok());
        assert!(validate_dtmf_digits("ABCD*#").is_ok());
        assert!(validate_dtmf_digits("abcd").is_ok());
        assert!(validate_dtmf_digits("1*2#3A4B").is_ok());
    }

    #[test]
    fn validate_dtmf_digits_rejects_empty() {
        assert!(validate_dtmf_digits("").is_err());
    }

    #[test]
    fn validate_dtmf_digits_rejects_out_of_charset() {
        // 'e' is NOT in the DTMF set (only A-D are valid).
        let err = validate_dtmf_digits("12e").unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("invalid dtmf"));

        // 'E' is ALSO out of charset.
        assert!(validate_dtmf_digits("E").is_err());

        // Whitespace / control chars rejected.
        assert!(validate_dtmf_digits("1 2").is_err());
        assert!(validate_dtmf_digits("1\n2").is_err());
    }

    #[test]
    fn default_leg_from_direction_maps_correctly() {
        assert_eq!(default_leg_from_direction("inbound"), Leg::Caller);
        assert_eq!(default_leg_from_direction("INBOUND"), Leg::Caller);
        assert_eq!(default_leg_from_direction("outbound"), Leg::Callee);
        // Unknown direction falls back to callee (safest default for
        // dispatch-initiated DTMF — operator is presumed to be driving
        // the outbound leg unless told otherwise).
        assert_eq!(default_leg_from_direction("unknown"), Leg::Callee);
        assert_eq!(default_leg_from_direction(""), Leg::Callee);
    }

    // ── Phase 4 Plan 04-05 — validate_recording_format / resolve_recording_path

    #[test]
    fn validate_recording_format_defaults_to_wav() {
        assert_eq!(validate_recording_format(None).unwrap(), "wav");
    }

    #[test]
    fn validate_recording_format_accepts_wav_and_mp3() {
        assert_eq!(validate_recording_format(Some("wav")).unwrap(), "wav");
        assert_eq!(validate_recording_format(Some("WAV")).unwrap(), "wav");
        assert_eq!(validate_recording_format(Some("mp3")).unwrap(), "mp3");
        assert_eq!(validate_recording_format(Some("MP3")).unwrap(), "mp3");
    }

    #[test]
    fn validate_recording_format_rejects_unknown() {
        let err = validate_recording_format(Some("flac")).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("format"));
        assert!(validate_recording_format(Some("ogg")).is_err());
        assert!(validate_recording_format(Some("")).is_err());
    }

    #[test]
    fn resolve_recording_path_auto_generates_with_session_and_format() {
        let path =
            resolve_recording_path(None, "sess-abc", "wav", "/var/recordings").unwrap();
        assert!(
            path.starts_with("/var/recordings/"),
            "auto path must start with recorder root, got: {}",
            path
        );
        assert!(
            path.contains("sess-abc"),
            "auto path must contain session_id, got: {}",
            path
        );
        assert!(
            path.ends_with(".wav"),
            "auto path must end with .wav, got: {}",
            path
        );
    }

    #[test]
    fn resolve_recording_path_rejects_traversal() {
        let err = resolve_recording_path(
            Some("/var/recordings/../../etc/passwd"),
            "sess",
            "wav",
            "/var/recordings",
        )
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains(".."));
    }

    #[test]
    fn resolve_recording_path_rejects_relative() {
        let err = resolve_recording_path(
            Some("relative/path.wav"),
            "sess",
            "wav",
            "/var/recordings",
        )
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.to_ascii_lowercase().contains("absolute"));
    }

    #[test]
    fn resolve_recording_path_rejects_outside_tree() {
        let err = resolve_recording_path(
            Some("/etc/passwd.wav"),
            "sess",
            "wav",
            "/var/recordings",
        )
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains("recorder tree"),
            "expected 'recorder tree' in error, got: {}",
            err.message
        );
    }

    #[test]
    fn resolve_recording_path_accepts_in_tree_explicit() {
        let result = resolve_recording_path(
            Some("/var/recordings/custom.wav"),
            "sess",
            "wav",
            "/var/recordings",
        )
        .unwrap();
        assert_eq!(result, "/var/recordings/custom.wav");
    }

    #[tokio::test]
    async fn build_recording_completed_event_uses_d07_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.wav");
        std::fs::write(&path, b"hello").unwrap();
        let ev = build_recording_completed_event(
            "sess-r",
            path.to_str().unwrap(),
            "wav",
        )
        .await;
        assert_eq!(ev.event, "recording.completed");
        assert_eq!(ev.data["session_id"], "sess-r");
        assert_eq!(ev.data["format"], "wav");
        assert_eq!(ev.data["size_bytes"], 5);
    }
}
