//! `/api/v1/listeners` — read-only projection over `ProxyConfig`
//! transport ports (Phase 12, LSTN-01..04). Writes return 501.
//!
//! Per Phase 12 D-01..D-06:
//!   - Always returns 4 entries (udp/tcp/tls/ws) regardless of config.
//!   - Disabled ports surface as `enabled: false, port: 0`.
//!   - POST/PUT/DELETE return 501 with the D-05 locked message.
//!   - `external_ip` field is intentionally OMITTED — ProxyConfig has
//!     no such field; RtpConfig.external_ip is unrelated to SIP
//!     transport bind addresses. Planner-resolved.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use serde::Serialize;

use crate::app::AppState;
use crate::config::ProxyConfig;
use crate::handler::api_v1::error::{ApiError, ApiResult};

/// Phase 12 D-05 locked message — wording is part of the API contract.
const MULTI_LISTENER_MSG: &str = "Multi-listener configuration is \
    intentionally unsupported in v2.0. Edit ProxyConfig and POST \
    /api/v1/system/reload to change transports.";

/// Phase 12 D-01: fixed protocol set, fixed order.
const PROTOCOLS: &[&str] = &["udp", "tcp", "tls", "ws"];

#[derive(Debug, Serialize)]
pub struct Listener {
    pub name: String,
    pub protocol: String,
    pub bind_addr: String,
    pub port: u16,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct ListenersResponse {
    pub items: Vec<Listener>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/listeners",
            get(handle_list)
                .post(handle_write_501)
                .put(handle_write_501)
                .delete(handle_write_501),
        )
        .route(
            "/listeners/{name}",
            get(handle_get)
                .post(handle_write_501)
                .put(handle_write_501)
                .delete(handle_write_501),
        )
}

/// LSTN-01: read-only projection of all 4 transport rows.
async fn handle_list(
    State(state): State<AppState>,
) -> ApiResult<Json<ListenersResponse>> {
    let items = build_listeners(&state.config().proxy);
    Ok(Json(ListenersResponse { items }))
}

/// LSTN-02: single transport by lowercase protocol name.
async fn handle_get(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Listener>> {
    let lookup = name.to_lowercase();
    let items = build_listeners(&state.config().proxy);
    items
        .into_iter()
        .find(|l| l.name == lookup)
        .map(Json)
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "listener {name} not found (valid: udp, tcp, tls, ws)"
            ))
        })
}

/// LSTN-03: writes return 501 with the locked D-05 message.
async fn handle_write_501() -> ApiResult<StatusCode> {
    Err(ApiError::not_implemented(MULTI_LISTENER_MSG))
}

/// Pure projection — testable without an AppState. Always emits one
/// entry per protocol in `PROTOCOLS` order. A `None` port renders as
/// `port=0, enabled=false` so disabled state is unambiguous.
pub(crate) fn build_listeners(proxy: &ProxyConfig) -> Vec<Listener> {
    PROTOCOLS
        .iter()
        .map(|p| {
            let port_opt = match *p {
                "udp" => proxy.udp_port,
                "tcp" => proxy.tcp_port,
                "tls" => proxy.tls_port,
                "ws" => proxy.ws_port,
                _ => None,
            };
            let (port, enabled) = match port_opt {
                Some(p) => (p, true),
                None => (0u16, false),
            };
            Listener {
                name: (*p).to_string(),
                protocol: (*p).to_string(),
                bind_addr: proxy.addr.clone(),
                port,
                enabled,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy_with(
        udp: Option<u16>,
        tcp: Option<u16>,
        tls: Option<u16>,
        ws: Option<u16>,
    ) -> ProxyConfig {
        // Default-construct via serde_json so we don't have to track
        // every field of ProxyConfig in tests.
        let mut v = serde_json::json!({
            "addr": "0.0.0.0",
            "udp_port": udp,
            "tcp_port": tcp,
            "tls_port": tls,
            "ws_port": ws,
            "user_backends": [],
        });
        // Strip nulls so serde_default kicks in for required fields.
        if let serde_json::Value::Object(ref mut m) = v {
            m.retain(|_, val| !val.is_null());
        }
        serde_json::from_value(v).expect("ProxyConfig from minimal json")
    }

    #[test]
    fn build_listeners_emits_four_entries_in_fixed_order() {
        let p = proxy_with(Some(5060), Some(5060), Some(5061), Some(5062));
        let out = build_listeners(&p);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].protocol, "udp");
        assert_eq!(out[1].protocol, "tcp");
        assert_eq!(out[2].protocol, "tls");
        assert_eq!(out[3].protocol, "ws");
    }

    #[test]
    fn disabled_port_marks_enabled_false_and_port_zero() {
        let p = proxy_with(Some(5060), None, None, None);
        let out = build_listeners(&p);
        assert!(out[0].enabled);
        assert_eq!(out[0].port, 5060);
        assert!(!out[1].enabled);
        assert_eq!(out[1].port, 0);
        assert!(!out[2].enabled);
        assert!(!out[3].enabled);
    }

    #[test]
    fn bind_addr_copied_from_proxy_addr() {
        let p = proxy_with(Some(5060), None, None, None);
        let out = build_listeners(&p);
        assert_eq!(out[0].bind_addr, "0.0.0.0");
    }
}
