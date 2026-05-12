//! Integration tests for `/api/v1/dids` (Phase 1, Plan 01 Task 2).
//!
//! Every route is asserted against the IT-01 contract:
//! 1. 401 without Bearer token
//! 2. Happy path with valid token
//! 3. 404 on missing resource
//! 4. 400 / 409 on bad input

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

fn bearer(token: &str) -> String {
    format!("Bearer {}", token)
}

async fn seed_did(
    state: &rustpbx::app::AppState,
    number: &str,
    trunk: Option<&str>,
    label: Option<&str>,
) {
    let new = rustpbx::models::did::NewDid {
        number: number.to_string(),
        trunk_name: trunk.map(|s| s.to_string()),
        extension_number: None,
        failover_trunk: None,
        label: label.map(|s| s.to_string()),
        enabled: true,
    };
    rustpbx::models::did::Model::upsert(state.db(), new)
        .await
        .expect("seed did");
}

// ---------------------------------------------------------------------------
// GET /api/v1/dids
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_dids_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dids")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_dids_returns_paginated_envelope_when_empty() {
    let (state, token) = test_state_with_api_key("list-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dids")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["items"].is_array(), "items must be an array: {body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 20);
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn list_dids_returns_seeded_rows() {
    let (state, token) = test_state_with_api_key("list-seeded").await;
    seed_did(&state, "+14155550001", Some("carrier-a"), Some("main")).await;
    seed_did(&state, "+14155550002", None, Some("parked")).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dids")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(body["total"], 2);
    assert_eq!(items[0]["number"], "+14155550001");
    assert_eq!(items[0]["trunk_name"], "carrier-a");
    assert_eq!(items[1]["number"], "+14155550002");
    assert!(items[1]["trunk_name"].is_null());
}

#[tokio::test]
async fn list_dids_filters_by_trunk() {
    let (state, token) = test_state_with_api_key("list-filter-trunk").await;
    seed_did(&state, "+14155550001", Some("carrier-a"), None).await;
    seed_did(&state, "+14155550002", Some("carrier-b"), None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dids?trunk=carrier-a")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["trunk_name"], "carrier-a");
}

#[tokio::test]
async fn list_dids_filters_unassigned() {
    let (state, token) = test_state_with_api_key("list-filter-unassigned").await;
    seed_did(&state, "+14155550001", Some("carrier-a"), None).await;
    seed_did(&state, "+14155550002", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dids?unassigned=true")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert!(body["items"][0]["trunk_name"].is_null());
}

#[tokio::test]
async fn list_dids_pagination_second_page() {
    let (state, token) = test_state_with_api_key("list-paginate").await;
    for i in 1..=5 {
        seed_did(&state, &format!("+1415555000{}", i), None, None).await;
    }

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dids?page=2&page_size=2")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["total"], 5);
    assert_eq!(body["page"], 2);
    assert_eq!(body["page_size"], 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["items"][0]["number"], "+14155550003");
    assert_eq!(body["items"][1]["number"], "+14155550004");
}

// ---------------------------------------------------------------------------
// POST /api/v1/dids
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_did_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/dids")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"number":"+14155551111"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_did_happy_path_returns_201() {
    let (state, token) = test_state_with_api_key("create-happy").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/dids")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"number":"+14155551111","trunk_name":"carrier-a","label":"primary"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["number"], "+14155551111");
    assert_eq!(body["trunk_name"], "carrier-a");
    assert_eq!(body["label"], "primary");
    assert_eq!(body["enabled"], true);
}

#[tokio::test]
async fn create_did_duplicate_returns_409() {
    let (state, token) = test_state_with_api_key("create-dup").await;
    seed_did(&state, "+14155552222", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/dids")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"number":"+14155552222"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "conflict");
}

#[tokio::test]
async fn create_did_invalid_number_returns_400() {
    let (state, token) = test_state_with_api_key("create-invalid").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/dids")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"number":"not-a-number"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "bad_request");
}

#[tokio::test]
async fn create_did_rejects_unknown_fields() {
    let (state, token) = test_state_with_api_key("create-unknown").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/dids")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"number":"+14155553333","bogus_field":"x"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    // deny_unknown_fields triggers a 422 from axum's JSON extractor path,
    // but some axum versions map it to 400. Accept either.
    let status = resp.status();
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "unexpected status: {status}"
    );
}

// ---------------------------------------------------------------------------
// GET /api/v1/dids/{number}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_did_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dids/%2B14155554444")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_did_happy_path() {
    let (state, token) = test_state_with_api_key("get-happy").await;
    seed_did(&state, "+14155554444", Some("carrier-a"), Some("main")).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dids/%2B14155554444")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["number"], "+14155554444");
    assert_eq!(body["trunk_name"], "carrier-a");
    assert_eq!(body["label"], "main");
}

#[tokio::test]
async fn get_did_missing_returns_404() {
    let (state, token) = test_state_with_api_key("get-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dids/%2B14155559999")
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
// PUT /api/v1/dids/{number}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_did_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/dids/%2B14155555555")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"label":"new"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn update_did_happy_path() {
    let (state, token) = test_state_with_api_key("update-happy").await;
    seed_did(&state, "+14155555555", Some("carrier-a"), Some("old")).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/dids/%2B14155555555")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"trunk_name":"carrier-b","label":"new","enabled":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["number"], "+14155555555");
    assert_eq!(body["trunk_name"], "carrier-b");
    assert_eq!(body["label"], "new");
    assert_eq!(body["enabled"], false);
}

#[tokio::test]
async fn update_did_missing_returns_404() {
    let (state, token) = test_state_with_api_key("update-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/dids/%2B14155556666")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"label":"ghost"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// DELETE /api/v1/dids/{number}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_did_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/dids/%2B14155557777")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_did_happy_path_returns_204() {
    let (state, token) = test_state_with_api_key("delete-happy").await;
    seed_did(&state, "+14155557777", None, None).await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/dids/%2B14155557777")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Row should be gone.
    let gone = rustpbx::models::did::Model::get(state.db(), "+14155557777")
        .await
        .unwrap();
    assert!(gone.is_none());
}

#[tokio::test]
async fn delete_did_missing_returns_404() {
    let (state, token) = test_state_with_api_key("delete-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/dids/%2B14155558888")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
