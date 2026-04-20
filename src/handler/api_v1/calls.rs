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

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::common::{PaginatedResponse, Pagination};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::proxy::active_call_registry::{ActiveProxyCallEntry, ActiveProxyCallStatus};
use crate::proxy::proxy_call::sip_session::SessionSnapshot;

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

// ── Router ───────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/calls", get(list_active_calls))
        .route("/calls/{id}", get(get_active_call))
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
        _ => Err(ApiError::bad_request(format!(
            "invalid direction filter '{}' (expected 'inbound' or 'outbound')",
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
}
