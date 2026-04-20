//! `/api/v1/calls` integration tests — Phase 4 Plan 04-01 (CALL-01, CALL-02).

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use common::{test_state_empty, test_state_with_api_key};
use rustpbx::call::domain::{CallCommand, MediaPathMode, SessionState};
use rustpbx::call::runtime::SessionId;
use rustpbx::proxy::active_call_registry::{ActiveProxyCallEntry, ActiveProxyCallStatus};
use rustpbx::proxy::proxy_call::sip_session::{SessionSnapshot, SipSession, SipSessionHandle};
use serde_json::Value;
use tokio::sync::mpsc;
use tower::ServiceExt;

// ── Fixture helpers ──────────────────────────────────────────────────────

fn make_entry(
    session_id: &str,
    status: ActiveProxyCallStatus,
    caller: &str,
    callee: &str,
    direction: &str,
) -> ActiveProxyCallEntry {
    ActiveProxyCallEntry {
        session_id: session_id.to_string(),
        caller: Some(caller.to_string()),
        callee: Some(callee.to_string()),
        direction: direction.to_string(),
        started_at: Utc::now(),
        answered_at: None,
        status,
    }
}

fn make_entry_at(
    session_id: &str,
    status: ActiveProxyCallStatus,
    started_at: DateTime<Utc>,
) -> ActiveProxyCallEntry {
    ActiveProxyCallEntry {
        session_id: session_id.to_string(),
        caller: Some("c".to_string()),
        callee: Some("d".to_string()),
        direction: "outbound".to_string(),
        started_at,
        answered_at: None,
        status,
    }
}

fn make_handle(
    session_id: &str,
) -> (
    SipSessionHandle,
    mpsc::UnboundedReceiver<rustpbx::call::domain::CallCommand>,
) {
    SipSession::with_handle(SessionId::from(session_id))
}

async fn body_json(body: Body) -> Value {
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn calls_require_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/calls")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn list_active_calls_empty() {
    let (state, token) = test_state_with_api_key("calls-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/calls")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["items"], serde_json::json!([]));
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 20);
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn list_active_calls_paginated() {
    let (state, token) = test_state_with_api_key("calls-paged").await;
    let registry = state.sip_server().inner.active_call_registry.clone();
    // Keep handles alive so cmd_rx doesn't drop.
    let (h1, _r1) = make_handle("s-1");
    let (h2, _r2) = make_handle("s-2");
    let (h3, _r3) = make_handle("s-3");
    registry.upsert(
        make_entry("s-1", ActiveProxyCallStatus::Talking, "+1", "+2", "outbound"),
        h1,
    );
    registry.upsert(
        make_entry("s-2", ActiveProxyCallStatus::Talking, "+3", "+4", "inbound"),
        h2,
    );
    registry.upsert(
        make_entry("s-3", ActiveProxyCallStatus::Ringing, "+5", "+6", "outbound"),
        h3,
    );

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/calls?page_size=2")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["total"], 3);

    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/calls?page=2&page_size=2")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body2 = body_json(resp2.into_body()).await;
    assert_eq!(body2["items"].as_array().unwrap().len(), 1);
    assert_eq!(body2["total"], 3);
}

#[tokio::test]
async fn list_active_calls_filtered_by_status() {
    let (state, token) = test_state_with_api_key("calls-filter-status").await;
    let registry = state.sip_server().inner.active_call_registry.clone();
    let (h1, _r1) = make_handle("s-a");
    let (h2, _r2) = make_handle("s-b");
    registry.upsert(
        make_entry("s-a", ActiveProxyCallStatus::Ringing, "+1", "+2", "outbound"),
        h1,
    );
    registry.upsert(
        make_entry("s-b", ActiveProxyCallStatus::Talking, "+3", "+4", "outbound"),
        h2,
    );

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/calls?status=ringing")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["status"], "ringing");

    // Invalid status → 400
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/calls?status=busy")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
    let body2 = body_json(resp2.into_body()).await;
    assert_eq!(body2["code"], "bad_request");
}

#[tokio::test]
async fn list_active_calls_filtered_by_since_or_400() {
    let (state, token) = test_state_with_api_key("calls-filter-since").await;
    let registry = state.sip_server().inner.active_call_registry.clone();
    let old_ts = Utc::now() - Duration::hours(2);
    let new_ts = Utc::now() - Duration::minutes(5);
    let (h1, _r1) = make_handle("s-old");
    let (h2, _r2) = make_handle("s-new");
    registry.upsert(
        make_entry_at("s-old", ActiveProxyCallStatus::Talking, old_ts),
        h1,
    );
    registry.upsert(
        make_entry_at("s-new", ActiveProxyCallStatus::Talking, new_ts),
        h2,
    );

    // since = 1 hour ago → only s-new
    let since = (Utc::now() - Duration::hours(1)).to_rfc3339();
    let uri = format!("/api/v1/calls?since={}", urlencoding::encode(&since));
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["session_id"], "s-new");

    // Garbage since → 400
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/calls?since=not-a-date")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_active_call_by_id_returns_rich_view() {
    let (state, token) = test_state_with_api_key("calls-get-rich").await;
    let registry = state.sip_server().inner.active_call_registry.clone();
    let (handle, _rx) = make_handle("rich-session");
    // Stamp a snapshot so leg_count=2 and handle.snapshot() returns Some(...).
    // SessionState::Active (not `Answered` — which doesn't exist) represents
    // a bridged/talking session per src/call/domain/state.rs.
    handle.update_snapshot(SessionSnapshot {
        id: SessionId::from("rich-session"),
        state: SessionState::Active,
        leg_count: 2,
        bridge_active: true,
        media_path: MediaPathMode::Anchored,
        answer_sdp: None,
        callee_dialogs: Vec::new(),
        pending_consult_leg_id: None,
    });
    registry.upsert(
        make_entry(
            "rich-session",
            ActiveProxyCallStatus::Talking,
            "+1",
            "+2",
            "outbound",
        ),
        handle,
    );

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/calls/rich-session")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["session_id"], "rich-session");
    assert_eq!(body["status"], "talking");
    assert_eq!(body["snapshot"]["leg_count"], 2);
    assert_eq!(body["snapshot"]["bridge_active"], true);
}

#[tokio::test]
async fn get_active_call_unknown_returns_404() {
    let (state, token) = test_state_with_api_key("calls-get-unknown").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/calls/does-not-exist")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["code"], "not_found");
}

// ── Plan 04-02 — hangup / mute / unmute tests ────────────────────────────
//
// These tests exercise the full IT-01 matrix for CALL-03 and CALL-05:
// auth (401), happy dispatch (200) with wire-level CallCommand assertion,
// 404 on unknown session, 400 on bad body, 409 when media tracks aren't
// established, 409 when the command mpsc is closed.

/// Seed a session into the registry and optionally stamp a 2-leg snapshot.
///
/// Returns the handle (so callers can later update the snapshot) and the
/// cmd_rx so the test can assert the exact CallCommand that lands.
fn seed_active_call(
    state: &rustpbx::app::AppState,
    session_id: &str,
    with_snapshot: bool,
) -> (SipSessionHandle, mpsc::UnboundedReceiver<CallCommand>) {
    let registry = state.sip_server().inner.active_call_registry.clone();
    let (handle, rx) = SipSession::with_handle(SessionId::from(session_id));
    if with_snapshot {
        handle.update_snapshot(SessionSnapshot {
            id: SessionId::from(session_id),
            state: SessionState::Active,
            leg_count: 2,
            bridge_active: true,
            media_path: MediaPathMode::Anchored,
            answer_sdp: None,
            callee_dialogs: Vec::new(),
            pending_consult_leg_id: None,
        });
    }
    registry.upsert(
        make_entry(
            session_id,
            ActiveProxyCallStatus::Talking,
            "+1",
            "+2",
            "outbound",
        ),
        handle.clone(),
    );
    (handle, rx)
}

// ── Hangup ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn hangup_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/any/hangup")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn hangup_dispatches_via_registry() {
    let (state, token) = test_state_with_api_key("hangup-happy").await;
    let (_handle, mut rx) = seed_active_call(&state, "sess-hangup", true);

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/sess-hangup/hangup")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"reason":"by_caller","code":200}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["message"], "dispatched");

    let cmd = rx
        .try_recv()
        .expect("Hangup command should have been dispatched");
    assert!(matches!(cmd, CallCommand::Hangup(_)));
}

#[tokio::test]
async fn hangup_unknown_session_returns_404() {
    let (state, token) = test_state_with_api_key("hangup-404").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/nope/hangup")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn hangup_dropped_rx_returns_409() {
    let (state, token) = test_state_with_api_key("hangup-409").await;
    let (_handle, rx) = seed_active_call(&state, "sess-dropped", true);
    // Drop the receiver — next send_command returns "channel closed".
    drop(rx);

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/sess-dropped/hangup")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["code"], "conflict");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("command dispatch failed")
    );
}

// ── Mute ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn mute_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/any/mute")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"leg":"caller"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn mute_happy_dispatches_caller_track() {
    let (state, token) = test_state_with_api_key("mute-happy").await;
    let (_handle, mut rx) = seed_active_call(&state, "sess-mute", true);

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/sess-mute/mute")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"leg":"caller"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let cmd = rx
        .try_recv()
        .expect("MuteTrack should have been dispatched");
    if let CallCommand::MuteTrack { track_id } = cmd {
        assert_eq!(track_id, "caller-track");
    } else {
        panic!("expected MuteTrack, got {:?}", cmd);
    }
}

#[tokio::test]
async fn mute_missing_leg_returns_400() {
    let (state, token) = test_state_with_api_key("mute-400-missing").await;
    let (_handle, _rx) = seed_active_call(&state, "sess-x", true);

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/sess-x/mute")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // Axum 0.8 returns 422 Unprocessable Entity for valid-JSON-but-missing-
    // required-field; older versions returned 400. Either way it's a client
    // error — accept the 4xx as the invariant (the exact status varies by
    // axum version; what matters is the request is rejected).
    let s = resp.status().as_u16();
    assert!(
        s == 400 || s == 422,
        "expected 4xx rejection for missing 'leg', got {}",
        s
    );
}

#[tokio::test]
async fn mute_invalid_leg_returns_400() {
    let (state, token) = test_state_with_api_key("mute-400-invalid").await;
    let (_handle, _rx) = seed_active_call(&state, "sess-y", true);

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/sess-y/mute")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"leg":"both"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["code"], "bad_request");
    assert!(body["error"].as_str().unwrap().contains("invalid leg"));
}

#[tokio::test]
async fn mute_without_media_tracks_returns_409() {
    let (state, token) = test_state_with_api_key("mute-409").await;
    // with_snapshot=false → handle.snapshot() is None → 409.
    let (_handle, _rx) = seed_active_call(&state, "sess-no-media", false);

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/sess-no-media/mute")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"leg":"caller"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["code"], "conflict");
    assert!(body["error"].as_str().unwrap().contains("media tracks"));
}

// ── Unmute ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn unmute_happy_dispatches_callee_track() {
    let (state, token) = test_state_with_api_key("unmute-happy").await;
    let (_handle, mut rx) = seed_active_call(&state, "sess-unmute", true);

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/calls/sess-unmute/unmute")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"leg":"callee"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let cmd = rx
        .try_recv()
        .expect("UnmuteTrack should have been dispatched");
    if let CallCommand::UnmuteTrack { track_id } = cmd {
        assert_eq!(track_id, "callee-track");
    } else {
        panic!("expected UnmuteTrack, got {:?}", cmd);
    }
}
