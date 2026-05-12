//! Integration tests for `/api/v1/routing/tables[/{name}]` (Phase 6
//! Plan 06-02 — RTE-01 / IT-01).
//!
//! Coverage matrix per `06-02-PLAN.md` <behavior>:
//!
//!   1.  401 without Bearer token (auth)
//!   2.  GET list empty returns 200 with `[]`
//!   3.  POST minimal body returns 200 with defaults (direction=both,
//!       priority=100, is_active=true, record_count=0)
//!   4.  POST with initial records persists `record_count`
//!   5.  POST duplicate name → 409
//!   6.  POST invalid direction → 400
//!   7.  POST invalid (uppercase) name → 400
//!   8.  POST records exceeding 1000 cap → 400
//!   9.  POST initial records with two `is_default: true` → 400 (D-18)
//!   10. GET by name returns view
//!   11. GET missing → 404
//!   12. PUT metadata returns updated view
//!   13. PUT body with `records` field → 400 (D-04 / deny_unknown_fields)
//!   14. PUT missing → 404
//!   15. DELETE happy → 200/204 then GET → 404
//!   16. DELETE missing → 404

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

fn make_record(prefix: &str, default: bool) -> Value {
    json!({
        "match": {"type": "lpm", "prefix": prefix},
        "target": {"kind": "trunk_group", "name": "carrier-a"},
        "is_default": default,
        "is_active": true,
    })
}

// =========================================================================
// 1. Auth (401)
// =========================================================================

#[tokio::test]
async fn list_tables_unauthenticated_returns_401() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/routing/tables")
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
async fn list_tables_empty_returns_200_with_empty_array() {
    let (state, token) = test_state_with_api_key("rt-list-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/routing/tables")
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
// 3. POST minimal → defaults populated
// =========================================================================

#[tokio::test]
async fn create_table_minimal_returns_200_with_defaults() {
    let (state, token) = test_state_with_api_key("rt-create-min").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name": "foo"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    assert!(
        status == axum::http::StatusCode::OK
            || status == axum::http::StatusCode::CREATED,
        "expected 200/201, got {} body {:?}",
        status,
        body
    );
    assert_eq!(body["name"], "foo");
    assert_eq!(body["direction"], "both");
    assert_eq!(body["priority"], 100);
    assert_eq!(body["is_active"], true);
    assert_eq!(body["record_count"], 0);
}

// =========================================================================
// 4. POST with records → record_count populated
// =========================================================================

#[tokio::test]
async fn create_table_with_records_persists_count() {
    let (state, token) = test_state_with_api_key("rt-create-rec").await;
    let app = rustpbx::app::create_router(state);
    let body = json!({
        "name": "with-records",
        "records": [
            make_record("+1415", false),
            make_record("+1212", false),
        ],
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp).await;
    assert!(
        status == axum::http::StatusCode::OK
            || status == axum::http::StatusCode::CREATED,
        "expected 200/201, got {} body {:?}",
        status,
        body
    );
    assert_eq!(body["record_count"], 2);
}

// =========================================================================
// 5. POST duplicate name → 409
// =========================================================================

#[tokio::test]
async fn create_table_duplicate_name_returns_409() {
    let (state, token) = test_state_with_api_key("rt-create-dup").await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name": "dup-name"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "first create should succeed, got {}",
        resp.status()
    );

    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name": "dup-name"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::CONFLICT);
    let body = body_json(resp2).await;
    assert_eq!(body["code"], "conflict");
}

// =========================================================================
// 6. POST invalid direction → 400
// =========================================================================

#[tokio::test]
async fn create_table_invalid_direction_returns_400() {
    let (state, token) = test_state_with_api_key("rt-create-bad-dir").await;
    let app = rustpbx::app::create_router(state);
    let body = json!({"name": "bad-dir", "direction": "sideways"});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 7. POST invalid uppercase name → 400
// =========================================================================

#[tokio::test]
async fn create_table_invalid_name_uppercase_returns_400() {
    let (state, token) = test_state_with_api_key("rt-create-bad-name").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name": "BadName"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 8. POST > 1000 records → 400
// =========================================================================

#[tokio::test]
async fn create_table_records_exceeding_cap_returns_400() {
    let (state, token) = test_state_with_api_key("rt-create-too-many").await;
    let app = rustpbx::app::create_router(state);
    let records: Vec<Value> = (0..1001)
        .map(|i| make_record(&format!("+{}", i), false))
        .collect();
    let body = json!({"name": "too-many", "records": records});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 9. POST two defaults → 400
// =========================================================================

#[tokio::test]
async fn create_table_multiple_defaults_returns_400() {
    let (state, token) = test_state_with_api_key("rt-create-2def").await;
    let app = rustpbx::app::create_router(state);
    let body = json!({
        "name": "two-defaults",
        "records": [
            make_record("+1", true),
            make_record("+2", true),
        ],
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 10. GET by name → view
// =========================================================================

#[tokio::test]
async fn get_table_by_name_returns_view() {
    let (state, token) = test_state_with_api_key("rt-get-ok").await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"name": "fetch-me", "description": "x"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/routing/tables/fetch-me")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::OK);
    let body = body_json(resp2).await;
    assert_eq!(body["name"], "fetch-me");
    assert_eq!(body["description"], "x");
}

// =========================================================================
// 11. GET missing → 404
// =========================================================================

#[tokio::test]
async fn get_table_missing_returns_404() {
    let (state, token) = test_state_with_api_key("rt-get-miss").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/routing/tables/nope")
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
// 12. PUT metadata → updated view
// =========================================================================

#[tokio::test]
async fn update_table_metadata_returns_updated_view() {
    let (state, token) = test_state_with_api_key("rt-put-ok").await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name": "patch-me"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/routing/tables/patch-me")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"description": "updated", "priority": 5})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::OK);
    let body = body_json(resp2).await;
    assert_eq!(body["description"], "updated");
    assert_eq!(body["priority"], 5);
}

// =========================================================================
// 13. PUT with `records` field → 400
// =========================================================================

#[tokio::test]
async fn update_table_with_records_field_returns_400() {
    let (state, token) = test_state_with_api_key("rt-put-rec").await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name": "norec"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/routing/tables/norec")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"description": "ok", "records": []}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 14. PUT missing → 404
// =========================================================================

#[tokio::test]
async fn update_table_missing_returns_404() {
    let (state, token) = test_state_with_api_key("rt-put-miss").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/routing/tables/nope")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"description": "x"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

// =========================================================================
// 15. DELETE → then GET 404
// =========================================================================

#[tokio::test]
async fn delete_table_returns_200_then_404_on_get() {
    let (state, token) = test_state_with_api_key("rt-del-ok").await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/tables")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name": "kill-me"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/routing/tables/kill-me")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp2.status() == axum::http::StatusCode::OK
            || resp2.status() == axum::http::StatusCode::NO_CONTENT,
        "expected 200/204, got {}",
        resp2.status()
    );

    let app3 = rustpbx::app::create_router(state);
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri("/api/v1/routing/tables/kill-me")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp3.status(), axum::http::StatusCode::NOT_FOUND);
}

// =========================================================================
// 16. DELETE missing → 404
// =========================================================================

#[tokio::test]
async fn delete_table_missing_returns_404() {
    let (state, token) = test_state_with_api_key("rt-del-miss").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/routing/tables/nope")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}
