//! Integration test for PR 4 — routing matcher wire-up of `kind="webrtc"`
//! trunks into the bridge dispatcher.
//!
//! The dispatcher itself is covered by unit tests in
//! `src/proxy/bridge/webrtc.rs` and `src/proxy/bridge/signaling/http_json.rs`.
//! This integration test exercises the wire-up between the trunk row in the
//! DB and the bridge dispatcher via the
//! `crate::proxy::webrtc_route_dispatch::dispatch_webrtc_by_name` helper —
//! which is what the routing matcher's `Forward` arm calls into when it
//! detects `kind="webrtc"`.
//!
//! Scope (per plan §Phase 11 integration test):
//! - DB insert of a webrtc trunk row
//! - The dispatch helper looks up the trunk by name
//! - The configured signaling adapter is invoked
//! - The mock HTTP signaling endpoint receives the SIP-side offer SDP
//!
//! Out of scope: end-to-end SIP listener driving the matcher. The matcher's
//! Forward-arm branch is type-enforced by the new `RouteResult::WebRtcBridge`
//! variant (every match site had to add an arm); driving real SIP INVITEs
//! through it requires Dialplan/SipSession wiring that is deferred to a
//! follow-up PR (see TODO(pr-4-followup) in `src/proxy/call.rs`).

use std::sync::Arc;

use axum::{
    Router,
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use chrono::Utc;
use rustpbx::models::sip_trunk::{self, SipTrunkDirection, SipTrunkStatus};
use rustpbx::proxy::bridge::signaling;
use rustpbx::proxy::webrtc_route_dispatch::dispatch_webrtc_by_name;
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{Value, json};
use tokio::sync::Mutex;

mod common;
use common::test_state_empty;

/// Minimal but parseable SDP offer for the SIP side. PCMU on 8000Hz.
const PCMU_OFFER_SDP: &str = "v=0\r\n\
o=- 123456 123456 IN IP4 127.0.0.1\r\n\
s=-\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 0 101\r\n\
a=rtpmap:0 PCMU/8000\r\n\
a=rtpmap:101 telephone-event/8000\r\n\
a=sendrecv\r\n";

type Captured = Arc<Mutex<Vec<(Vec<(String, String)>, Value)>>>;

#[derive(Clone)]
struct MockState {
    captured: Captured,
    response: Value,
    status: u16,
}

async fn handle(
    State(s): State<MockState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let hdrs: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    s.captured.lock().await.push((hdrs, body));
    (
        StatusCode::from_u16(s.status).unwrap_or(StatusCode::OK),
        Json(s.response.clone()),
    )
}

/// Spawn an in-process axum HTTP server that mimics a Pipecat-style
/// `/api/offer` signaling endpoint. Returns `(url, captured-requests)`.
async fn spawn_mock_signaling(response: Value) -> (String, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        captured: captured.clone(),
        response,
        status: 200,
    };
    let app = Router::new()
        .route("/api/offer", post(handle))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/api/offer"), captured)
}

#[tokio::test]
async fn dispatch_webrtc_by_name_hits_signaling_adapter() {
    // Built-in adapters must be registered for the dispatcher to find
    // `http_json`. Idempotent — safe to call from every test.
    signaling::register_builtins();

    // Pipecat-shaped canned answer (SDP body + session id). The SDP need
    // not be media-realistic — the test asserts on the mock-server side
    // that the offer reached it, not on what rustrtc does with the answer.
    let answer_body = "v=0\r\n\
o=- 654321 654321 IN IP4 127.0.0.1\r\n\
s=-\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=audio 5000 RTP/AVP 0\r\n\
a=rtpmap:0 PCMU/8000\r\n\
a=sendrecv\r\n";
    let (endpoint_url, captured) =
        spawn_mock_signaling(json!({"sdp": answer_body, "pc_id": "session-xyz"})).await;

    let state = test_state_empty().await;

    let kind_config = json!({
        "signaling": "http_json",
        "endpoint_url": endpoint_url,
        "audio_codec": "opus",
        "protocol": {
            "request_body_template": r#"{"sdp":"{offer_sdp}","type":"offer"}"#,
            "response_answer_path": "$.sdp",
            "response_session_path": "$.pc_id",
        }
    });

    let now = Utc::now();
    let am = sip_trunk::ActiveModel {
        name: Set("pipecat_bot".to_string()),
        kind: Set("webrtc".into()),
        display_name: Set(Some("Pipecat Bot".into())),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        is_active: Set(true),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        kind_config: Set(kind_config),
        ..Default::default()
    };
    let _ = am.insert(state.db()).await.expect("insert webrtc trunk");

    // Drive the dispatcher. Depending on rustrtc's tolerance of our canned
    // answer SDP, the overall call may succeed or fail at later stages
    // (WebRTC PC's set_remote_description, bridge setup). The wire-up
    // assertion is on the mock-server side — we assert the adapter was
    // invoked with our offer, regardless.
    let _ = dispatch_webrtc_by_name(state.db(), "pipecat_bot", PCMU_OFFER_SDP, None).await;

    let cap = captured.lock().await;
    assert_eq!(
        cap.len(),
        1,
        "expected exactly one POST to the mock signaling endpoint"
    );
    let (_headers, body) = &cap[0];
    let sdp_on_wire = body
        .get("sdp")
        .and_then(|v| v.as_str())
        .expect("mock should have received an `sdp` field");
    // The dispatcher offers an RTC-side SDP (from the outbound WebRTC PC's
    // create_offer), not the SIP-side offer verbatim. We only assert it's
    // a non-empty SDP fragment so the test is robust to rustrtc's exact
    // SDP shape.
    assert!(
        sdp_on_wire.starts_with("v=0"),
        "expected an SDP offer body, got: {sdp_on_wire}"
    );
    let body_type = body.get("type").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(body_type, "offer", "request body `type` must be 'offer'");
}

#[tokio::test]
async fn dispatch_webrtc_by_name_rejects_missing_trunk() {
    signaling::register_builtins();
    let state = test_state_empty().await;
    let result = dispatch_webrtc_by_name(state.db(), "no-such-trunk", PCMU_OFFER_SDP, None).await;
    let err = match result {
        Ok(_) => panic!("expected lookup failure for unknown trunk"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("not found") && msg.contains("no-such-trunk"),
        "error should name the missing trunk, got: {msg}"
    );
}

#[tokio::test]
async fn dispatch_webrtc_by_name_rejects_sip_kind() {
    signaling::register_builtins();
    let state = test_state_empty().await;

    let sip_cfg = json!({
        "sip_server": "sip:example.com:5060",
        "sip_transport": "udp",
        "register_enabled": false,
        "rewrite_hostport": true,
    });
    let now = Utc::now();
    let am = sip_trunk::ActiveModel {
        name: Set("regular_sip".to_string()),
        kind: Set("sip".into()),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        is_active: Set(true),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        kind_config: Set(sip_cfg),
        ..Default::default()
    };
    let _ = am.insert(state.db()).await.expect("insert sip trunk");

    let result = dispatch_webrtc_by_name(state.db(), "regular_sip", PCMU_OFFER_SDP, None).await;
    let err = match result {
        Ok(_) => panic!("expected kind-mismatch rejection"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("kind 'sip'") || msg.contains("expected 'webrtc'"),
        "error should call out the kind mismatch, got: {msg}"
    );
}

#[tokio::test]
async fn dispatch_webrtc_by_name_rejects_disabled_trunk() {
    signaling::register_builtins();
    let state = test_state_empty().await;

    let kind_config = json!({
        "signaling": "http_json",
        "endpoint_url": "http://127.0.0.1:1/api/offer",
        "audio_codec": "opus",
        "protocol": {
            "request_body_template": r#"{"sdp":"{offer_sdp}","type":"offer"}"#,
            "response_answer_path": "$.sdp",
        }
    });
    let now = Utc::now();
    let am = sip_trunk::ActiveModel {
        name: Set("disabled_bot".to_string()),
        kind: Set("webrtc".into()),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        is_active: Set(false),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        kind_config: Set(kind_config),
        ..Default::default()
    };
    let _ = am.insert(state.db()).await.expect("insert disabled trunk");

    let result = dispatch_webrtc_by_name(state.db(), "disabled_bot", PCMU_OFFER_SDP, None).await;
    let err = match result {
        Ok(_) => panic!("expected disabled-trunk rejection"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("disabled"),
        "error should call out the disabled state, got: {msg}"
    );
}
