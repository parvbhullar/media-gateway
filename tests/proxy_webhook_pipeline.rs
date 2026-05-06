//! IT-WH end-to-end webhook pipeline tests (Phase 7 Plan 07-05).
//!
//! Exercises the full subscribe → filter → deliver → retry → fallback →
//! cancel path through the live `run_webhook_processor` task spawned at
//! server boot in `src/proxy/server.rs:585`.
//!
//! Cases:
//!  1. call.completed → mock receives signed POST with all 4 headers
//!  2. HMAC signature verifies (recompute via signer::sign and compare)
//!  3. Retry exhausts on 502 → disk fallback file written
//!  4. Permanent fail on 400 → no retry, immediate fallback
//!  5. Cancel-on-delete aborts in-flight retry
//!  6. webhook.test on POST: 200 mock → test_delivery = "succeeded"
//!  7. webhook.test on POST: 500 mock → test_delivery = "failed", row persists
//!  8. transcribe.requested fires when marker dropped
//!  9. Event filter: events=["call.completed"] does NOT receive call.started

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::test_state_with_api_key;

// ─── Mock target server ──────────────────────────────────────────────────

#[derive(Clone)]
struct MockState {
    statuses: Arc<Vec<u16>>,
    hits: Arc<AtomicUsize>,
    last_status: Arc<AtomicU16>,
    captures: Arc<parking_lot::Mutex<Vec<RecordedRequest>>>,
}

#[derive(Clone, Debug)]
struct RecordedRequest {
    headers: std::collections::HashMap<String, String>,
    body: String,
}

async fn mock_handler(
    State(state): State<MockState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let idx = state.hits.fetch_add(1, Ordering::SeqCst);
    let s = state.statuses[idx % state.statuses.len()];
    state.last_status.store(s, Ordering::SeqCst);

    let mut hdr_map = std::collections::HashMap::new();
    for (k, v) in headers.iter() {
        if let Ok(vs) = v.to_str() {
            hdr_map.insert(k.as_str().to_string(), vs.to_string());
        }
    }
    state.captures.lock().push(RecordedRequest {
        headers: hdr_map,
        body: String::from_utf8_lossy(&body).to_string(),
    });
    StatusCode::from_u16(s).unwrap_or(StatusCode::OK)
}

async fn spawn_mock(statuses: Vec<u16>) -> (String, MockState) {
    let mock_state = MockState {
        statuses: Arc::new(statuses),
        hits: Arc::new(AtomicUsize::new(0)),
        last_status: Arc::new(AtomicU16::new(0)),
        captures: Arc::new(parking_lot::Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/", post(mock_handler))
        .with_state(mock_state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{}/", addr), mock_state)
}

// ─── Helpers ─────────────────────────────────────────────────────────────

#[allow(dead_code)]
async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[allow(dead_code)]
async fn create_webhook_via_api(
    state: &rustpbx::app::AppState,
    token: &str,
    name: &str,
    url: &str,
    events: Vec<&str>,
    retry_count: i32,
    timeout_ms: i32,
) -> Value {
    let body = json!({
        "name": name,
        "url": url,
        "secret": "test-secret",
        "events": events,
        "retry_count": retry_count,
        "timeout_ms": timeout_ms,
    });
    let app = rustpbx::app::create_router(state.clone());
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
    assert_eq!(status, StatusCode::CREATED, "create webhook: {:?}", body);
    body
}

/// Insert a webhook row directly into the DB so we can use `127.0.0.1`
/// mock URLs (which the API URL-validator rejects per D-27). Returns the
/// inserted Webhook id.
async fn seed_webhook_direct(
    state: &rustpbx::app::AppState,
    name: &str,
    url: &str,
    events: Vec<&str>,
    retry_count: i32,
    timeout_ms: i32,
) -> String {
    use rustpbx::models::webhooks;
    use sea_orm::{ActiveModelTrait, Set};
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let events_json = serde_json::Value::Array(
        events.into_iter().map(|e| serde_json::Value::String(e.to_string())).collect(),
    );
    let am = webhooks::ActiveModel {
        id: Set(id.clone()),
        name: Set(name.to_string()),
        url: Set(url.to_string()),
        secret: Set("test-secret".to_string()),
        events: Set(events_json),
        description: Set(None),
        is_active: Set(true),
        retry_count: Set(retry_count),
        timeout_ms: Set(timeout_ms),
        created_at: Set(now),
        updated_at: Set(now),
        account_id: Set("root".to_string()),
    };
    am.insert(state.db()).await.expect("insert webhook");
    id
}

async fn delete_webhook_via_api(
    state: &rustpbx::app::AppState,
    token: &str,
    id: &str,
) {
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
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

fn fire_event(state: &rustpbx::app::AppState, event_name: &str) {
    let event = rustpbx::proxy::webhook::WebhookEvent {
        event_id: rustpbx::proxy::webhook::new_event_id(),
        event: event_name.to_string(),
        timestamp: rustpbx::proxy::webhook::current_unix_timestamp(),
        data: json!({"sample": true}),
    };
    let _ = state.webhook_sender().send(event);
}

async fn wait_for_hits(state: &MockState, expected: usize, max_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(max_ms);
    while std::time::Instant::now() < deadline {
        if state.hits.load(Ordering::SeqCst) >= expected {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

// ─── Cases ───────────────────────────────────────────────────────────────

/// 1 + 2. call.completed → mock receives signed POST; HMAC verifies.
/// Covers WH-02 + WH-04 + WH-03 (signature).
#[tokio::test]
async fn it_wh_call_completed_delivers_with_valid_hmac() {
    let (mock_url, mock_state) = spawn_mock(vec![200]).await;
    let (state, _token) = test_state_with_api_key("itwh-deliver").await;

    // Direct DB seed bypasses the API URL validator (which denies 127.0.0.1
    // per D-27). The processor reads from the same row.
    seed_webhook_direct(&state, "deliver", &mock_url, vec!["call.completed"], 3, 5000)
        .await;

    // Give processor time to settle, then fire.
    tokio::time::sleep(Duration::from_millis(100)).await;
    fire_event(&state, "call.completed");

    assert!(wait_for_hits(&mock_state, 1, 3000).await, "timeout waiting for hit");

    let captures = mock_state.captures.lock().clone();
    assert_eq!(captures.len(), 1);
    let req = &captures[0];

    // Required headers.
    assert_eq!(req.headers.get("x-webhook-event").map(String::as_str), Some("call.completed"));
    assert_eq!(req.headers.get("x-webhook-secret").map(String::as_str), Some("test-secret"));
    assert!(req.headers.contains_key("x-webhook-request-id"));
    let sig = req.headers.get("x-webhook-signature").expect("signature");
    assert!(sig.starts_with("t="), "signature: {}", sig);

    // Verify HMAC: recompute via signer and compare v1 component.
    let sig_parts: Vec<&str> = sig.split(',').collect();
    let t_part = sig_parts.iter().find(|p| p.starts_with("t=")).unwrap();
    let timestamp: i64 = t_part.trim_start_matches("t=").parse().unwrap();
    let expected = rustpbx::proxy::webhook::signer::signature_header(
        timestamp,
        &req.body,
        "test-secret",
    );
    assert_eq!(sig, &expected, "HMAC mismatch");
}

/// 3. Retry exhausts on 502 → disk fallback file written.
/// Covers WH-03 (retry + fallback).
#[tokio::test]
async fn it_wh_retry_exhausts_writes_disk_fallback() {
    let (mock_url, mock_state) = spawn_mock(vec![502]).await;
    let (state, _token) = test_state_with_api_key("itwh-retry").await;

    // retry_count=0 keeps the test fast: 1 attempt, immediate fallback.
    seed_webhook_direct(
        &state,
        "retry-mock",
        &mock_url,
        vec!["call.completed"],
        0,
        2000,
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    fire_event(&state, "call.completed");

    assert!(wait_for_hits(&mock_state, 1, 3000).await);
    // Allow a beat for the fallback file write.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let generated_dir = state.config().proxy.generated_dir.clone();
    let failed_dir = std::path::PathBuf::from(&generated_dir)
        .join("webhooks")
        .join("failed");
    assert!(
        failed_dir.exists(),
        "expected fallback dir {:?}",
        failed_dir
    );
    let entries: Vec<_> = std::fs::read_dir(&failed_dir).unwrap().collect();
    assert!(!entries.is_empty(), "expected at least 1 fallback file");
}

/// 4. Permanent fail on 400 → 1 attempt, immediate fallback.
#[tokio::test]
async fn it_wh_permanent_fail_400_immediate_fallback() {
    let (mock_url, mock_state) = spawn_mock(vec![400]).await;
    let (state, _token) = test_state_with_api_key("itwh-perm").await;
    seed_webhook_direct(
        &state,
        "perm",
        &mock_url,
        vec!["call.completed"],
        3,
        2000,
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    fire_event(&state, "call.completed");

    assert!(wait_for_hits(&mock_state, 1, 3000).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Should NOT have retried (4xx except 408/429 = permanent).
    assert_eq!(mock_state.hits.load(Ordering::SeqCst), 1, "no retry on 400");
}

/// 5. Cancel-on-delete: after firing event with 502 mock, DELETE webhook
/// during the retry sleep → no further attempts.
#[tokio::test]
async fn it_wh_delete_cancels_in_flight_retry() {
    let (mock_url, mock_state) = spawn_mock(vec![502]).await;
    let (state, token) = test_state_with_api_key("itwh-cancel").await;
    let id = seed_webhook_direct(
        &state,
        "cancel-target",
        &mock_url,
        vec!["call.completed"],
        3, // 1 + 3 retries
        2000,
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    fire_event(&state, "call.completed");

    // Wait for first hit to land.
    assert!(wait_for_hits(&mock_state, 1, 3000).await);
    let hits_after_first = mock_state.hits.load(Ordering::SeqCst);

    // DELETE mid-retry → cancel registry should fire.
    delete_webhook_via_api(&state, &token, &id).await;

    // Wait long enough that the next retry (after ~750ms-1.25s backoff jitter)
    // would have fired if cancel didn't take. Then assert no progress.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let hits_after_cancel = mock_state.hits.load(Ordering::SeqCst);
    assert_eq!(
        hits_after_cancel, hits_after_first,
        "cancel should have stopped further retries"
    );
}

/// 6. deliver_test_event against 200 mock → Ok(()) (test_delivery=succeeded).
/// We exercise the helper directly because the API URL validator denies
/// loopback URLs and the mock server binds to 127.0.0.1.
#[tokio::test]
async fn it_wh_deliver_test_event_success() {
    let (mock_url, mock_state) = spawn_mock(vec![200]).await;
    use rustpbx::models::webhooks::Model as WhModel;
    let now = chrono::Utc::now();
    let webhook = WhModel {
        id: "wh-ok".to_string(),
        name: "test-ok".to_string(),
        url: mock_url,
        secret: "test-secret".to_string(),
        events: serde_json::json!([]),
        description: None,
        is_active: true,
        retry_count: 0,
        timeout_ms: 2000,
        created_at: now,
        updated_at: now,
        account_id: "root".to_string(),
    };
    let event = rustpbx::proxy::webhook::WebhookEvent {
        event_id: rustpbx::proxy::webhook::new_event_id(),
        event: "webhook.test".to_string(),
        timestamp: rustpbx::proxy::webhook::current_unix_timestamp(),
        data: serde_json::json!({"webhook_id": "wh-ok", "message": "Test event from supersip"}),
    };
    let envelope = serde_json::json!({
        "event_id": event.event_id, "event": event.event,
        "timestamp": event.timestamp, "data": event.data,
    })
    .to_string();
    let client = reqwest::Client::new();
    let res = rustpbx::proxy::webhook::deliver_test_event(
        &webhook, &event, &envelope, &client,
    )
    .await;
    assert!(res.is_ok(), "expected Ok, got {:?}", res);
    assert!(mock_state.hits.load(Ordering::SeqCst) >= 1);
}

/// 7. deliver_test_event against 500 mock → Err with "500" in message
/// (test_delivery=failed). The non-fatal-row-persistence behavior of POST
/// is verified at the lib level (CreateWebhookResponse contract — see
/// handler unit tests); here we focus on the helper's outcome shape.
#[tokio::test]
async fn it_wh_deliver_test_event_failure() {
    let (mock_url, _mock_state) = spawn_mock(vec![500]).await;
    use rustpbx::models::webhooks::Model as WhModel;
    let now = chrono::Utc::now();
    let webhook = WhModel {
        id: "wh-fail".to_string(),
        name: "test-fail".to_string(),
        url: mock_url,
        secret: "test-secret".to_string(),
        events: serde_json::json!([]),
        description: None,
        is_active: true,
        retry_count: 0,
        timeout_ms: 2000,
        created_at: now,
        updated_at: now,
        account_id: "root".to_string(),
    };
    let event = rustpbx::proxy::webhook::WebhookEvent {
        event_id: rustpbx::proxy::webhook::new_event_id(),
        event: "webhook.test".to_string(),
        timestamp: rustpbx::proxy::webhook::current_unix_timestamp(),
        data: serde_json::json!({}),
    };
    let envelope = serde_json::to_string(&serde_json::json!({
        "event_id": event.event_id, "event": event.event,
        "timestamp": event.timestamp, "data": event.data,
    }))
    .unwrap();
    let client = reqwest::Client::new();
    let res = rustpbx::proxy::webhook::deliver_test_event(
        &webhook, &event, &envelope, &client,
    )
    .await;
    let err = res.expect_err("expected Err on 500");
    assert!(err.contains("500"), "err: {}", err);
}

/// 8. transcribe.requested fires when marker is dropped.
/// We exercise the broadcast path directly (the marker emit helper is
/// unit-tested in the lib; here we verify end-to-end deliverability).
#[tokio::test]
async fn it_wh_transcribe_requested_fires() {
    let (mock_url, mock_state) = spawn_mock(vec![200]).await;
    let (state, _token) = test_state_with_api_key("itwh-transcribe").await;
    seed_webhook_direct(
        &state,
        "transcribe-target",
        &mock_url,
        vec!["transcribe.requested"],
        0,
        2000,
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    let event = rustpbx::proxy::webhook::WebhookEvent {
        event_id: rustpbx::proxy::webhook::new_event_id(),
        event: "transcribe.requested".to_string(),
        timestamp: rustpbx::proxy::webhook::current_unix_timestamp(),
        data: json!({
            "session_id": "sess-xyz",
            "recording_path": "/tmp/rec.wav",
            "marker_path": "/tmp/rec.wav.transcribe.marker",
        }),
    };
    let _ = state.webhook_sender().send(event);

    assert!(wait_for_hits(&mock_state, 1, 3000).await);
    let captures = mock_state.captures.lock().clone();
    assert_eq!(captures[0].headers.get("x-webhook-event").map(String::as_str),
               Some("transcribe.requested"));
}

/// 9. Event filter: webhook subscribed only to call.completed does NOT
/// receive call.started.
#[tokio::test]
async fn it_wh_event_filter_excludes_unsubscribed() {
    let (mock_url, mock_state) = spawn_mock(vec![200]).await;
    let (state, _token) = test_state_with_api_key("itwh-filter").await;
    seed_webhook_direct(
        &state,
        "filter-target",
        &mock_url,
        vec!["call.completed"],
        0,
        2000,
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    fire_event(&state, "call.started");

    // Wait briefly — should NOT see any hit.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        mock_state.hits.load(Ordering::SeqCst),
        0,
        "call.started must not deliver to a call.completed-only webhook"
    );

    // Now fire call.completed and confirm it IS delivered.
    fire_event(&state, "call.completed");
    assert!(wait_for_hits(&mock_state, 1, 3000).await);
}
