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

// ---------------------------------------------------------------------------
// GET /system/info — Phase 11 SYS-03
// ---------------------------------------------------------------------------

#[tokio::test]
async fn info_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn info_happy_path_shape() {
    let (state, token) = test_state_with_api_key("sys-info").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/info")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert!(body["version"].is_string());
    assert!(!body["version"].as_str().unwrap().is_empty());
    assert!(body["build"].is_object());
    assert!(body["build"]["time"].is_string());
    assert!(body["build"]["git_commit"].is_string());
    assert!(body["build"]["git_branch"].is_string());
    assert!(
        body["build"]["git_dirty"].is_boolean(),
        "git_dirty must be a bool: {body}"
    );
    assert!(body["full_version_string"].is_string());
    assert!(
        body["full_version_string"]
            .as_str()
            .unwrap()
            .contains("rustpbx"),
        "full_version_string should contain rustpbx: {body}"
    );
}

// ---------------------------------------------------------------------------
// GET /system/config — Phase 11 SYS-04
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn config_happy_path_shape() {
    let (state, token) = test_state_with_api_key("sys-config").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/config")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    // SAFE_PROXY_FIELDS positive: addr is exposed.
    assert!(
        body["proxy"]["addr"].is_string(),
        "proxy.addr should be exposed: {body}"
    );

    // Negative-allowlist proof: sensitive fields must NEVER appear, even if
    // they exist on ProxyConfig (e.g. ssl_private_key, ssl_certificate).
    assert!(
        body["proxy"]["ssl_private_key"].is_null(),
        "ssl_private_key must NEVER appear in /system/config response: {body}"
    );
    assert!(
        body["proxy"]["ssl_certificate"].is_null(),
        "ssl_certificate must NEVER appear in /system/config response: {body}"
    );
    // jwt_secret is not a ProxyConfig field but the test asserts the
    // negative invariant for forward-compatibility — any future addition
    // named jwt_secret would still be excluded by the allowlist.
    assert!(
        body["proxy"]["jwt_secret"].is_null(),
        "jwt_secret must NEVER appear in /system/config response: {body}"
    );
    // database_url lives on Config (not ProxyConfig) and must never leak.
    assert!(
        body["proxy"]["database_url"].is_null(),
        "database_url must NEVER appear in /system/config response: {body}"
    );

    // runtime block must be a (possibly empty) JSON object.
    assert!(
        body["runtime"].is_object(),
        "runtime must be a JSON object: {body}"
    );
}

// ---------------------------------------------------------------------------
// GET /system/cluster — Phase 11 SYS-06 (constant)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cluster_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/cluster")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cluster_happy_path_constant() {
    let (state, token) = test_state_with_api_key("sys-cluster").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/cluster")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(body["mode"], "single_node");
    let nodes = body["nodes"].as_array().expect("nodes is array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], "primary");
    assert_eq!(nodes[0]["role"], "primary");
    assert_eq!(nodes[0]["healthy"], true);
    assert!(body["note"].is_string());
    assert!(!body["note"].as_str().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// GET /system/stats — Phase 11 SYS-05
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stats_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stats_happy_path_shape() {
    let (state, token) = test_state_with_api_key("sys-stats").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/system/stats")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    // calls
    assert!(body["calls"]["active"].is_number());
    assert!(body["calls"]["total_24h"].is_number());
    assert!(body["calls"]["failed_24h"].is_number());
    // proxy
    assert!(body["proxy"]["uptime_secs"].is_number());
    assert!(body["proxy"]["active_dialogs"].is_number());
    assert!(body["proxy"]["registrations"].is_number());
    // gateways
    assert!(body["gateways"]["up"].is_number());
    assert!(body["gateways"]["down"].is_number());
    assert!(body["gateways"]["total"].is_number());
    // security
    assert!(body["security"]["blocks_total"].is_number());
    assert!(body["security"]["flood_rejected_24h"].is_number());
    assert!(body["security"]["auth_failures_24h"].is_number());
}

// ---------------------------------------------------------------------------
// AMI X-Deprecation header — Phase 11 MIG-04
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ami_response_carries_deprecation_header() {
    // Hit the AMI legacy /ami/v1/health endpoint. AMI auth allows
    // 127.0.0.1 by default — pass that via the X-Forwarded-For header,
    // which `ClientAddr::from_http_parts` honours when no `ConnectInfo`
    // is attached (oneshot does not).
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/ami/v1/health")
                .header("x-forwarded-for", "127.0.0.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let header_value = resp
        .headers()
        .get("x-deprecation")
        .expect("x-deprecation header must be present on AMI response")
        .to_str()
        .expect("x-deprecation header must be ASCII");
    assert!(
        header_value.contains("/api/v1/system/"),
        "x-deprecation header must reference /api/v1/system/: got {header_value:?}"
    );
}

#[tokio::test]
async fn ami_deprecation_header_present_even_when_denied() {
    // The middleware is layered AFTER auth, so even a 403 from the AMI
    // gate carries the migration hint. We do this by sending a request
    // with no client_ip override — default ClientAddr is 0.0.0.0 which
    // is NOT in the default `AmiConfig` allowlist (127.0.0.1/::1 only).
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/ami/v1/health")
                .header("x-forwarded-for", "203.0.113.42")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        resp.headers().get("x-deprecation").is_some(),
        "x-deprecation header must be present even on 403 from AMI auth"
    );
}
