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

// ---------------------------------------------------------------------------
// GET /api/v1/cdrs/search  (Phase 11, Plan 11-02 — CDR-05)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/search")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn search_filters_by_status_and_returns_summary() {
    let (state, token) = test_state_with_api_key("cdr-search-status").await;
    seed_cdr(&state, "c1", "inbound", "answered", Some("100"), Some("200")).await;
    seed_cdr(&state, "c2", "inbound", "answered", Some("100"), Some("201")).await;
    seed_cdr(&state, "c3", "outbound", "no_answer", Some("200"), Some("300")).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/search?status=answered")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["total"], 2);
    assert_eq!(body["summary"]["by_status"]["answered"], 2);
    assert_eq!(body["summary"]["total"], 2);
}

#[tokio::test]
async fn search_summary_breakdown_sums_to_total() {
    let (state, token) = test_state_with_api_key("cdr-search-sum").await;
    seed_cdr(&state, "s1", "inbound", "answered", None, None).await;
    seed_cdr(&state, "s2", "inbound", "answered", None, None).await;
    seed_cdr(&state, "s3", "outbound", "no_answer", None, None).await;
    seed_cdr(&state, "s4", "outbound", "failed", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/search")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let by_status = body["summary"]["by_status"].as_object().unwrap();
    let sum: u64 = by_status
        .values()
        .map(|v| v.as_u64().unwrap_or(0))
        .sum();
    // 2 answered + 1 no_answer + 1 failed = 4 (busy = 0; total includes
    // only the four tracked statuses)
    assert_eq!(sum, body["summary"]["total"].as_u64().unwrap());
    assert_eq!(sum, 4);
}

// ---------------------------------------------------------------------------
// GET /api/v1/cdrs/recent  (Phase 11, Plan 11-02 — CDR-06)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recent_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/recent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recent_returns_50_most_recent() {
    let (state, token) = test_state_with_api_key("cdr-recent-50").await;
    for i in 0..60 {
        seed_cdr(
            &state,
            &format!("recent-{i:03}"),
            "inbound",
            "answered",
            None,
            None,
        )
        .await;
    }

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/recent")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 50);
}

#[tokio::test]
async fn recent_honors_limit_param() {
    let (state, token) = test_state_with_api_key("cdr-recent-limit").await;
    for i in 0..15 {
        seed_cdr(
            &state,
            &format!("rl-{i:03}"),
            "inbound",
            "answered",
            None,
            None,
        )
        .await;
    }

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/recent?limit=10")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 10);
    assert_eq!(body["page_size"], 10);
}

#[tokio::test]
async fn recent_caps_limit_at_500() {
    let (state, token) = test_state_with_api_key("cdr-recent-cap").await;
    // Seed only a few rows; we're verifying that the page_size in the
    // response envelope is clamped to <= 500 even when ?limit=9999.
    seed_cdr(&state, "rc-1", "inbound", "answered", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/recent?limit=9999")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["page_size"], 500);
}

// ---------------------------------------------------------------------------
// GET /api/v1/cdrs/export  (Phase 11, Plan 11-02 — CDR-07)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn export_streams_csv_with_header_row() {
    let (state, token) = test_state_with_api_key("cdr-export-csv").await;
    seed_cdr(&state, "ex-1", "inbound", "answered", Some("100"), Some("200")).await;
    seed_cdr(&state, "ex-2", "inbound", "answered", Some("101"), Some("201")).await;
    seed_cdr(&state, "ex-3", "outbound", "failed", Some("102"), Some("202")).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/export")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let cd = resp
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(ct, "text/csv; charset=utf-8");
    assert!(
        cd.starts_with("attachment; filename=\"cdrs-"),
        "unexpected content-disposition: {}",
        cd
    );

    let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    // header + 3 data rows
    assert_eq!(lines.len(), 4, "expected header + 3 data rows: {:?}", lines);
    assert!(
        lines[0].starts_with("call_id,direction,status,"),
        "header mismatch: {}",
        lines[0]
    );
}

#[tokio::test]
async fn export_filters_by_status() {
    let (state, token) = test_state_with_api_key("cdr-export-filter").await;
    seed_cdr(&state, "ef-1", "inbound", "answered", None, None).await;
    seed_cdr(&state, "ef-2", "inbound", "answered", None, None).await;
    seed_cdr(&state, "ef-3", "outbound", "failed", None, None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/export?status=answered")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    // header + 2 answered rows
    assert_eq!(lines.len(), 3, "expected header + 2 data rows: {:?}", lines);
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
