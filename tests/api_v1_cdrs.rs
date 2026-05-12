//! Integration tests for `/api/v1/cdrs` (Phase 1, Plan 01-03).

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::{Duration as ChronoDuration, Utc};
use rustpbx::models::call_record::{self, ActiveModel as CdrAm};
use sea_orm::{ActiveModelTrait, Set};
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

async fn seed_cdr(
    state: &rustpbx::app::AppState,
    call_id: &str,
    direction: &str,
    status: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> call_record::Model {
    let now = Utc::now();
    let am = CdrAm {
        call_id: Set(call_id.to_string()),
        direction: Set(direction.to_string()),
        status: Set(status.to_string()),
        started_at: Set(now),
        ended_at: Set(Some(now + ChronoDuration::seconds(30))),
        duration_secs: Set(30),
        from_number: Set(from.map(String::from)),
        to_number: Set(to.map(String::from)),
        has_transcript: Set(false),
        transcript_status: Set("pending".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(state.db()).await.expect("seed cdr")
}

// ---------------------------------------------------------------------------
// GET /api/v1/cdrs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_cdrs_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_cdrs_empty_returns_paginated_envelope() {
    let (state, token) = test_state_with_api_key("cdr-list-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["items"].is_array());
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 20);
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn list_cdrs_returns_seeded_rows() {
    let (state, token) = test_state_with_api_key("cdr-list-seeded").await;
    seed_cdr(
        &state,
        "call-001",
        "inbound",
        "completed",
        Some("+14155550001"),
        Some("+14155550002"),
    )
    .await;
    seed_cdr(
        &state,
        "call-002",
        "outbound",
        "failed",
        Some("+14155550003"),
        Some("+14155550004"),
    )
    .await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn list_cdrs_filters_by_direction() {
    let (state, token) = test_state_with_api_key("cdr-filter-direction").await;
    seed_cdr(&state, "call-a", "inbound", "completed", None, None).await;
    seed_cdr(&state, "call-b", "outbound", "completed", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs?direction=inbound")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["direction"], "inbound");
}

#[tokio::test]
async fn list_cdrs_filters_by_status_and_pagination() {
    let (state, token) = test_state_with_api_key("cdr-filter-status").await;
    for i in 0..5 {
        seed_cdr(
            &state,
            &format!("call-{i:03}"),
            "inbound",
            "completed",
            None,
            None,
        )
        .await;
    }
    seed_cdr(&state, "call-fail", "inbound", "failed", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs?status=completed&page=2&page_size=2")
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
}

// ---------------------------------------------------------------------------
// GET /api/v1/cdrs/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_cdr_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_cdr_happy_path() {
    let (state, token) = test_state_with_api_key("cdr-get-happy").await;
    let seeded = seed_cdr(
        &state,
        "call-xyz",
        "inbound",
        "completed",
        Some("+14155550010"),
        Some("+14155550011"),
    )
    .await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/cdrs/{}", seeded.id))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["id"], seeded.id);
    assert_eq!(body["call_id"], "call-xyz");
    assert_eq!(body["direction"], "inbound");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["from_number"], "+14155550010");
}

#[tokio::test]
async fn get_cdr_missing_returns_404() {
    let (state, token) = test_state_with_api_key("cdr-get-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/99999")
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
// DELETE /api/v1/cdrs/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_cdr_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/cdrs/1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_cdr_happy_path_returns_204() {
    let (state, token) = test_state_with_api_key("cdr-delete-happy").await;
    let seeded = seed_cdr(&state, "call-del", "inbound", "completed", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/cdrs/{}", seeded.id))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_cdr_missing_returns_404() {
    let (state, token) = test_state_with_api_key("cdr-delete-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/cdrs/99999")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// 501 stubs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cdr_recording_returns_501() {
    let (state, token) = test_state_with_api_key("cdr-recording").await;
    let seeded = seed_cdr(&state, "call-rec", "inbound", "completed", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/cdrs/{}/recording", seeded.id))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_implemented");
    assert_eq!(body["error"], "recording retrieval not implemented");
}

#[tokio::test]
async fn cdr_sip_flow_returns_501() {
    let (state, token) = test_state_with_api_key("cdr-sipflow").await;
    let seeded = seed_cdr(&state, "call-flow", "inbound", "completed", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/cdrs/{}/sip-flow", seeded.id))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_implemented");
    assert_eq!(body["error"], "sip flow retrieval not implemented");
}
