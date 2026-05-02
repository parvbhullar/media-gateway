//! Integration tests for `/api/v1/security/*` (Phase 10 IT-01).
//!
//! Covers SEC-01..SEC-05 REST surface:
//!   1.  GET   /security/firewall       without auth  → 401
//!   2.  GET   /security/firewall       valid auth    → 200 + []
//!   3.  PATCH /security/firewall       valid rule    → 200 + rule echoed
//!   4.  PATCH /security/firewall       invalid CIDR  → 400
//!   5.  GET   /security/flood-tracker  valid auth    → 200 + {"data":[]}
//!   6.  GET   /security/blocks         valid auth    → 200 + {"data":[]}
//!   7.  DELETE /security/blocks/{ip}   non-existent  → 404
//!   8.  GET   /security/auth-failures  valid auth    → 200 + {"data":[]}

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

// =========================================================================
// 1. GET /security/firewall without auth → 401
// =========================================================================

#[tokio::test]
async fn list_firewall_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/security/firewall")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// =========================================================================
// 2. GET /security/firewall with auth → 200 + []
// =========================================================================

#[tokio::test]
async fn list_firewall_empty_returns_empty_array() {
    let (state, token) = test_state_with_api_key("sec-fw-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/security/firewall")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("body is a JSON array");
    assert!(arr.is_empty(), "expected [], got {:?}", arr);
}

// =========================================================================
// 3. PATCH /security/firewall with valid rule → 200 + rule echoed
// =========================================================================

#[tokio::test]
async fn replace_firewall_happy_returns_200() {
    let (state, token) = test_state_with_api_key("sec-fw-happy").await;
    let app = rustpbx::app::create_router(state);
    let body = json!({
        "rules": [
            {
                "position": 1,
                "action": "deny",
                "cidr": "10.0.0.0/8",
                "description": null
            }
        ]
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/security/firewall")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp_body = body_json(resp).await;
    let arr = resp_body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["position"], 1);
    assert_eq!(arr[0]["action"], "deny");
    assert_eq!(arr[0]["cidr"], "10.0.0.0/8");
}

// =========================================================================
// 4. PATCH /security/firewall with invalid CIDR → 400
// =========================================================================

#[tokio::test]
async fn replace_firewall_invalid_cidr_returns_400() {
    let (state, token) = test_state_with_api_key("sec-fw-bad-cidr").await;
    let app = rustpbx::app::create_router(state);
    let body = json!({
        "rules": [
            {
                "position": 1,
                "action": "deny",
                "cidr": "not-a-cidr",
                "description": null
            }
        ]
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/security/firewall")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// =========================================================================
// 5. GET /security/flood-tracker → 200 + {"data":[]}
// =========================================================================

#[tokio::test]
async fn list_flood_tracker_returns_empty_data() {
    let (state, token) = test_state_with_api_key("sec-flood").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/security/flood-tracker")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let data = body["data"].as_array().expect("data is an array");
    assert!(data.is_empty(), "expected empty data, got {:?}", data);
}

// =========================================================================
// 6. GET /security/blocks → 200 + {"data":[]}
// =========================================================================

#[tokio::test]
async fn list_blocks_returns_empty_data() {
    let (state, token) = test_state_with_api_key("sec-blocks-list").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/security/blocks")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let data = body["data"].as_array().expect("data is an array");
    assert!(data.is_empty(), "expected empty data, got {:?}", data);
}

// =========================================================================
// 7. DELETE /security/blocks/{ip} on non-existent → 404
// =========================================================================

#[tokio::test]
async fn delete_block_missing_returns_404() {
    let (state, token) = test_state_with_api_key("sec-blocks-del").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/security/blocks/1.2.3.4")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// =========================================================================
// 8. GET /security/auth-failures → 200 + {"data":[]}
// =========================================================================

#[tokio::test]
async fn list_auth_failures_returns_empty_data() {
    let (state, token) = test_state_with_api_key("sec-auth-fail").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/security/auth-failures")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let data = body["data"].as_array().expect("data is an array");
    assert!(data.is_empty(), "expected empty data, got {:?}", data);
}
