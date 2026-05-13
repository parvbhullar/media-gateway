//! Built-in `http_json` signaling adapter — provider-agnostic.
//!
//! See plan: `/home/anuj/.claude/plans/imperative-sauteeing-cake.md` (Phase 5).
//!
//! Drives a templated HTTP POST of the SDP offer to a configured endpoint
//! and extracts the SDP answer via configurable JSONPath-like expressions.
//! Vendor specifics live entirely in per-trunk config (`kind_config.protocol`)
//! — this file contains no vendor names.
//!
//! ## Per-trunk protocol shape
//!
//! ```json
//! {
//!   "request_body_template": "{\"sdp\":\"{offer_sdp}\",\"type\":\"offer\"}",
//!   "response_answer_path":  "$.sdp",
//!   "response_session_path": "$.pc_id",
//!   "extra_headers": [["X-Tenant","abc"]]
//! }
//! ```

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    NegotiateOutcome, SessionHandle, SignalingContext, SignalingError, WebRtcSignalingAdapter,
};

/// Configuration for an `http_json` trunk — deserialized from the trunk's
/// `kind_config.protocol` blob.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HttpJsonProtocol {
    /// JSON body template; must contain the literal `{offer_sdp}` placeholder.
    /// At negotiate time the placeholder is replaced with the JSON-escaped
    /// SDP string contents (without surrounding quotes), then the whole
    /// document is parsed as JSON and POSTed.
    pub request_body_template: String,
    /// Simple JSONPath-style expression into the response yielding the SDP
    /// answer string. Supported syntax: dot path starting with `$` —
    /// e.g. `"$.sdp"`, `"$.result.answer"`. Array indexing not required for
    /// v1; can be added by upgrading the extractor.
    pub response_answer_path: String,
    /// Optional JSONPath into the response yielding an opaque session
    /// identifier. Same syntax as `response_answer_path`.
    #[serde(default)]
    pub response_session_path: Option<String>,
    /// Optional extra headers sent with the request, as `[name, value]`
    /// pairs.
    #[serde(default)]
    pub extra_headers: Option<Vec<(String, String)>>,
}

impl HttpJsonProtocol {
    /// Validate the protocol blob at CRUD/load time, before any negotiate
    /// call. See [`HttpJsonAdapter::validate_protocol`].
    pub fn validate(&self) -> Result<(), SignalingError> {
        if !self.request_body_template.contains("{offer_sdp}") {
            return Err(SignalingError::InvalidProtocol(
                "request_body_template must contain `{offer_sdp}` placeholder".to_string(),
            ));
        }
        // Substitute the placeholder with a harmless JSON string and ensure
        // the result parses as valid JSON. This catches malformed templates
        // (missing quotes around the placeholder, stray commas, etc.) at
        // CRUD time rather than at call time.
        let probe = self.request_body_template.replace("{offer_sdp}", "_");
        if serde_json::from_str::<Value>(&probe).is_err() {
            return Err(SignalingError::InvalidProtocol(
                "request_body_template must be valid JSON after substitution".to_string(),
            ));
        }
        if self.response_answer_path.trim().is_empty() {
            return Err(SignalingError::InvalidProtocol(
                "response_answer_path must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

/// Built-in `http_json` adapter. Stateless beyond a reusable `reqwest`
/// client — one instance services many trunks.
pub struct HttpJsonAdapter {
    client: reqwest::Client,
}

impl HttpJsonAdapter {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for HttpJsonAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Substitute `{offer_sdp}` in `template` with the JSON-string-escaped
/// contents of `offer_sdp` (without the surrounding quotes), so the
/// resulting document parses as valid JSON whose string value at the
/// placeholder location is exactly `offer_sdp`.
fn substitute_offer_sdp(template: &str, offer_sdp: &str) -> Result<Value, SignalingError> {
    // `serde_json::to_string(&offer_sdp)` produces `"v=0\r\n..."` — the
    // outer double-quotes wrap the escaped contents. Strip them so the
    // template author can write `"{offer_sdp}"` (the quotes come from the
    // template) and we splice in just the escaped body.
    let quoted = serde_json::to_string(offer_sdp).map_err(|e| {
        SignalingError::InvalidProtocol(format!("failed to JSON-escape offer SDP: {e}"))
    })?;
    debug_assert!(quoted.starts_with('"') && quoted.ends_with('"'));
    let escaped = &quoted[1..quoted.len() - 1];
    let substituted = template.replace("{offer_sdp}", escaped);
    serde_json::from_str::<Value>(&substituted).map_err(|e| {
        SignalingError::InvalidProtocol(format!(
            "request_body_template did not yield valid JSON after substitution: {e}"
        ))
    })
}

/// Minimal JSONPath-style extractor supporting `$`, `$.a`, `$.a.b.c`.
///
/// Returns `None` if the path doesn't resolve. Bracket and array index
/// syntax are deliberately unsupported for v1 — keep the dependency
/// footprint zero. Add a real JSONPath crate if/when a real configuration
/// needs the extra expressiveness.
fn jsonpath_get<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "$" {
        return Some(root);
    }
    let rest = trimmed.strip_prefix("$.")?;
    let mut cur = root;
    for segment in rest.split('.') {
        if segment.is_empty() {
            return None;
        }
        cur = cur.get(segment)?;
    }
    Some(cur)
}

#[async_trait]
impl WebRtcSignalingAdapter for HttpJsonAdapter {
    fn validate_protocol(&self, protocol: Option<&Value>) -> Result<(), SignalingError> {
        let value = protocol.ok_or_else(|| SignalingError::MissingProtocol("http_json".into()))?;
        let cfg: HttpJsonProtocol = serde_json::from_value(value.clone()).map_err(|e| {
            SignalingError::InvalidProtocol(format!(
                "failed to deserialize http_json protocol: {e}"
            ))
        })?;
        cfg.validate()
    }

    async fn negotiate(
        &self,
        ctx: &SignalingContext,
        offer_sdp: &str,
    ) -> Result<NegotiateOutcome, SignalingError> {
        let proto_value = ctx
            .protocol
            .as_ref()
            .ok_or_else(|| SignalingError::MissingProtocol("http_json".into()))?;
        let proto: HttpJsonProtocol =
            serde_json::from_value(proto_value.clone()).map_err(|e| {
                SignalingError::InvalidProtocol(format!(
                    "failed to deserialize http_json protocol: {e}"
                ))
            })?;

        // 1. Build the JSON request body with `{offer_sdp}` substituted in.
        let body = substitute_offer_sdp(&proto.request_body_template, offer_sdp)?;

        // 2. POST the body to the configured endpoint.
        let mut req = self
            .client
            .post(&ctx.endpoint_url)
            .timeout(Duration::from_millis(ctx.timeout_ms.max(1)))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some(h) = &ctx.auth_header {
            req = req.header(reqwest::header::AUTHORIZATION, h);
        }
        if let Some(extras) = &proto.extra_headers {
            for (k, v) in extras {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| SignalingError::Transport(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(SignalingError::Transport(format!(
                "non-success HTTP status {}",
                status.as_u16()
            )));
        }
        let response_json: Value = resp.json().await.map_err(|e| {
            SignalingError::InvalidResponse(format!("response was not JSON: {e}"))
        })?;

        // 3. Extract answer SDP + optional session handle.
        let answer_sdp = jsonpath_get(&response_json, &proto.response_answer_path)
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SignalingError::InvalidResponse(format!(
                    "answer SDP not found at `{}` in response (or not a string)",
                    proto.response_answer_path
                ))
            })?
            .to_string();

        let session_value = proto
            .response_session_path
            .as_ref()
            .and_then(|p| jsonpath_get(&response_json, p).cloned())
            .unwrap_or(Value::Null);

        Ok(NegotiateOutcome {
            answer_sdp,
            session: SessionHandle(session_value),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn ok_proto() -> Value {
        json!({
            "request_body_template": r#"{"sdp":"{offer_sdp}","type":"offer"}"#,
            "response_answer_path": "$.sdp",
        })
    }

    #[test]
    fn validate_protocol_requires_protocol_value() {
        let adapter = HttpJsonAdapter::new();
        let err = adapter.validate_protocol(None).unwrap_err();
        matches!(err, SignalingError::MissingProtocol(_));
    }

    #[test]
    fn validate_protocol_requires_offer_sdp_placeholder() {
        let adapter = HttpJsonAdapter::new();
        let bad = json!({
            "request_body_template": r#"{"sdp":"placeholder-missing","type":"offer"}"#,
            "response_answer_path": "$.sdp",
        });
        let err = adapter.validate_protocol(Some(&bad)).unwrap_err();
        match err {
            SignalingError::InvalidProtocol(msg) => assert!(msg.contains("offer_sdp")),
            other => panic!("expected InvalidProtocol, got {other:?}"),
        }
    }

    #[test]
    fn validate_protocol_requires_valid_json_template() {
        let adapter = HttpJsonAdapter::new();
        // Missing the closing brace → invalid JSON after substitution.
        let bad = json!({
            "request_body_template": r#"{"sdp":"{offer_sdp}""#,
            "response_answer_path": "$.sdp",
        });
        let err = adapter.validate_protocol(Some(&bad)).unwrap_err();
        match err {
            SignalingError::InvalidProtocol(msg) => assert!(msg.contains("valid JSON")),
            other => panic!("expected InvalidProtocol, got {other:?}"),
        }
    }

    #[test]
    fn validate_protocol_requires_response_answer_path() {
        let adapter = HttpJsonAdapter::new();
        let bad = json!({
            "request_body_template": r#"{"sdp":"{offer_sdp}"}"#,
            "response_answer_path": "",
        });
        let err = adapter.validate_protocol(Some(&bad)).unwrap_err();
        match err {
            SignalingError::InvalidProtocol(msg) => {
                assert!(msg.contains("response_answer_path"))
            }
            other => panic!("expected InvalidProtocol, got {other:?}"),
        }
    }

    #[test]
    fn validate_protocol_accepts_good_config() {
        let adapter = HttpJsonAdapter::new();
        adapter.validate_protocol(Some(&ok_proto())).unwrap();
    }

    #[test]
    fn substitute_preserves_sdp_with_crlf_and_quotes() {
        let template = r#"{"sdp":"{offer_sdp}","type":"offer"}"#;
        let offer = "v=0\r\no=- 1 1 IN IP4 \"127.0.0.1\"\r\n";
        let body = substitute_offer_sdp(template, offer).unwrap();
        assert_eq!(body["sdp"].as_str().unwrap(), offer);
        assert_eq!(body["type"].as_str().unwrap(), "offer");
    }

    #[test]
    fn jsonpath_get_basic() {
        let v = json!({"sdp": "ans", "outer": {"inner": "x"}});
        assert_eq!(jsonpath_get(&v, "$.sdp").unwrap(), "ans");
        assert_eq!(jsonpath_get(&v, "$.outer.inner").unwrap(), "x");
        assert!(jsonpath_get(&v, "$.missing").is_none());
        assert!(jsonpath_get(&v, "").is_none());
        assert_eq!(jsonpath_get(&v, "$").unwrap(), &v);
    }

    /// Minimal in-process HTTP recorder/responder for adapter tests.
    /// Returns `(url, captured-requests, server-handle)`.
    async fn spawn_mock_server(
        response: serde_json::Value,
        status: u16,
    ) -> (
        String,
        Arc<Mutex<Vec<(String, Vec<(String, String)>, serde_json::Value)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{
            Router,
            extract::{Json, State},
            http::{HeaderMap, StatusCode},
            response::IntoResponse,
            routing::post,
        };

        type Captured = Arc<Mutex<Vec<(String, Vec<(String, String)>, serde_json::Value)>>>;

        #[derive(Clone)]
        struct AppState {
            captured: Captured,
            response: serde_json::Value,
            status: u16,
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
            s.captured.lock().await.push(("/offer".to_string(), hdrs, body));
            (
                StatusCode::from_u16(s.status).unwrap_or(StatusCode::OK),
                Json(s.response.clone()),
            )
        }

        let captured: Captured = Arc::new(Mutex::new(Vec::new()));
        let state = AppState {
            captured: captured.clone(),
            response,
            status,
        };
        let app = Router::new().route("/offer", post(handle)).with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/offer"), captured, handle)
    }

    #[tokio::test]
    async fn negotiate_substitutes_offer_sdp_and_extracts_answer() {
        let response = json!({"sdp": "v=0\r\nanswer\r\n", "pc_id": "abc-123"});
        let (url, captured, _h) = spawn_mock_server(response, 200).await;

        let adapter = HttpJsonAdapter::new();
        let ctx = SignalingContext {
            endpoint_url: url,
            auth_header: None,
            timeout_ms: 2_000,
            protocol: Some(ok_proto()),
        };
        let offer = "v=0\r\noffer-body\r\n";
        let out = adapter.negotiate(&ctx, offer).await.unwrap();

        assert_eq!(out.answer_sdp, "v=0\r\nanswer\r\n");
        let cap = captured.lock().await;
        assert_eq!(cap.len(), 1);
        assert_eq!(cap[0].2["sdp"].as_str().unwrap(), offer);
        assert_eq!(cap[0].2["type"].as_str().unwrap(), "offer");
    }

    #[tokio::test]
    async fn negotiate_extracts_session_when_path_set() {
        let response = json!({"sdp": "ans-sdp", "pc_id": "session-xyz"});
        let (url, _captured, _h) = spawn_mock_server(response, 200).await;

        let mut proto = ok_proto();
        proto["response_session_path"] = json!("$.pc_id");

        let adapter = HttpJsonAdapter::new();
        let ctx = SignalingContext {
            endpoint_url: url,
            auth_header: None,
            timeout_ms: 2_000,
            protocol: Some(proto),
        };
        let out = adapter.negotiate(&ctx, "v=0\r\n").await.unwrap();
        assert_eq!(out.answer_sdp, "ans-sdp");
        assert_eq!(out.session.0, json!("session-xyz"));
    }

    #[tokio::test]
    async fn negotiate_propagates_auth_header_and_extras() {
        let response = json!({"sdp": "ans"});
        let (url, captured, _h) = spawn_mock_server(response, 200).await;

        let mut proto = ok_proto();
        proto["extra_headers"] = json!([["X-Tenant", "acme"]]);

        let adapter = HttpJsonAdapter::new();
        let ctx = SignalingContext {
            endpoint_url: url,
            auth_header: Some("Bearer tok".into()),
            timeout_ms: 2_000,
            protocol: Some(proto),
        };
        adapter.negotiate(&ctx, "v=0\r\n").await.unwrap();

        let cap = captured.lock().await;
        let headers = &cap[0].1;
        let find = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        assert_eq!(find("authorization").as_deref(), Some("Bearer tok"));
        assert_eq!(find("x-tenant").as_deref(), Some("acme"));
    }

    #[tokio::test]
    async fn negotiate_returns_transport_error_on_non_2xx() {
        let (url, _captured, _h) = spawn_mock_server(json!({"err": "nope"}), 500).await;
        let adapter = HttpJsonAdapter::new();
        let ctx = SignalingContext {
            endpoint_url: url,
            auth_header: None,
            timeout_ms: 2_000,
            protocol: Some(ok_proto()),
        };
        let err = adapter.negotiate(&ctx, "v=0\r\n").await.unwrap_err();
        match err {
            SignalingError::Transport(msg) => assert!(msg.contains("500")),
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn negotiate_returns_invalid_response_when_answer_path_missing() {
        let (url, _c, _h) = spawn_mock_server(json!({"different": "shape"}), 200).await;
        let adapter = HttpJsonAdapter::new();
        let ctx = SignalingContext {
            endpoint_url: url,
            auth_header: None,
            timeout_ms: 2_000,
            protocol: Some(ok_proto()),
        };
        let err = adapter.negotiate(&ctx, "v=0\r\n").await.unwrap_err();
        match err {
            SignalingError::InvalidResponse(msg) => assert!(msg.contains("answer SDP not found")),
            other => panic!("expected InvalidResponse, got {other:?}"),
        }
    }
}
