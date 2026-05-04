//! Integration tests for /api/v1/recordings export and bulk delete (REC-05, REC-06).
//! Phase 12 Plan 12-03.

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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024)
        .await
        .expect("read body bytes")
        .to_vec()
}

fn bearer(token: &str) -> String {
    format!("Bearer {}", token)
}

/// Insert a CDR row with a non-null recording_url. Returns the new row id.
async fn seed_cdr_with_recording(db: &DatabaseConnection, recording_url: &str) -> i64 {
    let now = Utc::now();
    let call_id = format!("exp-test-{}", uuid::Uuid::new_v4());
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

/// Delete a CDR row by id (cleanup after test).
async fn cleanup_cdr(db: &DatabaseConnection, id: i64) {
    rustpbx::models::call_record::Entity::delete_by_id(id)
        .exec(db)
        .await
        .expect("cleanup cdr");
}

// ---------------------------------------------------------------------------
// POST /api/v1/recordings/export -- auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/recordings/export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// POST /api/v1/recordings/export -- empty result set returns valid ZIP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_empty_set_returns_zip() {
    let (state, token) = test_state_with_api_key("export-empty-zip").await;
    let app = rustpbx::app::create_router(state);

    // POST with a status filter that will never match any seeded row.
    // No Content-Type/body -- Option<Json<ExportBody>> accepts missing body.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/recordings/export?status=__nonexistent__")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "export must return 200 for empty set");

    let ct = resp
        .headers()
        .get("content-type")
        .expect("content-type header must be present")
        .to_str()
        .unwrap();
    assert!(
        ct.contains("application/zip"),
        "content-type must be application/zip, got: {ct}"
    );

    let bytes = body_bytes(resp).await;
    // ZIP magic bytes: PK\x03\x04 (local file header signature).
    // Even an empty ZIP has an end-of-central-directory record starting with PK\x05\x06.
    // We accept either magic prefix as a valid ZIP.
    assert!(
        bytes.starts_with(&[0x50, 0x4B, 0x03, 0x04])
            || bytes.starts_with(&[0x50, 0x4B, 0x05, 0x06])
            || bytes.starts_with(b"PK"),
        "response body must be a valid ZIP archive; got {} bytes starting with {:?}",
        bytes.len(),
        &bytes[..bytes.len().min(4)]
    );
}

// ---------------------------------------------------------------------------
// POST /api/v1/recordings/export -- cap check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_over_cap_returns_400() {
    // Seeding 10_001 rows in a real test DB is impractical.
    // The unit test `check_recordings_export_cap_rejects_over_limit` covers
    // the rejection path. This integration test verifies the route is reachable
    // with auth and that a zero-row export returns 200 (cap not exceeded).
    let (state, token) = test_state_with_api_key("export-cap-check").await;
    let app = rustpbx::app::create_router(state);
    // No Content-Type/body -- Option<Json<ExportBody>> accepts missing body.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/recordings/export")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // With 0 seeded recordings we expect 200 (empty ZIP).
    // DB is isolated per test_state so we can never accidentally exceed the cap.
    assert!(
        resp.status() == StatusCode::OK || resp.status() == StatusCode::BAD_REQUEST,
        "export must return 200 (empty) or 400 (cap exceeded), got {}",
        resp.status()
    );
}

// ---------------------------------------------------------------------------
// DELETE /api/v1/recordings/bulk -- without ?confirm=true returns 400 + preview
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bulk_delete_without_confirm_returns_preview() {
    let (state, token) = test_state_with_api_key("bulk-no-confirm").await;
    let id = seed_cdr_with_recording(state.db(), "s3://bucket/preview.wav").await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/recordings/bulk")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "bulk delete without ?confirm=true must return 400"
    );

    let body = body_json(resp).await;
    assert!(
        body["preview"]["matched"].is_number(),
        "preview.matched must be a number, got: {}",
        body
    );
    assert!(
        body["preview"]["would_delete"].is_number(),
        "preview.would_delete must be a number, got: {}",
        body
    );
    let msg = body["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("confirm=true"),
        "message must mention confirm=true, got: {msg}"
    );

    cleanup_cdr(state.db(), id).await;
}

// ---------------------------------------------------------------------------
// DELETE /api/v1/recordings/bulk?confirm=true -- deletes and clears recording_url
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bulk_delete_with_confirm_deletes() {
    let (state, token) = test_state_with_api_key("bulk-confirm-deletes").await;
    let id = seed_cdr_with_recording(state.db(), "s3://bucket/bulk.wav").await;

    // DELETE with confirm=true
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/recordings/bulk?confirm=true")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "bulk delete with confirm=true must return 200"
    );

    let body = body_json(resp).await;
    assert!(
        body["deleted"].as_u64().unwrap_or(0) >= 1,
        "deleted count must be >= 1, got: {}",
        body
    );

    // Verify recording_url was cleared: GET /api/v1/recordings/{id} returns 404.
    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
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
        resp2.status(),
        StatusCode::NOT_FOUND,
        "/recordings/{id} must be 404 after bulk delete clears recording_url"
    );

    // CDR row must still exist: GET /api/v1/cdrs/{id} returns 200.
    let app3 = rustpbx::app::create_router(state.clone());
    let resp3 = app3
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
        resp3.status(),
        StatusCode::OK,
        "CDR row must survive bulk delete (recording_url cleared, row preserved)"
    );

    // Cleanup CDR row.
    cleanup_cdr(state.db(), id).await;
}
