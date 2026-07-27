use crate::console::{ConsoleState, middleware::AuthRequired};
use crate::proxy::active_call_registry::ActiveProxyCallRegistry;
use crate::proxy::proxy_call::sip_session::SessionSnapshot;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

const DEFAULT_ACTIVE_CALL_LIMIT: usize = 50;
const MAX_ACTIVE_CALL_LIMIT: usize = 500;

pub fn urls() -> Router<Arc<ConsoleState>> {
    Router::new()
        .route("/calls/active", get(list_active_calls))
        .route("/calls/active/{session_id}", get(show_active_call))
        .route(
            "/calls/active/{session_id}/commands",
            post(dispatch_call_command),
        )
}

#[derive(Default, Deserialize)]
pub struct ActiveCallListQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// Which leg of a call a command targets (caller = A-leg, callee = B-leg).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Leg {
    Caller,
    Callee,
}

/// Audio source for play commands.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PlaySource {
    File { path: String },
    Url { url: String },
    Tts { text: String, voice: Option<String> },
}

/// Optional playback options for play commands.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiPlayOptions {
    #[serde(default)]
    pub loop_playback: bool,
    #[serde(default = "default_true")]
    pub interrupt_on_dtmf: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CallCommandPayload {
    Hangup {
        reason: Option<String>,
        code: Option<u16>,
        initiator: Option<String>,
    },
    #[serde(alias = "accept")]
    Accept {
        callee: Option<String>,
        sdp: Option<String>,
    },
    Transfer {
        target: String,
    },
    Mute {
        track_id: String,
    },
    Unmute {
        track_id: String,
    },
    // ── Extended API variants (used by /api/v1/calls handlers) ──────────
    ApiHangup {
        reason: Option<String>,
        code: Option<u16>,
    },
    ApiMute {
        leg: Leg,
    },
    ApiUnmute {
        leg: Leg,
    },
    BlindTransfer {
        target: String,
        leg: Option<Leg>,
    },
    AttendedTransferStart {
        target: String,
        leg: Option<Leg>,
    },
    AttendedTransferComplete {
        consult_leg: String,
    },
    AttendedTransferCancel {
        consult_leg: String,
    },
    Play {
        source: PlaySource,
        leg: Option<Leg>,
        options: Option<ApiPlayOptions>,
    },
    Dtmf {
        digits: String,
        duration_ms: Option<u32>,
        inter_digit_ms: Option<u32>,
        leg: Option<Leg>,
    },
    Record {
        path: Option<String>,
        format: Option<String>,
        beep: Option<bool>,
        max_duration_secs: Option<u32>,
        transcribe: Option<bool>,
    },
}

pub async fn list_active_calls(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_): AuthRequired,
    Query(query): Query<ActiveCallListQuery>,
) -> Response {
    let Some(server) = state.sip_server() else {
        return service_unavailable();
    };

    let limit = query
        .limit
        .unwrap_or(DEFAULT_ACTIVE_CALL_LIMIT)
        .clamp(1, MAX_ACTIVE_CALL_LIMIT);
    let registry = server.active_call_registry.clone();
    let entries = registry.list_recent(limit);
    let payload: Vec<_> = entries
        .into_iter()
        .map(|entry| {
            let session_id = entry.session_id.clone();
            json!({
                "meta": entry,
                "state": snapshot_for(&registry, &session_id),
            })
        })
        .collect();

    Json(json!({ "data": payload })).into_response()
}

pub async fn show_active_call(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_): AuthRequired,
    AxumPath(session_id): AxumPath<String>,
) -> Response {
    let Some(server) = state.sip_server() else {
        return service_unavailable();
    };
    let registry = server.active_call_registry.clone();

    let Some(handle) = registry.get_handle(&session_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Call not found" })),
        )
            .into_response();
    };

    Json(json!({ "data": json!({
        "meta": registry.get(&session_id),
        "state": handle.snapshot(),
    }) }))
    .into_response()
}

pub async fn dispatch_call_command(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_): AuthRequired,
    AxumPath(session_id): AxumPath<String>,
    Json(payload): Json<CallCommandPayload>,
) -> Response {
    let Some(server) = state.sip_server() else {
        return service_unavailable();
    };
    let registry = server.active_call_registry.clone();

    // Verify session exists
    let Some(_handle) = registry.get_handle(&session_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Call not found" })),
        )
            .into_response();
    };

    // Use unified dispatch path
    use crate::call::runtime::dispatch_console_command;

    match dispatch_console_command(&registry, &session_id, payload) {
        Ok(result) => {
            if result.success {
                Json(json!({ "message": "Command dispatched" })).into_response()
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "message": result.message })),
                )
                    .into_response()
            }
        }
        Err(e) => (
            StatusCode::CONFLICT,
            Json(json!({ "message": format!("Failed to deliver command: {}", e) })),
        )
            .into_response(),
    }
}

fn snapshot_for(
    registry: &Arc<ActiveProxyCallRegistry>,
    session_id: &str,
) -> Option<SessionSnapshot> {
    registry
        .get_handle(session_id)
        .and_then(|handle| handle.snapshot())
}

fn service_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "message": "SIP server unavailable" })),
    )
        .into_response()
}
