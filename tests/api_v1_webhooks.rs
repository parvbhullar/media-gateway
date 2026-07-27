//! Integration tests for `/api/v1/webhooks[/{id}]` (Phase 7 Plan 07-02 —
//! WH-01 / WH-06 / IT-01).
//!
//! Coverage matrix per `07-02-PLAN.md` <behavior>:
//!
//!   1.  401 without Bearer token (auth)
//!   2.  GET list empty returns 200 with `[]`
//!   3.  POST valid payload returns 201 + WebhookView
//!   4.  POST duplicate name → 409 with code=conflict
//!   5.  POST localhost url → 400 with code=bad_request
//!   6.  POST RFC1918 url (http://10.0.0.5/h) → 201 (D-27 allowed)
//!   7.  POST events=["bogus.event"] → 400, message lists valid events
//!   8.  POST timeout_ms=50 → 400
//!   9.  POST retry_count=11 → 400
//!   10. POST invalid url scheme (file:) → 400
//!   11. GET /webhooks/{id} happy → 200
//!   12. GET /webhooks/non-existent → 404
//!   13. PUT happy full-replacement → 200, fields updated
//!   14. PUT non-existent → 404
//!   15. DELETE happy → 204
//!   16. DELETE non-existent → 404

use axum::{
    body::Body,
    http::{Request, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

// ─── Helpers ────────────────────────────────────────────────────────────

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

fn create_payload(name: &str) -> Value {
    json!({
        "name": name,
        "url": "https://example.com/hook",
        "secret": "shh",
    })
}

async fn create_webhook(
    state: rustpbx::app::AppState,
    token: &str,
    body: Value,
) -> (axum::http::StatusCode, Value) {
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/webhooks")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    (status, body)
}

// =========================================================================
// 1. Auth (401)
// =========================================================================

#[tokio::test]
async fn it_wh_list_unauthenticated_returns_401() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/webhooks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
}

// =========================================================================
// 2. List empty → 200 + []
// =========================================================================

#[tokio::test]
async fn it_wh_list_empty_returns_empty_array() {
    let (state, token) = test_state_with_api_key("wh-list-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/webhooks")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert!(arr.is_empty(), "expected [], got {:?}", arr);
}

// =========================================================================
// 3. POST valid → 201
// =========================================================================

#[tokio::test]
async fn it_wh_create_happy_returns_201() {
    let (state, token) = test_state_with_api_key("wh-create").await;
    let body = json!({
        "name": "primary",
        "url": "https://example.com/hook",
        "secret": "shh",
        "events": ["call.completed"],
        "description": "main",
        "is_active": true,
        "retry_count": 5,
        "timeout_ms": 10000,
    });
    let (status, body) = create_webhook(state, &token, body).await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "expected 201, got {} body {:?}",
        status,
        body
    );
    assert_eq!(body["name"], "primary");
    assert_eq!(body["url"], "https://example.com/hook");
    assert_eq!(body["secret"], "shh"); // plaintext per D-35
    assert_eq!(body["retry_count"], 5);
    assert_eq!(body["timeout_ms"], 10000);
    assert_eq!(body["is_active"], true);
    assert!(
        body["id"].as_str().map(|s| s.len() == 36).unwrap_or(false),
        "id should be uuid-shaped, got {:?}",
        body["id"]
    );
    assert_eq!(body["events"], json!(["call.completed"]));
}

// =========================================================================
// 4. POST duplicate name → 409
// =========================================================================

#[tokio::test]
async fn it_wh_create_duplicate_name_returns_409() {
    let (state, token) = test_state_with_api_key("wh-dup").await;
    let (s1, _) = create_webhook(
        state.clone(),
        &token,
        create_payload("dup-name"),
    )
    .await;
    assert_eq!(s1, axum::http::StatusCode::CREATED);

    let (s2, body) = create_webhook(
        state,
        &token,
        create_payload("dup-name"),
    )
    .await;
    assert_eq!(s2, axum::http::StatusCode::CONFLICT);
    assert_eq!(body["code"], "conflict");
}

// =========================================================================
// 5. POST localhost url → 400
// =========================================================================

#[tokio::test]
async fn it_wh_create_localhost_url_returns_400() {
    let (state, token) = test_state_with_api_key("wh-localhost").await;
    let body = json!({
        "name": "loc",
        "url": "http://localhost:8080/hook",
        "secret": "s",
    });
    let (status, body) = create_webhook(state, &token, body).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "bad_request");
}

// =========================================================================
// 6. POST RFC1918 url → 201 (D-27 explicit allow)
// =========================================================================

#[tokio::test]
async fn it_wh_create_rfc1918_url_allowed_d27() {
    let (state, token) = test_state_with_api_key("wh-rfc1918").await;
    let body = json!({
        "name": "internal",
        "url": "http://10.0.0.5/hook",
        "secret": "s",
    });
    let (status, body) = create_webhook(state, &token, body).await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "RFC1918 must be allowed per D-27, got {} body {:?}",
        status,
        body
    );
    assert_eq!(body["url"], "http://10.0.0.5/hook");
}

// =========================================================================
// 7. POST invalid event → 400
// =========================================================================

#[tokio::test]
async fn it_wh_create_invalid_event_returns_400() {
    let (state, token) = test_state_with_api_key("wh-bad-event").await;
    let body = json!({
        "name": "bad-event",
        "url": "https://example.com/hook",
        "secret": "s",
        "events": ["bogus.event"],
    });
    let (status, body) = create_webhook(state, &token, body).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "bad_request");
    let msg = body["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("call.started") && msg.contains("webhook.test"),
        "error message must list valid events (D-09), got {:?}",
        msg
    );
}

// =========================================================================
// 8. POST timeout_ms=50 → 400
// =========================================================================

#[tokio::test]
async fn it_wh_create_timeout_below_min_returns_400() {
    let (state, token) = test_state_with_api_key("wh-bad-timeout").await;
    let body = json!({
        "name": "bad-timeout",
        "url": "https://example.com/hook",
        "secret": "s",
        "timeout_ms": 50,
    });
    let (status, _) = create_webhook(state, &token, body).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 9. POST retry_count=11 → 400
// =========================================================================

#[tokio::test]
async fn it_wh_create_retry_count_above_max_returns_400() {
    let (state, token) = test_state_with_api_key("wh-bad-retry").await;
    let body = json!({
        "name": "bad-retry",
        "url": "https://example.com/hook",
        "secret": "s",
        "retry_count": 11,
    });
    let (status, _) = create_webhook(state, &token, body).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 10. POST invalid url scheme → 400
// =========================================================================

#[tokio::test]
async fn it_wh_create_invalid_scheme_returns_400() {
    let (state, token) = test_state_with_api_key("wh-bad-scheme").await;
    let body = json!({
        "name": "bad-scheme",
        "url": "file:///etc/passwd",
        "secret": "s",
    });
    let (status, _) = create_webhook(state, &token, body).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 11. GET /webhooks/{id} happy → 200
// =========================================================================

#[tokio::test]
async fn it_wh_get_by_id_returns_view() {
    let (state, token) = test_state_with_api_key("wh-get-ok").await;
    let (cs, created) = create_webhook(
        state.clone(),
        &token,
        create_payload("fetch-me"),
    )
    .await;
    assert_eq!(cs, axum::http::StatusCode::CREATED);
    let id = created["id"].as_str().expect("id present").to_string();

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/webhooks/{}", id))
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["id"], id);
    assert_eq!(body["name"], "fetch-me");
}

// =========================================================================
// 12. GET missing → 404
// =========================================================================

#[tokio::test]
async fn it_wh_get_missing_returns_404() {
    let (state, token) = test_state_with_api_key("wh-get-miss").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/webhooks/does-not-exist")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_found");
}

// =========================================================================
// 13. PUT happy → 200, fields updated
// =========================================================================

#[tokio::test]
async fn it_wh_update_happy_returns_updated_view() {
    let (state, token) = test_state_with_api_key("wh-put-ok").await;
    let (cs, created) = create_webhook(
        state.clone(),
        &token,
        create_payload("patch-me"),
    )
    .await;
    assert_eq!(cs, axum::http::StatusCode::CREATED);
    let id = created["id"].as_str().expect("id").to_string();

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/v1/webhooks/{}", id))
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "description": "updated",
                        "retry_count": 7,
                        "events": ["call.completed", "call.failed"],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["description"], "updated");
    assert_eq!(body["retry_count"], 7);
    assert_eq!(body["events"], json!(["call.completed", "call.failed"]));
}

// =========================================================================
// 14. PUT missing → 404
// =========================================================================

#[tokio::test]
async fn it_wh_update_missing_returns_404() {
    let (state, token) = test_state_with_api_key("wh-put-miss").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/webhooks/non-existent-id")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"description": "x"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_found");
}

// =========================================================================
// 15. DELETE happy → 204
// =========================================================================

#[tokio::test]
async fn it_wh_delete_happy_returns_204() {
    let (state, token) = test_state_with_api_key("wh-del-ok").await;
    let (cs, created) = create_webhook(
        state.clone(),
        &token,
        create_payload("kill-me"),
    )
    .await;
    assert_eq!(cs, axum::http::StatusCode::CREATED);
    let id = created["id"].as_str().expect("id").to_string();

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/webhooks/{}", id))
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);

    // Confirm GET now returns 404.
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/webhooks/{}", id))
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::NOT_FOUND);
}

// =========================================================================
// 16. DELETE missing → 404
// =========================================================================

#[tokio::test]
async fn it_wh_delete_missing_returns_404() {
    let (state, token) = test_state_with_api_key("wh-del-miss").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/webhooks/non-existent-id")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}
