//! Integration tests for the LiveKit trunk dispatcher (Phase 7).
//!
//! Strategy B (per the plan): we don't try to mock LiveKit's signaling
//! protobuf — `Room::connect` will simply fail when pointed at a
//! non-existent ws:// URL. Instead, we exercise the dispatch-level
//! contract that runs BEFORE `Room::connect`:
//!
//! - The optional dispatch webhook (Decision API) is POSTed with the
//!   expected body shape.
//! - On `decision: "reject"`, `dispatch_livekit` returns an error whose
//!   anyhow chain holds a `DispatchRejection` — call.rs uses this to map
//!   the SIP failure code, so the test pins that contract.
//! - With `require_webhook_ack=true` + webhook 500, dispatch aborts
//!   BEFORE any LiveKit connect attempt.
//! - With `require_webhook_ack=false` + webhook 500, dispatch falls
//!   through past the webhook (the eventual error is the LiveKit
//!   connect failure, not a webhook-ack failure).
//! - `fetch_external_trunk(.., BridgeKind::LiveKit)` rejects missing,
//!   disabled, and wrong-kind rows.
//!
//! Real-LiveKit-server flows (audio plumbing, room teardown) are covered
//! by the manual smoke script — see `docs/smoke_test_livekit_bridge.md`.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use chrono::Utc;
use rustpbx::models::sip_trunk::{self, SipTrunkDirection, SipTrunkStatus};
use rustpbx::proxy::bridge::common::{DispatchContext, fetch_external_trunk};
use rustpbx::proxy::bridge::livekit::dispatch::{DispatchRejection, dispatch_livekit};
use rustpbx::proxy::bridge::session::BridgeKind;
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{Value, json};
use tokio::sync::Mutex;

mod common;
use common::test_state_empty;

/// Minimal but parseable SDP offer for the SIP side. PCMU on 8000Hz.
/// Same shape used by `tests/webrtc_trunk_bridge_test.rs` — sufficient
/// for `build_inbound_rtp_pc` to negotiate without erroring out before
/// the webhook step we want to exercise.
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

/// Spawn an in-process axum mock for the LiveKit dispatch webhook.
/// Returns `(url, captured-requests)`.
async fn spawn_mock_webhook(response: Value, status: u16) -> (String, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        captured: captured.clone(),
        response,
        status,
    };
    let app = Router::new()
        .route("/dispatch", post(handle))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/dispatch"), captured)
}

/// Build a `livekit` kind_config JSON blob with optional dispatch_endpoint
/// + require_webhook_ack. Uses an unreachable `ws://127.0.0.1:1` server
/// URL — connect will fail, which is fine: every test here asserts on
/// behaviour BEFORE the LiveKit connect (or on the webhook-rejection
/// short-circuit that returns even earlier).
fn livekit_kind_config(
    dispatch_endpoint: Option<&str>,
    require_webhook_ack: bool,
) -> Value {
    let mut cfg = json!({
        "server_url": "ws://127.0.0.1:1",
        "api_key": "test_api_key",
        "api_secret": "this_secret_is_long_enough_for_hmac_sha256",
        "room_template": "room-{did}",
        "identity_template": "caller-{from_user}",
        "audio_codec": "opus",
        "signaling_timeout_ms": 2000,
        "require_webhook_ack": require_webhook_ack,
    });
    if let Some(url) = dispatch_endpoint {
        cfg["dispatch_endpoint"] = Value::String(url.to_string());
    }
    cfg
}

/// Insert a livekit-kind trunk row and return its name.
async fn insert_livekit_trunk(
    state: &rustpbx::app::AppState,
    name: &str,
    kind_config: Value,
    is_active: bool,
) {
    let now = Utc::now();
    let am = sip_trunk::ActiveModel {
        name: Set(name.to_string()),
        kind: Set("livekit".into()),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        is_active: Set(is_active),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        kind_config: Set(kind_config),
        ..Default::default()
    };
    let _ = am.insert(state.db()).await.expect("insert livekit trunk");
}

fn ctx_for(call_id: &str, from_user: &str, to_user: &str) -> DispatchContext {
    DispatchContext {
        call_id: call_id.to_string(),
        from_user: from_user.to_string(),
        to_user: to_user.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Webhook firing + body shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn livekit_dispatch_fires_webhook_with_templated_room_and_identity() {
    // Mock webhook accepts the call with no overrides; dispatch will then
    // proceed to LiveKit connect, which fails against ws://127.0.0.1:1 —
    // but the webhook fires BEFORE that, so the captured body proves the
    // template-substitution path and the webhook POST contract.
    let (endpoint, captured) =
        spawn_mock_webhook(json!({"decision": "accept"}), 200).await;
    let state = test_state_empty().await;
    insert_livekit_trunk(
        &state,
        "lk_template_trunk",
        livekit_kind_config(Some(&endpoint), false),
        true,
    )
    .await;

    let row = fetch_external_trunk(state.db(), "lk_template_trunk", BridgeKind::LiveKit)
        .await
        .expect("trunk lookup");

    // Will fail at Room::connect against ws://127.0.0.1:1; ignore.
    let _ = dispatch_livekit(
        &row,
        PCMU_OFFER_SDP,
        None,
        &ctx_for("call-xyz", "alice", "12345"),
    )
    .await;

    let cap = captured.lock().await;
    assert_eq!(cap.len(), 1, "webhook should fire exactly once");
    let (_headers, body) = &cap[0];
    assert_eq!(body["call_id"], "call-xyz");
    assert_eq!(body["from_user"], "alice");
    assert_eq!(body["to_user"], "12345");
    assert_eq!(body["did"], "12345");
    assert_eq!(body["trunk_name"], "lk_template_trunk");
    // Templates rendered with the DispatchContext.
    assert_eq!(body["room"], "room-12345");
    assert_eq!(body["identity"], "caller-alice");
}

// ---------------------------------------------------------------------------
// Reject path → DispatchRejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn livekit_dispatch_returns_dispatch_rejection_when_webhook_says_reject() {
    let (endpoint, _captured) = spawn_mock_webhook(
        json!({"decision": "reject", "reject_code": 403, "reject_reason": "blocked"}),
        200,
    )
    .await;
    let state = test_state_empty().await;
    insert_livekit_trunk(
        &state,
        "lk_reject_trunk",
        livekit_kind_config(Some(&endpoint), false),
        true,
    )
    .await;
    let row = fetch_external_trunk(state.db(), "lk_reject_trunk", BridgeKind::LiveKit)
        .await
        .unwrap();

    let err = match dispatch_livekit(
        &row,
        PCMU_OFFER_SDP,
        None,
        &ctx_for("call-rej", "bob", "9999"),
    )
    .await
    {
        Ok(_) => panic!("reject decision must propagate as an error"),
        Err(e) => e,
    };

    // call.rs downcasts the anyhow chain to map the SIP failure code —
    // pin that contract here.
    let rej = err
        .downcast_ref::<DispatchRejection>()
        .expect("err should hold a DispatchRejection");
    assert_eq!(rej.code, 403);
    assert_eq!(rej.reason.as_deref(), Some("blocked"));
}

// ---------------------------------------------------------------------------
// require_webhook_ack=true + 500 → abort before LiveKit connect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn livekit_dispatch_aborts_when_require_webhook_ack_and_endpoint_500s() {
    let (endpoint, _cap) = spawn_mock_webhook(json!({}), 500).await;
    let state = test_state_empty().await;
    insert_livekit_trunk(
        &state,
        "lk_ack_required",
        livekit_kind_config(Some(&endpoint), true),
        true,
    )
    .await;
    let row = fetch_external_trunk(state.db(), "lk_ack_required", BridgeKind::LiveKit)
        .await
        .unwrap();

    let err = match dispatch_livekit(
        &row,
        PCMU_OFFER_SDP,
        None,
        &ctx_for("call-ack", "alice", "1234"),
    )
    .await
    {
        Ok(_) => panic!("require_webhook_ack=true + 500 must abort"),
        Err(e) => e,
    };

    let msg = err.to_string();
    // Webhook-ack failure surfaces the upstream status / "webhook ack
    // required but ..." string; the LiveKit connect string would mention
    // ws:// or "connect" — assert we got the ack-required path.
    assert!(
        msg.contains("webhook") || msg.contains("status") || msg.contains("500"),
        "expected webhook-ack failure, got: {msg}"
    );
    // And the err is NOT a DispatchRejection — that's reserved for the
    // explicit `decision: reject` reply, not transport failures.
    assert!(
        err.downcast_ref::<DispatchRejection>().is_none(),
        "transport failure must not masquerade as DispatchRejection"
    );
}

// ---------------------------------------------------------------------------
// require_webhook_ack=false + 500 → fall through past webhook
// ---------------------------------------------------------------------------

#[tokio::test]
async fn livekit_dispatch_falls_through_when_require_webhook_ack_false_and_endpoint_500s() {
    let (endpoint, captured) = spawn_mock_webhook(json!({}), 500).await;
    let state = test_state_empty().await;
    insert_livekit_trunk(
        &state,
        "lk_ack_optional",
        livekit_kind_config(Some(&endpoint), false),
        true,
    )
    .await;
    let row = fetch_external_trunk(state.db(), "lk_ack_optional", BridgeKind::LiveKit)
        .await
        .unwrap();

    let err = match dispatch_livekit(
        &row,
        PCMU_OFFER_SDP,
        None,
        &ctx_for("call-opt", "alice", "1234"),
    )
    .await
    {
        Ok(_) => panic!("LiveKit connect to ws://127.0.0.1:1 will fail"),
        Err(e) => e,
    };

    // Webhook was hit (proves we got past the template step) ...
    assert_eq!(
        captured.lock().await.len(),
        1,
        "webhook should have been POSTed even though we fell through"
    );
    // ... and the resulting error is NOT a webhook-ack error nor a
    // DispatchRejection — it's the downstream LiveKit connect failure.
    let msg = err.to_string();
    assert!(
        !msg.contains("webhook ack required"),
        "fall-through path must not surface as a webhook-ack failure, got: {msg}"
    );
    assert!(
        err.downcast_ref::<DispatchRejection>().is_none(),
        "fall-through must not be a DispatchRejection"
    );
}

// ---------------------------------------------------------------------------
// fetch_external_trunk kind validator — exercised through the livekit arm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_external_trunk_rejects_missing_trunk_for_livekit() {
    let state = test_state_empty().await;
    let res =
        fetch_external_trunk(state.db(), "no-such-livekit-trunk", BridgeKind::LiveKit).await;
    let err = res.expect_err("missing trunk must error");
    let msg = err.to_string();
    assert!(
        msg.contains("not found") && msg.contains("no-such-livekit-trunk"),
        "error should name the missing trunk, got: {msg}"
    );
}

#[tokio::test]
async fn fetch_external_trunk_rejects_disabled_trunk_for_livekit() {
    let state = test_state_empty().await;
    insert_livekit_trunk(
        &state,
        "lk_disabled",
        livekit_kind_config(None, false),
        false, // is_active=false
    )
    .await;
    let err = fetch_external_trunk(state.db(), "lk_disabled", BridgeKind::LiveKit)
        .await
        .expect_err("disabled trunk must error");
    let msg = err.to_string();
    assert!(
        msg.contains("disabled"),
        "error should call out the disabled state, got: {msg}"
    );
}

#[tokio::test]
async fn fetch_external_trunk_rejects_wrong_kind_for_livekit() {
    // Insert a sip-kind trunk; ask for it as livekit; expect a kind-mismatch
    // error rather than a silent type punning.
    let state = test_state_empty().await;
    let sip_cfg = json!({
        "sip_server": "sip:example.com:5060",
        "sip_transport": "udp",
        "register_enabled": false,
        "rewrite_hostport": true,
    });
    let now = Utc::now();
    let am = sip_trunk::ActiveModel {
        name: Set("legacy_sip_for_livekit_check".to_string()),
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

    let err = fetch_external_trunk(
        state.db(),
        "legacy_sip_for_livekit_check",
        BridgeKind::LiveKit,
    )
    .await
    .expect_err("kind mismatch must error");
    let msg = err.to_string();
    assert!(
        msg.contains("kind 'sip'") && msg.contains("expected 'livekit'"),
        "error should call out the kind mismatch, got: {msg}"
    );
}
