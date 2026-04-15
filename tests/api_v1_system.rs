//! Integration tests for `/api/v1/system/*` (Phase 1, Plan 01-05).

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

fn bearer(token: &str) -> String {
    format!("Bearer {}", token)
}

// ---------------------------------------------------------------------------
// GET /system/health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_happy_path_shape() {
    let (state, token) = test_state_with_api_key("sys-health").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/health")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert!(body["uptime_secs"].is_number());
    assert!(body["uptime_secs"].as_u64().unwrap() < 3600);
    assert_eq!(body["db_ok"], true);
    assert_eq!(body["active_calls"], 0);
    assert!(body["version"].is_string());
    assert!(!body["version"].as_str().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// POST /system/reload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reload_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/system/reload")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reload_happy_path_shape() {
    // Plan 01-06 changed the shape: reload now performs real work across
    // 3 steps (trunks, routes, acl). The "app" step is deferred to Phase 11.
    let (state, token) = test_state_with_api_key("sys-reload").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/system/reload")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let reloaded = body["reloaded"].as_array().expect("reloaded is array");
    assert_eq!(reloaded.len(), 3);
    let names: Vec<&str> = reloaded.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(names, vec!["trunks", "routes", "acl"]);

    assert!(body["elapsed_ms"].is_number());
    assert!(body["steps"].is_array());
    assert_eq!(body["steps"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn reload_populates_per_step_outcomes() {
    // SYS-02 gap closure: reload must invoke real per-step work and
    // report structured outcomes. The step helpers themselves spend real
    // time (file IO + lock acquisition) so elapsed_ms is observably > 0
    // for at least one step even on an empty fixture.
    let (state, token) = test_state_with_api_key("sys-reload-outcomes").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/system/reload")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let steps = body["steps"].as_array().expect("steps is array");
    assert_eq!(steps.len(), 3, "expected 3 reload steps, got {}", steps.len());
    assert_eq!(steps[0]["step"], "trunks");
    assert_eq!(steps[1]["step"], "routes");
    assert_eq!(steps[2]["step"], "acl");

    // Each step must carry elapsed_ms and changed_count fields. Real work
    // signal: the total elapsed_ms across the three steps must be > 0 on
    // any reasonable machine (file IO + async context-switching dominates
    // the empty-fixture case).
    let mut total_elapsed: u64 = 0;
    for step in steps {
        assert!(step["elapsed_ms"].is_number(), "step missing elapsed_ms: {step}");
        assert!(step["changed_count"].is_number(), "step missing changed_count: {step}");
        total_elapsed += step["elapsed_ms"].as_u64().unwrap_or(0);
    }
    assert!(
        total_elapsed > 0 || body["elapsed_ms"].as_u64().unwrap_or(0) > 0,
        "expected some real work, got total_elapsed={}",
        total_elapsed
    );

    // Legacy field must still be populated for backward compat.
    let reloaded: Vec<&str> = body["reloaded"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(reloaded, vec!["trunks", "routes", "acl"]);
}

#[tokio::test]
async fn concurrent_reload_cas_conflict_returns_409() {
    // SYS-02 gap closure: the CAS serialization must be observable, not
    // just present in code. Spawn two concurrent reload requests via
    // tokio::spawn (stronger parallelism than plain tokio::join! which
    // can run both futures on the same poller). Exactly one should win
    // the CAS (200) and the other lose it (409).
    let (state, token) = test_state_with_api_key("sys-reload-race").await;

    let token_a = token.clone();
    let token_b = token.clone();
    let state_a = state.clone();
    let state_b = state.clone();

    let call_reload = |state: rustpbx::app::AppState, token: String| async move {
        let app = rustpbx::app::create_router(state);
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/system/reload")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    };

    let h_a = tokio::spawn(call_reload(state_a, token_a));
    let h_b = tokio::spawn(call_reload(state_b, token_b));

    let resp_a = h_a.await.expect("task a panicked");
    let resp_b = h_b.await.expect("task b panicked");

    let mut statuses = [resp_a.status(), resp_b.status()];
    statuses.sort_by_key(|s| s.as_u16());

    assert_eq!(
        statuses[0],
        StatusCode::OK,
        "expected one 200 OK, got {:?}",
        statuses
    );
    assert_eq!(
        statuses[1],
        StatusCode::CONFLICT,
        "expected one 409 Conflict, got {:?}",
        statuses
    );
}

#[tokio::test]
async fn reload_twice_sequentially_both_succeed() {
    // Two SEQUENTIAL reload calls should both return 200 because the
    // ReloadGuard releases the flag on drop.
    let (state, token) = test_state_with_api_key("sys-reload-twice").await;

    let app1 = rustpbx::app::create_router(state.clone());
    let resp1 = app1
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/system/reload")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/system/reload")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
}
