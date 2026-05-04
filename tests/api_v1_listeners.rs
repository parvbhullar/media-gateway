//! Integration tests for `/api/v1/listeners` (Phase 12, LSTN-01..04).

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key, test_state_with_config_mut};

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
// Auth gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/listeners")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// GET /api/v1/listeners — list all
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_returns_four_entries() {
    let (state, token) = test_state_with_api_key("lstn-list").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/listeners")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 4);
    assert_eq!(items[0]["protocol"], "udp");
    assert_eq!(items[1]["protocol"], "tcp");
    assert_eq!(items[2]["protocol"], "tls");
    assert_eq!(items[3]["protocol"], "ws");
}

// ---------------------------------------------------------------------------
// GET /api/v1/listeners/{name} — single entry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_returns_listener_by_name() {
    let (state, token) = test_state_with_api_key("lstn-get").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/listeners/udp")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["protocol"], "udp");
}

#[tokio::test]
async fn get_unknown_returns_404() {
    let (state, token) = test_state_with_api_key("lstn-404").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/listeners/sctp")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_found");
}

// ---------------------------------------------------------------------------
// Write stubs — POST / PUT / DELETE return 501
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_returns_501() {
    let (state, token) = test_state_with_api_key("lstn-post").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/listeners")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_implemented");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("Multi-listener configuration"),
        "error must contain locked D-05 message: {body}"
    );
}

#[tokio::test]
async fn put_returns_501() {
    let (state, token) = test_state_with_api_key("lstn-put").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/listeners/udp")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_implemented");
}

#[tokio::test]
async fn delete_returns_501() {
    let (state, token) = test_state_with_api_key("lstn-del").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/listeners/udp")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_implemented");
}

// ---------------------------------------------------------------------------
// Disabled port encoding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disabled_port_marks_enabled_false() {
    // Configure tls_port=None so the tls entry is disabled.
    let (state, token) =
        test_state_with_config_mut("lstn-disabled", |c| {
            c.proxy.tls_port = None;
        })
        .await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/listeners")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().expect("items array");
    let tls = items
        .iter()
        .find(|l| l["protocol"] == "tls")
        .expect("tls entry must be present");
    assert_eq!(tls["enabled"], false, "tls entry must be disabled: {tls}");
    assert_eq!(tls["port"], 0, "disabled port must be 0: {tls}");
}
