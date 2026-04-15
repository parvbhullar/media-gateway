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
    assert_eq!(reloaded.len(), 4);
    let names: Vec<&str> = reloaded.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(names, vec!["trunks", "routes", "acl", "app"]);

    assert!(body["elapsed_ms"].is_number());
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
