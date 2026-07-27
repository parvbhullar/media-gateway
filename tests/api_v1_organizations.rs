//! Integration tests for `/api/v1/organizations` — org-level multi-tenancy
//! CRUD, disable/enable, and usage reporting.

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use tower::ServiceExt;

mod common;
use common::test_state_with_api_key;

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

fn bearer(token: &str) -> String {
    format!("Bearer {}", token)
}

#[tokio::test]
async fn upsert_list_disable_enable_round_trip() {
    let (state, token) = test_state_with_api_key("orgs-round-trip").await;
    let app = rustpbx::app::create_router(state);

    // 1. PUT /organizations/acme with name+limits -> 201.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/organizations/acme")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Acme Corp",
                        "max_cps": 10,
                        "max_calls": 50
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["org_id"], "acme");
    assert_eq!(body["name"], "Acme Corp");
    assert_eq!(body["enabled"], true);

    // 2. GET /organizations -> contains "acme" with today counts all 0.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/organizations")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let orgs = body.as_array().expect("array");
    let acme = orgs
        .iter()
        .find(|o| o["org_id"] == "acme")
        .expect("acme present in list");
    assert_eq!(acme["enabled"], true);
    assert_eq!(acme["today"]["did_count"], 0);
    assert_eq!(acme["today"]["trunk_count"], 0);
    assert_eq!(acme["today"]["extension_count"], 0);
    assert_eq!(acme["today"]["call_record_count_today"], 0);

    // 3. PATCH /organizations/acme/disable {"action":"immediate"} -> 200, enabled:false.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/organizations/acme/disable")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"action": "immediate"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["enabled"], false);

    // 4. GET /organizations/acme -> enabled:false.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/organizations/acme")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["enabled"], false);

    // 5. PATCH /organizations/acme/enable -> 200, enabled:true.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/organizations/acme/enable")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["enabled"], true);
}

#[tokio::test]
async fn disable_enable_unknown_org_returns_404() {
    let (state, token) = test_state_with_api_key("orgs-404").await;
    let app = rustpbx::app::create_router(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/organizations/no-such-org/disable")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"action": "immediate"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/organizations/no-such-org/enable")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Regression test: PUT /organizations/{id} omitting `enabled` must not
/// silently re-enable a disabled org. Only an explicit `enabled` in the body
/// (or a true create) may change the enabled state.
#[tokio::test]
async fn put_without_enabled_field_preserves_disabled_state() {
    let (state, token) = test_state_with_api_key("orgs-preserve-disabled").await;
    let app = rustpbx::app::create_router(state);

    // Create the org (defaults to enabled:true).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/organizations/initech")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"name": "Initech"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Disable it.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/organizations/initech/disable")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"action": "immediate"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["enabled"], false);

    // PUT again, changing only contact info, omitting `enabled` entirely.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/organizations/initech")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "Initech",
                        "contact_name": "Bill Lumbergh"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    // Must still be disabled — the PUT did not include `enabled`.
    assert_eq!(body["enabled"], false);
    assert_eq!(body["contact_name"], "Bill Lumbergh");

    // GET confirms the disabled state stuck.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/organizations/initech")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["enabled"], false);
}

/// Regression test: a `drain` disable schedules a delayed hangup task keyed
/// off the org row's `updated_at` generation. If the org is re-enabled
/// before the grace period elapses, the stale task must not clobber the
/// re-enable when it eventually fires.
#[tokio::test]
async fn drain_task_does_not_clobber_a_later_re_enable() {
    let (state, token) = test_state_with_api_key("orgs-drain-race").await;
    let app = rustpbx::app::create_router(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/organizations/globex")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"name": "Globex"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Disable with a 1s drain grace period.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/organizations/globex/disable")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"action": "drain", "grace_seconds": 1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Immediately re-enable, before the drain task's grace period elapses.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/organizations/globex/enable")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Wait past the drain grace period so the stale task fires.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // The stale drain task must not have reverted or otherwise disturbed
    // the re-enable.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/organizations/globex")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["enabled"], true);
}
