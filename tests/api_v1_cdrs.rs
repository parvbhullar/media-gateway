//! Integration tests for `/api/v1/cdrs` (Phase 1, Plan 01-03).

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::{Duration as ChronoDuration, Utc};
use rustpbx::models::call_record::{self, ActiveModel as CdrAm};
use rustpbx::models::sip_trunk::{
    self, SipTransport, SipTrunkConfig, SipTrunkDirection, SipTrunkStatus,
};
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
async fn cdr_recording_route_is_live_not_found_in_test_env() {
    // enrich-cdr-api: the 501 stub is gone; the route delegates to the shared
    // console recording streamer. The seeded row has no recording file / S3 /
    // sipflow capture in the test env, so it resolves to NOT_FOUND (or 503 with
    // no console state), never NOT_IMPLEMENTED.
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
    assert_ne!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    assert!(
        matches!(
            resp.status(),
            StatusCode::NOT_FOUND | StatusCode::SERVICE_UNAVAILABLE
        ),
        "expected 404/503 for a recording-less row, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn cdr_sip_flow_route_is_live_no_capture_in_test_env() {
    // enrich-cdr-api: the 501 stub is gone; the route delegates to the shared
    // console sip-flow builder. The test AppState has no SIP/sipflow backend
    // (`with_skip_sip_bind`), so a real capture can't be returned — it resolves
    // to NOT_FOUND (no SIP server / flow backend) or SERVICE_UNAVAILABLE (no
    // console state), never NOT_IMPLEMENTED.
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
    assert_ne!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    assert!(
        matches!(
            resp.status(),
            StatusCode::NOT_FOUND | StatusCode::SERVICE_UNAVAILABLE
        ),
        "expected 404/503 in a backend-less test env, got {}",
        resp.status()
    );
}

// ---------------------------------------------------------------------------
// 503 attribution — failure_source exposure + filter
// ---------------------------------------------------------------------------

async fn seed_cdr_with_failure_source(
    state: &rustpbx::app::AppState,
    call_id: &str,
    status_code: i16,
    failure_source: &str,
) {
    let now = Utc::now();
    let am = CdrAm {
        call_id: Set(call_id.to_string()),
        direction: Set("outbound".to_string()),
        status: Set("failed".to_string()),
        status_code: Set(Some(status_code)),
        failure_source: Set(Some(failure_source.to_string())),
        started_at: Set(now),
        ended_at: Set(Some(now)),
        duration_secs: Set(0),
        has_transcript: Set(false),
        transcript_status: Set("none".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(state.db()).await.expect("seed cdr failure_source");
}

#[tokio::test]
async fn list_cdrs_exposes_and_filters_failure_source() {
    let (state, token) = test_state_with_api_key("cdr-failure-source").await;
    // Two 503s: one generated by our SBC, one relayed from the carrier.
    seed_cdr_with_failure_source(&state, "call-sbc", 503, "sbc").await;
    seed_cdr_with_failure_source(&state, "call-carrier", 503, "upstream").await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs?failure_source=sbc")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    // Filter narrows to the SBC-side 503 only...
    assert_eq!(body["total"], 1, "failure_source=sbc must exclude the carrier 503");
    // ...and the view exposes the attribution + the SIP code.
    assert_eq!(body["items"][0]["failure_source"], "sbc");
    assert_eq!(body["items"][0]["status_code"], 503);
    assert_eq!(body["items"][0]["call_id"], "call-sbc");
}

// ---------------------------------------------------------------------------
// GET /api/v1/cdrs/summary — aggregated counts (per status / per source / per
// trunk via filter)
// ---------------------------------------------------------------------------

async fn insert_trunk(state: &rustpbx::app::AppState, name: &str) -> sip_trunk::Model {
    let now = Utc::now();
    let cfg = SipTrunkConfig {
        sip_server: Some("sip.example.com:5060".to_string()),
        sip_transport: SipTransport::Udp,
        register_enabled: false,
        rewrite_hostport: true,
        ..Default::default()
    };
    let am = sip_trunk::ActiveModel {
        name: Set(name.to_string()),
        kind: Set("sip".into()),
        display_name: Set(Some(name.to_string())),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        is_active: Set(true),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        kind_config: Set(serde_json::to_value(&cfg).unwrap()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(state.db()).await.expect("insert trunk")
}

async fn seed_cdr_full(
    state: &rustpbx::app::AppState,
    call_id: &str,
    status: &str,
    status_code: i16,
    failure_source: Option<&str>,
    sip_trunk_id: Option<i64>,
) {
    let now = Utc::now();
    let am = CdrAm {
        call_id: Set(call_id.to_string()),
        direction: Set("outbound".to_string()),
        status: Set(status.to_string()),
        status_code: Set(Some(status_code)),
        failure_source: Set(failure_source.map(String::from)),
        sip_trunk_id: Set(sip_trunk_id),
        started_at: Set(now),
        ended_at: Set(Some(now)),
        duration_secs: Set(0),
        has_transcript: Set(false),
        transcript_status: Set("none".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(state.db()).await.expect("seed cdr full");
}

/// Count for a SIP code in the report's `by_status_code` array, or 0 if absent.
fn code_count(body: &Value, code: i64) -> i64 {
    body["by_status_code"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .find(|e| e["code"] == code)
                .and_then(|e| e["count"].as_i64())
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn cdr_summary_groups_outcome_and_status() {
    let (state, token) = test_state_with_api_key("cdr-summary").await;
    // outcome_kind is unset on seed → recomputed from status_code:
    // 200→OK, 503→SYS, 486→USR.
    seed_cdr_full(&state, "c1", "completed", 200, None, None).await;
    seed_cdr_full(&state, "c2", "failed", 503, Some("sbc"), None).await;
    seed_cdr_full(&state, "c3", "failed", 503, Some("upstream"), None).await;
    seed_cdr_full(&state, "c4", "failed", 486, Some("upstream"), None).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cdrs/summary")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    // OK/USR/SYS partition the total exactly.
    assert_eq!(body["summary"]["total"], 4);
    assert_eq!(body["summary"]["ok"], 1); // the 200
    assert_eq!(body["summary"]["usr"], 1); // the 486
    assert_eq!(body["summary"]["sys"], 2); // the two 503s
    // sbc + upstream legs flagged (c2,c3,c4) = 3.
    assert_eq!(body["summary"]["upstream_flagged"], 3);
    // SIP-code breakdown is an array of {code, kind, count, remark}.
    assert_eq!(code_count(&body, 503), 2);
    assert_eq!(code_count(&body, 486), 1);
    assert_eq!(code_count(&body, 200), 1);
    assert_eq!(body["group_by"], "day");
}

#[tokio::test]
async fn cdr_summary_filters_by_trunk() {
    let (state, token) = test_state_with_api_key("cdr-summary-trunk").await;
    let trunk_a = insert_trunk(&state, "summary-trunk-a").await;
    let trunk_b = insert_trunk(&state, "summary-trunk-b").await;
    seed_cdr_full(&state, "t1", "completed", 200, None, Some(trunk_a.id)).await;
    seed_cdr_full(&state, "t2", "failed", 503, Some("sbc"), Some(trunk_a.id)).await;
    seed_cdr_full(&state, "t3", "failed", 486, Some("upstream"), Some(trunk_b.id)).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(&format!("/api/v1/cdrs/summary?sip_trunk_id={}", trunk_a.id))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["summary"]["total"], 2, "only trunk_a's calls");
    assert_eq!(code_count(&body, 503), 1);
    assert_eq!(code_count(&body, 486), 0, "trunk_b's 486 excluded");
}

// ---------------------------------------------------------------------------
// GET /api/v1/cdrs — trunk filter + enriched item shape (enrich-cdr-api)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_cdrs_filters_by_trunk_and_exposes_enriched_fields() {
    let (state, token) = test_state_with_api_key("cdr-list-trunk").await;
    let trunk_a = insert_trunk(&state, "list-trunk-a").await;
    let trunk_b = insert_trunk(&state, "list-trunk-b").await;
    // outcome_kind is unset on seed → recomputed from status_code: 503 → SYS.
    seed_cdr_full(&state, "lt1", "failed", 503, Some("sbc"), Some(trunk_a.id)).await;
    seed_cdr_full(&state, "lt2", "completed", 200, None, Some(trunk_b.id)).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(&format!("/api/v1/cdrs?sip_trunk_id={}", trunk_a.id))
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    // Filter narrows to trunk_a only.
    assert_eq!(body["total"], 1);
    let item = &body["items"][0];
    assert_eq!(item["sip_trunk_id"], trunk_a.id);
    assert_eq!(item["call_id"], "lt1");
    // Enriched fields: outcome class + nested recording/sipflow.
    assert_eq!(item["outcome_kind"], "SYS");
    assert_eq!(item["recording"]["available"], false);
    assert!(item["recording"]["url"].is_null());
    assert_eq!(item["sipflow"]["available"], false);
    assert!(item["sipflow"]["url"].is_null());
    // No Q.850 recorded on this seed → q850 is null.
    assert!(item["q850"].is_null());
}
