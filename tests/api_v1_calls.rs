//! `/api/v1/calls` integration tests — Phase 4 Plan 04-01 (CALL-01, CALL-02).

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use common::{test_state_empty, test_state_with_api_key};
use rustpbx::call::domain::{MediaPathMode, SessionState};
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
