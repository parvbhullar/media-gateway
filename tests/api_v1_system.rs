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
    // SYS-02 gap closure: the CAS serialization must be observable in
    // tests, not just present in code.
    //
    // **Why this test is NOT a naive tokio::spawn race:**
    // The empty-fixture reload path completes in sub-millisecond time
    // on modern hardware — faster than tokio can even hand a spawned
    // task from one worker to another. A pure `tokio::spawn(A);
    // tokio::spawn(B);` race is flaky because task A can fully
    // complete `reload_all`, drop the guard, and return 200 BEFORE
    // task B is ever polled. We verified this empirically: across 5
    // runs with N=8 barrier-aligned spawns, 2 runs still produced
    // ok=2, conflict=6 because the first winner finished before the
    // second wave landed.
    //
    // **Deterministic design:**
    // 1. Pre-flip `state.reload_requested` to `true` — this EXACTLY
    //    mimics the on-wire condition "a prior reload is still
    //    in-flight when request B arrives". The CAS in `reload_all`
    //    will observe the flag set and take the conflict branch.
    // 2. Send request A — must return 409 (CAS loses).
    // 3. Clear the flag and send request B — must return 200 (CAS
    //    wins). This proves the flag isn't "stuck" and the reload
    //    path works end-to-end after a conflict was returned.
    //
    // This deterministically exercises the CAS-conflict code path,
    // which is exactly what "observable in tests, not just in code"
    // means for a branch that has no externally-observable side effect
    // other than the HTTP status code.
    use std::sync::atomic::Ordering;

    let (state, token) = test_state_with_api_key("sys-reload-race").await;

    // 1. Simulate "another reload in progress" by pre-acquiring the flag.
    state.reload_requested.store(true, Ordering::SeqCst);

    // 2. A reload request must now hit the CAS-conflict branch and
    //    return 409 Conflict with the JSON error envelope.
    let app_conflict = rustpbx::app::create_router(state.clone());
    let resp_conflict = app_conflict
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
    assert_eq!(
        resp_conflict.status(),
        StatusCode::CONFLICT,
        "reload while reload_requested=true must return 409"
    );
    let body = body_json(resp_conflict).await;
    assert_eq!(body["code"], "conflict");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("reload already in progress"),
        "error message must describe the conflict: {body}"
    );

    // 3. Clear the flag and confirm the happy path still works on the
    //    same AppState. This proves the CAS mechanism is reversible
    //    and a returned 409 does not leak flag state.
    state.reload_requested.store(false, Ordering::SeqCst);

    let app_ok = rustpbx::app::create_router(state.clone());
    let resp_ok = app_ok
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
    assert_eq!(
        resp_ok.status(),
        StatusCode::OK,
        "reload must succeed after the in-progress flag is cleared"
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
