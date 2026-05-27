//! Optional dispatch_endpoint webhook — fire before LiveKit room connect
//! so a controller can decide accept/reject + supply overrides.
//!
//! Response shape is the Decision API. See spec:
//! `docs/superpowers/specs/2026-05-25-livekit-trunk-design.md`.

use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::decision::{Decision, WebhookDecisionRaw};

pub struct WebhookInput<'a> {
    pub endpoint_url: &'a str,
    pub auth_header: Option<&'a str>,
    pub protocol: Option<&'a Value>,
    pub vars: &'a std::collections::HashMap<&'a str, &'a str>,
    pub timeout_ms: u64,
    pub require_ack: bool,
}

/// POST the dispatch webhook and return the validated Decision.
///
/// Behaviour:
/// - 2xx + valid JSON + valid schema → return `Ok(Decision::Accept{...}|Decision::Reject{...})`.
/// - Schema validation error (e.g. `decision: "bogus"`, oversize override) → ALWAYS `Err`,
///   regardless of `require_ack`. Schema bugs are controller errors, not transient.
/// - Non-2xx, timeout, body-not-JSON:
///     - require_ack=true  → `Err`.
///     - require_ack=false → log warn + return `Ok(Decision::Accept{no overrides})`.
pub async fn post(client: &reqwest::Client, input: WebhookInput<'_>) -> Result<Decision> {
    // Build request body — either from protocol.request_body_template
    // (if configured) or a default shape.
    let body: Value = match input.protocol {
        Some(p) => {
            let tmpl = p
                .get("request_body_template")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("webhook protocol missing request_body_template"))?;
            // JSON-safe render: vars inside the body template are
            // substituted into JSON string positions, so we escape
            // quotes / backslashes / control chars before splicing.
            // Without this, a SIP user-part containing a quote
            // (`+91"hacker"`) produces invalid JSON and the webhook
            // would silently never receive the call.
            let rendered = super::template::render_for_json_string(tmpl, input.vars)
                .map_err(|e| anyhow!("webhook template: {e}"))?;
            serde_json::from_str(&rendered).map_err(|e| {
                anyhow!("webhook body not valid JSON after substitution: {e}")
            })?
        }
        None => serde_json::json!({
            "call_id":    input.vars.get("call_id").copied().unwrap_or(""),
            "from_user":  input.vars.get("from_user").copied().unwrap_or(""),
            "to_user":    input.vars.get("to_user").copied().unwrap_or(""),
            "did":        input.vars.get("did").copied().unwrap_or(""),
            "trunk_name": input.vars.get("trunk_name").copied().unwrap_or(""),
            "room":       input.vars.get("room").copied().unwrap_or(""),
            "identity":   input.vars.get("identity").copied().unwrap_or(""),
        }),
    };

    let fallback = |reason: String| -> Result<Decision> {
        if input.require_ack {
            Err(anyhow!("webhook ack required but {reason}"))
        } else {
            tracing::warn!(
                endpoint = %input.endpoint_url,
                "{reason}; falling back to accept-with-no-overrides"
            );
            Ok(Decision::Accept {
                room: None,
                identity: None,
                metadata: None,
                wait_ms: 0,
            })
        }
    };

    let mut req = client
        .post(input.endpoint_url)
        .timeout(Duration::from_millis(input.timeout_ms.max(1)))
        .json(&body);
    if let Some(h) = input.auth_header {
        req = req.header(reqwest::header::AUTHORIZATION, h);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return fallback(format!("webhook POST failed: {e}")),
    };
    if !resp.status().is_success() {
        return fallback(format!("webhook non-success status {}", resp.status()));
    }
    // Try to parse JSON body. Empty body or non-JSON → fallback.
    let raw: WebhookDecisionRaw = match resp.json().await {
        Ok(v) => v,
        Err(e) => return fallback(format!("webhook response not JSON: {e}")),
    };
    // Schema-validation errors always abort (controller bug, not transient).
    raw.validate().map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post as axum_post,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    type Captured = Arc<Mutex<Vec<(Vec<(String, String)>, serde_json::Value)>>>;

    #[derive(Clone)]
    struct AppState {
        captured: Captured,
        response: serde_json::Value,
        status: u16,
        delay_ms: u64,
    }

    async fn handle(
        State(s): State<AppState>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> impl IntoResponse {
        let hdrs: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        s.captured.lock().await.push((hdrs, body));
        if s.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(s.delay_ms)).await;
        }
        (
            StatusCode::from_u16(s.status).unwrap_or(StatusCode::OK),
            Json(s.response.clone()),
        )
    }

    async fn spawn_mock(
        response: serde_json::Value,
        status: u16,
        delay_ms: u64,
    ) -> (String, Captured) {
        let captured: Captured = Arc::new(Mutex::new(Vec::new()));
        let state = AppState {
            captured: captured.clone(),
            response,
            status,
            delay_ms,
        };
        let app = Router::new().route("/dispatch", axum_post(handle)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/dispatch"), captured)
    }

    fn default_vars() -> HashMap<&'static str, &'static str> {
        let mut v = HashMap::new();
        v.insert("call_id", "test-call");
        v.insert("from_user", "alice");
        v.insert("to_user", "bob");
        v.insert("did", "12345");
        v.insert("trunk_name", "lk_trunk");
        v.insert("room", "default-room");
        v.insert("identity", "default-identity");
        v
    }

    #[tokio::test]
    async fn webhook_returns_accept_with_no_body_fields() {
        let (url, _cap) = spawn_mock(serde_json::json!({"decision": "accept"}), 200, 0).await;
        let client = reqwest::Client::new();
        let vars = default_vars();
        let d = post(&client, WebhookInput {
            endpoint_url: &url,
            auth_header: None,
            protocol: None,
            vars: &vars,
            timeout_ms: 2_000,
            require_ack: false,
        }).await.unwrap();
        match d {
            Decision::Accept { room, identity, metadata, wait_ms } => {
                assert!(room.is_none() && identity.is_none() && metadata.is_none());
                assert_eq!(wait_ms, 0);
            }
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn webhook_returns_accept_with_overrides() {
        let (url, _cap) = spawn_mock(serde_json::json!({
            "decision": "accept",
            "room": "agent-pool-7",
            "identity": "caller-xyz",
            "metadata": {"tenant": "acme"},
            "wait_ms": 500,
        }), 200, 0).await;
        let client = reqwest::Client::new();
        let vars = default_vars();
        let d = post(&client, WebhookInput {
            endpoint_url: &url,
            auth_header: None,
            protocol: None,
            vars: &vars,
            timeout_ms: 2_000,
            require_ack: false,
        }).await.unwrap();
        match d {
            Decision::Accept { room, identity, metadata, wait_ms } => {
                assert_eq!(room.as_deref(), Some("agent-pool-7"));
                assert_eq!(identity.as_deref(), Some("caller-xyz"));
                assert!(metadata.unwrap().contains("acme"));
                assert_eq!(wait_ms, 500);
            }
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn webhook_returns_reject() {
        let (url, _cap) = spawn_mock(serde_json::json!({
            "decision": "reject",
            "reject_code": 503,
            "reject_reason": "off-hours",
        }), 200, 0).await;
        let client = reqwest::Client::new();
        let vars = default_vars();
        let d = post(&client, WebhookInput {
            endpoint_url: &url,
            auth_header: None,
            protocol: None,
            vars: &vars,
            timeout_ms: 2_000,
            require_ack: false,
        }).await.unwrap();
        match d {
            Decision::Reject { code, reason } => {
                assert_eq!(code, 503);
                assert_eq!(reason.as_deref(), Some("off-hours"));
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn webhook_500_with_require_ack_true_errors() {
        let (url, _cap) = spawn_mock(serde_json::json!({}), 500, 0).await;
        let client = reqwest::Client::new();
        let vars = default_vars();
        let err = post(&client, WebhookInput {
            endpoint_url: &url,
            auth_header: None,
            protocol: None,
            vars: &vars,
            timeout_ms: 2_000,
            require_ack: true,
        }).await.unwrap_err();
        assert!(err.to_string().contains("500") || err.to_string().contains("status"));
    }

    #[tokio::test]
    async fn webhook_500_with_require_ack_false_falls_back_to_accept() {
        let (url, _cap) = spawn_mock(serde_json::json!({}), 500, 0).await;
        let client = reqwest::Client::new();
        let vars = default_vars();
        let d = post(&client, WebhookInput {
            endpoint_url: &url,
            auth_header: None,
            protocol: None,
            vars: &vars,
            timeout_ms: 2_000,
            require_ack: false,
        }).await.unwrap();
        match d {
            Decision::Accept { room, identity, metadata, wait_ms } => {
                assert!(room.is_none() && identity.is_none() && metadata.is_none());
                assert_eq!(wait_ms, 0);
            }
            other => panic!("expected fallback Accept, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn webhook_invalid_decision_field_always_errors() {
        // schema-validation error → abort even when require_ack=false.
        let (url, _cap) = spawn_mock(serde_json::json!({"decision": "maybe"}), 200, 0).await;
        let client = reqwest::Client::new();
        let vars = default_vars();
        let err = post(&client, WebhookInput {
            endpoint_url: &url,
            auth_header: None,
            protocol: None,
            vars: &vars,
            timeout_ms: 2_000,
            require_ack: false,
        }).await.unwrap_err();
        assert!(err.to_string().contains("invalid decision"));
    }

    #[tokio::test]
    async fn webhook_too_long_room_always_errors() {
        let long = "x".repeat(300);
        let (url, _cap) = spawn_mock(serde_json::json!({
            "decision": "accept", "room": long,
        }), 200, 0).await;
        let client = reqwest::Client::new();
        let vars = default_vars();
        let err = post(&client, WebhookInput {
            endpoint_url: &url,
            auth_header: None,
            protocol: None,
            vars: &vars,
            timeout_ms: 2_000,
            require_ack: false,
        }).await.unwrap_err();
        assert!(err.to_string().contains("room"));
    }

    #[tokio::test]
    async fn webhook_propagates_auth_header() {
        let (url, captured) = spawn_mock(serde_json::json!({"decision": "accept"}), 200, 0).await;
        let client = reqwest::Client::new();
        let vars = default_vars();
        let _ = post(&client, WebhookInput {
            endpoint_url: &url,
            auth_header: Some("Bearer xyz"),
            protocol: None,
            vars: &vars,
            timeout_ms: 2_000,
            require_ack: false,
        }).await.unwrap();
        let cap = captured.lock().await;
        assert_eq!(cap.len(), 1);
        let (headers, body) = &cap[0];
        // Authorization header propagated.
        let auth = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.clone());
        assert_eq!(auth.as_deref(), Some("Bearer xyz"));
        // body shape sanity-check
        assert_eq!(body["call_id"], "test-call");
        assert_eq!(body["from_user"], "alice");
        assert_eq!(body["did"], "12345");
        assert_eq!(body["room"], "default-room");
    }
}
