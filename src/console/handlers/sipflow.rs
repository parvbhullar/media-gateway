use crate::console::{ConsoleState, middleware::AuthRequired};
use crate::models::call_record::{Column as CallRecordColumn, Entity as CallRecordEntity};
use crate::models::system_config;
use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, TimeZone};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct FlowQueryParams {
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Serialize)]
struct SipFlowSettingsResponse {
    enabled: bool,
    backend_type: String,
    config: serde_json::Value,
}

pub fn urls() -> Router<Arc<ConsoleState>> {
    Router::new()
        .route("/sipflow/settings", get(get_settings).put(update_settings))
        .route("/sipflow/flow/{call_id}", get(query_flow))
        .route("/sipflow/media/{call_id}", get(query_media))
}

async fn get_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_user): AuthRequired,
) -> Response {
    let app_state = match state.app_state() {
        Some(app) => app,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "Application not initialized"
                })),
            )
                .into_response();
        }
    };

    let config = app_state.config();

    let (enabled, backend_type, config_json) = match &config.sipflow {
        None => (false, "none".to_string(), json!({})),
        Some(sipflow_config) => {
            use crate::config::SipFlowConfig;
            let (backend_type, config_data) = match sipflow_config {
                SipFlowConfig::Local {
                    root,
                    subdirs,
                    flush_count,
                    flush_interval_secs,
                    id_cache_size,
                    ..
                } => (
                    "local",
                    json!({
                        "root": root,
                        "subdirs": subdirs,
                        "flush_count": flush_count,
                        "flush_interval_secs": flush_interval_secs,
                        "id_cache_size": id_cache_size
                    }),
                ),
                SipFlowConfig::Remote {
                    udp_addr,
                    http_addr,
                    timeout_secs,
                } => (
                    "remote",
                    json!({
                        "udp_addr": udp_addr,
                        "http_addr": http_addr,
                        "timeout_secs": timeout_secs
                    }),
                ),
            };
            (true, backend_type.to_string(), config_data)
        }
    };

    Json(SipFlowSettingsResponse {
        enabled,
        backend_type,
        config: config_json,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    backend_type: String,
    config: serde_json::Value,
}

async fn update_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_user): AuthRequired,
    Json(payload): Json<UpdateSettingsRequest>,
) -> Response {
    // Get DB connection
    let Some(app_state) = state.app_state() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "Application state is unavailable." })),
        )
            .into_response();
    };
    let db = app_state.db().clone();

    // Build the flat (sipflow.<field>, JSON-value) rows to upsert, matching
    // the config_merge seed convention. The SipFlowConfig enum uses
    // `#[serde(tag = "type")]` so `sipflow.type` acts as the discriminator.
    let backend_type = payload.backend_type.as_str();
    let rows: Vec<(&'static str, serde_json::Value)> = match backend_type {
        "none" => Vec::new(),
        "local" => {
            let Some(root) = payload
                .config
                .get("root")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty())
            else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "sipflow local backend requires 'root'" })),
                )
                    .into_response();
            };
            let mut out: Vec<(&'static str, serde_json::Value)> = Vec::new();
            out.push(("sipflow.type", json!("local")));
            out.push(("sipflow.root", json!(root)));
            if let Some(s) = payload.config.get("subdirs").and_then(|v| v.as_str()) {
                out.push(("sipflow.subdirs", json!(s)));
            }
            if let Some(n) = payload.config.get("flush_count").and_then(|v| v.as_i64()) {
                out.push(("sipflow.flush_count", json!(n)));
            }
            if let Some(n) = payload
                .config
                .get("flush_interval_secs")
                .and_then(|v| v.as_i64())
            {
                out.push(("sipflow.flush_interval_secs", json!(n)));
            }
            if let Some(n) = payload.config.get("id_cache_size").and_then(|v| v.as_i64()) {
                out.push(("sipflow.id_cache_size", json!(n)));
            }
            out
        }
        "remote" => {
            let udp = payload
                .config
                .get("udp_addr")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty());
            let http = payload
                .config
                .get("http_addr")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty());
            let (Some(udp), Some(http)) = (udp, http) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "sipflow remote backend requires 'udp_addr' and 'http_addr'"
                    })),
                )
                    .into_response();
            };
            let mut out: Vec<(&'static str, serde_json::Value)> = Vec::new();
            out.push(("sipflow.type", json!("remote")));
            out.push(("sipflow.udp_addr", json!(udp)));
            out.push(("sipflow.http_addr", json!(http)));
            if let Some(n) = payload.config.get("timeout_secs").and_then(|v| v.as_i64()) {
                out.push(("sipflow.timeout_secs", json!(n)));
            }
            out
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!("Invalid backend type: {}", backend_type)
                })),
            )
                .into_response();
        }
    };

    // Wipe the entire `sipflow.*` key space (stale fields from previous
    // mode or base-config seeding) plus any legacy top-level `sipflow`
    // row, then upsert the new set.
    {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        if let Err(e) = system_config::Entity::delete_many()
            .filter(system_config::Column::Key.starts_with("sipflow."))
            .exec(&db)
            .await
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to clear stale sipflow.* rows: {e}") })),
            )
                .into_response();
        }
        let _ = system_config::Entity::delete_by_id("sipflow".to_string())
            .exec(&db)
            .await;
    }

    for (key, val) in rows {
        if let Err(e) = system_config::Model::upsert(&db, key, &val.to_string(), false).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to save '{key}': {e}") })),
            )
                .into_response();
        }
    }

    (
        StatusCode::OK,
        Json(json!({
            "message": "SipFlow settings updated. Please restart the server for changes to take effect.",
            "restart_required": true
        })),
    )
        .into_response()
}


async fn query_flow(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_user): AuthRequired,
    AxumPath(call_id): AxumPath<String>,
    Query(params): Query<FlowQueryParams>,
) -> Response {
    let sip_server = match state.sip_server() {
        Some(server) => server,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "SIP server not available"
                })),
            )
                .into_response();
        }
    };

    let sipflow = match &sip_server.sip_flow {
        Some(flow) => flow,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "SipFlow not enabled"
                })),
            )
                .into_response();
        }
    };

    let backend = match sipflow.backend() {
        Some(backend) => backend,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "SipFlow backend not configured"
                })),
            )
                .into_response();
        }
    };

    // Parse time range
    let now = chrono::Local::now();
    let mut start_time = params.start.and_then(|s| parse_datetime(&s));
    let mut end_time = params.end.and_then(|s| parse_datetime(&s));

    if (start_time.is_none() || end_time.is_none())
        && let Ok(Some(record)) = CallRecordEntity::find()
            .filter(CallRecordColumn::CallId.eq(&call_id))
            .one(state.db())
            .await
        {
            if start_time.is_none() {
                start_time = Some(
                    record.started_at.with_timezone(&chrono::Local) - chrono::Duration::minutes(10),
                );
            }
            if end_time.is_none() {
                end_time = Some(
                    record
                        .ended_at
                        .unwrap_or(record.started_at)
                        .with_timezone(&chrono::Local)
                        + chrono::Duration::hours(1),
                );
            }
        }

    let start_time = start_time.unwrap_or_else(|| now - chrono::Duration::hours(1));
    let end_time = end_time.unwrap_or(now);

    match backend.query_flow(&call_id, start_time, end_time).await {
        Ok(items) => {
            if items.is_empty() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "error": "Call flow not found"
                    })),
                )
                    .into_response();
            }

            let json_items: Vec<serde_json::Value> = items
                .iter()
                .map(|item| {
                    json!({
                        "seq": item.seq,
                        "timestamp": item.timestamp,
                        "msg_type": format!("{:?}", item.msg_type),
                        "src_addr": item.src_addr,
                        "dst_addr": item.dst_addr,
                        "raw_message": String::from_utf8_lossy(&item.payload),
                    })
                })
                .collect();

            Json(json!({
                "status": "success",
                "call_id": call_id,
                "start_time": start_time.to_rfc3339(),
                "end_time": end_time.to_rfc3339(),
                "flow": json_items
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("Failed to query flow: {}", e)
            })),
        )
            .into_response(),
    }
}

async fn query_media(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_user): AuthRequired,
    AxumPath(call_id): AxumPath<String>,
    Query(params): Query<FlowQueryParams>,
) -> Response {
    let sip_server = match state.sip_server() {
        Some(server) => server,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "SIP server not available"
                })),
            )
                .into_response();
        }
    };

    let sipflow = match &sip_server.sip_flow {
        Some(flow) => flow,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "SipFlow not enabled"
                })),
            )
                .into_response();
        }
    };

    let backend = match sipflow.backend() {
        Some(backend) => backend,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "SipFlow backend not configured"
                })),
            )
                .into_response();
        }
    };

    let now = chrono::Local::now();
    let mut start_time = params.start.and_then(|s| parse_datetime(&s));
    let mut end_time = params.end.and_then(|s| parse_datetime(&s));

    if (start_time.is_none() || end_time.is_none())
        && let Ok(Some(record)) = CallRecordEntity::find()
            .filter(CallRecordColumn::CallId.eq(&call_id))
            .one(state.db())
            .await
        {
            if start_time.is_none() {
                start_time = Some(
                    record.started_at.with_timezone(&chrono::Local) - chrono::Duration::minutes(10),
                );
            }
            if end_time.is_none() {
                end_time = Some(
                    record
                        .ended_at
                        .unwrap_or(record.started_at)
                        .with_timezone(&chrono::Local)
                        + chrono::Duration::hours(1),
                );
            }
        }

    let start_time = start_time.unwrap_or_else(|| now - chrono::Duration::hours(1));
    let end_time = end_time.unwrap_or(now);

    match backend.query_media(&call_id, start_time, end_time).await {
        Ok(data) => {
            if data.is_empty() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "error": "Call media not found"
                    })),
                )
                    .into_response();
            }

            use axum::http::header;

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "audio/wav")
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}.wav\"", call_id),
                )
                .body(axum::body::Body::from(data))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("Failed to query media: {}", e)
            })),
        )
            .into_response(),
    }
}

fn parse_datetime(s: &str) -> Option<DateTime<chrono::Local>> {
    // Try ISO 8601 format
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Local));
    }

    // Try Unix timestamp
    if let Ok(ts) = s.parse::<i64>()
        && let Some(dt) = chrono::Local.timestamp_opt(ts, 0).single() {
            return Some(dt);
        }

    None
}
