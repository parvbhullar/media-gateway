//! Integration tests for `/api/v1/endpoints` (Phase 13 Plan 13-02 — EPUA-01..05).
//!
//! Test matrix:
//!   1.  401 without Bearer token (list)
//!   2.  list-empty returns `[]`
//!   3.  POST happy round-trip: 201 + follow-up GET
//!   4.  POST duplicate username returns 409
//!   5.  POST empty username returns 400
//!   6.  POST empty password returns 400
//!   7.  GET by UUID happy path
//!   8.  GET missing UUID returns 404
//!   9.  PUT (update) password changes ha1 in DB (response has no ha1)
//!   10. DELETE happy returns 204 + follow-up GET shows removed
//!   11. DELETE missing UUID returns 404
//!   12. Tenant isolation: sub-account cannot see master endpoints

use axum::{
    body::Body,
    http::{Request, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::{test_state_with_api_key, test_state_with_api_key_for_account};

// ---- helpers ----------------------------------------------------------------

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

fn bearer(token: &str) -> String {
    format!("Bearer {}", token)
}

// ---- 1. 401 without Bearer token -------------------------------------------

#[tokio::test]
async fn list_endpoints_requires_auth() {
    let (state, _token) = test_state_with_api_key("ep-no-auth").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/endpoints")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

// ---- 2. list-empty ----------------------------------------------------------

#[tokio::test]
async fn list_endpoints_empty_returns_empty_array() {
    let (state, token) = test_state_with_api_key("ep-list-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("body is a JSON array");
    assert!(arr.is_empty(), "expected [], got {:?}", arr);
}

// ---- 3. POST happy round-trip -----------------------------------------------

#[tokio::test]
async fn create_endpoint_happy_returns_201_and_round_trips() {
    let (state, token) = test_state_with_api_key("ep-create-happy").await;

    // POST
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "username": "alice",
                        "password": "s3cr3t",
                        "realm": "sip.example.com"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let body = body_json(resp).await;

    // Response shape per D-14 — no password, no ha1
    assert_eq!(body["username"], "alice");
    assert_eq!(body["realm"], "sip.example.com");
    assert!(body["id"].as_str().is_some(), "id must be a string UUID");
    assert!(body.get("password").is_none(), "password must not be in response");
    assert!(body.get("ha1").is_none(), "ha1 must not be in response");
    assert_eq!(body["sip_registered"], false);
    assert!(body["last_register_at"].is_null());

    let id = body["id"].as_str().unwrap().to_string();

    // GET by UUID round-trip
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/endpoints/{}", id))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    let body2 = body_json(resp2).await;
    assert_eq!(body2["id"], id.as_str());
    assert_eq!(body2["username"], "alice");
}

// ---- 4. POST duplicate username → 409 --------------------------------------

#[tokio::test]
async fn create_endpoint_duplicate_username_returns_409() {
    let (state, token) = test_state_with_api_key("ep-dup").await;

    let payload = json!({"username": "bob", "password": "pw1"}).to_string();

    // First POST — 201
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Second POST with same username — 409
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 409);
    let body = body_json(resp2).await;
    assert_eq!(body["code"], "conflict");
}

// ---- 5. POST empty username → 400 ------------------------------------------

#[tokio::test]
async fn create_endpoint_empty_username_returns_400() {
    let (state, token) = test_state_with_api_key("ep-bad-user").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "", "password": "pw"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "bad_request");
}

// ---- 6. POST empty password → 400 ------------------------------------------

#[tokio::test]
async fn create_endpoint_empty_password_returns_400() {
    let (state, token) = test_state_with_api_key("ep-bad-pass").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "carol", "password": ""}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "bad_request");
}

// ---- 7. GET by UUID happy ---------------------------------------------------

#[tokio::test]
async fn get_endpoint_by_id_happy() {
    let (state, token) = test_state_with_api_key("ep-get-id").await;

    // Create first
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "dave", "password": "pw"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let created = body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();

    // GET
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/endpoints/{}", id))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    let body = body_json(resp2).await;
    assert_eq!(body["username"], "dave");
    assert_eq!(body["id"], id.as_str());
}

// ---- 8. GET missing UUID → 404 ---------------------------------------------

#[tokio::test]
async fn get_endpoint_missing_returns_404() {
    let (state, token) = test_state_with_api_key("ep-get-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/endpoints/00000000-0000-0000-0000-000000000000")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_found");
}

// ---- 9. PUT changes password (response has no ha1 / password) --------------

#[tokio::test]
async fn update_endpoint_password_no_ha1_in_response() {
    let (state, token) = test_state_with_api_key("ep-update-pw").await;

    // Create
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "eve", "password": "old-pw"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let created = body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();

    // PUT with new password
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/v1/endpoints/{}", id))
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"password": "new-pw"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    let body = body_json(resp2).await;
    assert_eq!(body["username"], "eve");
    assert!(body.get("password").is_none(), "password must not appear in response");
    assert!(body.get("ha1").is_none(), "ha1 must not appear in response");
}

// ---- 10. DELETE happy → 204 + follow-up list shows removed -----------------

#[tokio::test]
async fn delete_endpoint_happy_returns_204() {
    let (state, token) = test_state_with_api_key("ep-delete-happy").await;

    // Create
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "frank", "password": "pw"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let created = body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();

    // DELETE
    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/endpoints/{}", id))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 204);

    // Follow-up list — should be empty
    let app3 = rustpbx::app::create_router(state);
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp3.status().as_u16(), 200);
    let body3 = body_json(resp3).await;
    let arr = body3.as_array().expect("array");
    assert!(arr.is_empty(), "expected [] after delete, got {:?}", arr);
}

// ---- 11. DELETE missing UUID → 404 -----------------------------------------

#[tokio::test]
async fn delete_endpoint_missing_returns_404() {
    let (state, token) = test_state_with_api_key("ep-delete-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/endpoints/00000000-0000-0000-0000-000000000001")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_found");
}

// ---- 12. Tenant isolation: sub-account cannot see master endpoints ----------

#[tokio::test]
async fn tenant_isolation_sub_account_cannot_see_master_endpoints() {
    let (state, master_token) =
        test_state_with_api_key_for_account("ep-iso-master", "root").await;

    // Insert endpoint under master (root) account
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&master_token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "grace", "password": "pw"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Issue a sub-account API key
    use chrono::Utc;
    use rustpbx::{
        handler::api_v1::auth::{IssuedKey, issue_api_key},
        models::api_key,
    };
    use sea_orm::{ActiveModelTrait, Set};
    let IssuedKey { plaintext: sub_token, hash } = issue_api_key();
    api_key::ActiveModel {
        name: Set("ep-iso-sub".to_string()),
        hash_sha256: Set(hash),
        description: Set(None),
        created_at: Set(Utc::now()),
        account_id: Set("tenant-x".to_string()),
        ..Default::default()
    }
    .insert(state.db())
    .await
    .expect("insert sub-account key");

    // Sub-account list must be empty
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/endpoints")
                .header(header::AUTHORIZATION, bearer(&sub_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    let body = body_json(resp2).await;
    let arr = body.as_array().expect("array");
    assert!(
        arr.is_empty(),
        "sub-account must not see master endpoints, got {:?}",
        arr
    );
}
