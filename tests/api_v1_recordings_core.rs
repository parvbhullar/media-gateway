//! Integration tests for /api/v1/recordings core handlers (REC-01..04).
//! Phase 12 Plan 12-02.

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use rustpbx::models::call_record::ActiveModel as CdrAm;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
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

/// Insert a CDR row with a non-null recording_url. Returns the new row id.
async fn seed_cdr_with_recording(db: &DatabaseConnection, recording_url: &str) -> i64 {
    let now = Utc::now();
    let call_id = format!("rec-test-{}", uuid::Uuid::new_v4());
    let am = CdrAm {
        call_id: Set(call_id),
        direction: Set("inbound".to_string()),
        status: Set("answered".to_string()),
        started_at: Set(now),
        ended_at: Set(Some(now)),
        duration_secs: Set(60),
        recording_url: Set(Some(recording_url.to_string())),
        has_transcript: Set(false),
        transcript_status: Set("pending".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(db).await.expect("seed cdr with recording").id
}

/// Insert a CDR row with recording_url = NULL. Returns the new row id.
async fn seed_cdr_without_recording(db: &DatabaseConnection) -> i64 {
    let now = Utc::now();
    let call_id = format!("no-rec-{}", uuid::Uuid::new_v4());
    let am = CdrAm {
        call_id: Set(call_id),
        direction: Set("inbound".to_string()),
        status: Set("answered".to_string()),
        started_at: Set(now),
        ended_at: Set(Some(now)),
        duration_secs: Set(30),
        recording_url: Set(None),
        has_transcript: Set(false),
        transcript_status: Set("pending".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(db).await.expect("seed cdr without recording").id
}

/// Delete a CDR row by id (cleanup after test).
async fn cleanup_cdr(db: &DatabaseConnection, id: i64) {
    rustpbx::models::call_record::Entity::delete_by_id(id)
        .exec(db)
        .await
        .expect("cleanup cdr");
}

// ---------------------------------------------------------------------------
// GET /api/v1/recordings -- auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recordings_list_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/recordings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// GET /api/v1/recordings -- list with seeded recording
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recordings_list_returns_items() {
    let (state, token) = test_state_with_api_key("rec-list-items").await;
    let id = seed_cdr_with_recording(state.db(), "s3://bucket/recording.wav").await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/recordings")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["items"].is_array(), "items must be array");
    assert!(
        body["pagination"]["total"].is_number(),
        "pagination.total must be number"
    );
    assert!(
        body["pagination"]["page"].is_number(),
        "pagination.page must be number"
    );
    assert!(
        body["pagination"]["page_size"].is_number(),
        "pagination.page_size must be number"
    );
    let items = body["items"].as_array().unwrap();
    assert!(
        items.iter().any(|item| item["id"] == id),
        "seeded recording id must appear in items"
    );
    cleanup_cdr(state.db(), id).await;
}

// ---------------------------------------------------------------------------
// GET /api/v1/recordings -- null recording_url rows must be excluded
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recordings_list_filters_nulls() {
    let (state, token) = test_state_with_api_key("rec-list-nulls").await;
    let id = seed_cdr_without_recording(state.db()).await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/recordings")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert!(
        !items.iter().any(|item| item["id"] == id),
        "CDR with null recording_url must NOT appear in /recordings list"
    );
    cleanup_cdr(state.db(), id).await;
}

// ---------------------------------------------------------------------------
// GET /api/v1/recordings/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recordings_get_returns_view() {
    let (state, token) = test_state_with_api_key("rec-get-view").await;
    let id = seed_cdr_with_recording(state.db(), "s3://bucket/test.wav").await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/recordings/{id}"))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["id"], id, "id must match");
    assert!(
        body["recording_url"].as_str().is_some(),
        "recording_url must be non-null string"
    );
    let storage = body["recording_storage"].as_str().unwrap();
    assert!(
        storage == "local" || storage == "remote",
        "recording_storage must be 'local' or 'remote', got: {storage}"
    );
    cleanup_cdr(state.db(), id).await;
}

#[tokio::test]
async fn recordings_get_missing_returns_404() {
    let (state, token) = test_state_with_api_key("rec-get-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/recordings/9999999")
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

#[tokio::test]
async fn recordings_get_cdr_without_recording_returns_404() {
    let (state, token) = test_state_with_api_key("rec-get-no-url").await;
    let id = seed_cdr_without_recording(state.db()).await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/recordings/{id}"))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    cleanup_cdr(state.db(), id).await;
}

// ---------------------------------------------------------------------------
// GET /api/v1/recordings/{id}/download -- remote URL redirects
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recordings_download_remote_redirects() {
    let (state, token) = test_state_with_api_key("rec-download-remote").await;
    let url = "https://cdn.example.com/recording.wav";
    let id = seed_cdr_with_recording(state.db(), url).await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/recordings/{id}/download"))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FOUND,
        "remote url must redirect with 302"
    );
    let location = resp
        .headers()
        .get("location")
        .expect("Location header must be present")
        .to_str()
        .unwrap();
    assert_eq!(location, url, "Location must match the recording_url");
    cleanup_cdr(state.db(), id).await;
}

// ---------------------------------------------------------------------------
// DELETE /api/v1/recordings/{id} -- clears recording_url, CDR row survives
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recordings_delete_clears_url_keeps_row() {
    let (state, token) = test_state_with_api_key("rec-delete-keeps-row").await;
    let id = seed_cdr_with_recording(state.db(), "s3://bucket/del.wav").await;

    // DELETE recording -- expect 204
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/recordings/{id}"))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "DELETE must return 204");

    // CDR row must still exist -- GET /api/v1/cdrs/{id} returns 200
    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/cdrs/{id}"))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp2.status(),
        StatusCode::OK,
        "CDR row must survive recording delete"
    );

    // /recordings/{id} must now return 404 (recording_url cleared)
    let app3 = rustpbx::app::create_router(state.clone());
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/recordings/{id}"))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp3.status(),
        StatusCode::NOT_FOUND,
        "/recordings/{id} must 404 after recording_url is cleared"
    );
    // Cleanup row
    rustpbx::models::call_record::Entity::delete_by_id(id)
        .exec(state.db())
        .await
        .ok();
}
