//! Integration tests for `/api/v1/trunks/{name}/capacity` (Phase 5
//! Plan 05-02 — TSUB-04 CRUD half / TSUB-07 response shape).
//!
//! Matrix per `05-CONTEXT.md` D-22 + IT-01:
//!
//!   1. 401 without Bearer token (auth gate)
//!   2. GET on a missing parent trunk returns 404
//!   3. GET on a trunk with no capacity row returns
//!      `{max_calls:null, max_cps:null, current_active:0, current_cps_rate:0}` (D-04)
//!   4. PUT happy round-trip — values persist and reappear via GET
//!   5. PUT both null = unlimited persists null
//!   6. PUT max_calls=0 → 400 with substring `use null for unlimited` (D-05)
//!   7. PUT max_cps=0 → 400 with the same substring (D-05)
//!   8. PUT replaces existing row (idempotent upsert)
//!   9. PUT on a missing parent trunk returns 404
//!
//! Fixture helpers mirror `tests/api_v1_trunk_credentials.rs`.

use axum::{
    body::Body,
    http::{Request, header},
};
use chrono::Utc;
use rustpbx::models::sip_trunk::{
    self, SipTransport, SipTrunkDirection, SipTrunkStatus,
};
use rustpbx::models::trunk_group::{self, TrunkGroupDistributionMode};
use rustpbx::models::trunk_group_member;
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

// ─── Fixture helpers ─────────────────────────────────────────────────────

async fn insert_trunk(
    state: &rustpbx::app::AppState,
    name: &str,
) -> sip_trunk::Model {
    let now = Utc::now();
    let am = sip_trunk::ActiveModel {
        name: Set(name.to_string()),
        display_name: Set(Some(format!("{} display", name))),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        sip_server: Set(Some("sip.example.com:5060".to_string())),
        sip_transport: Set(SipTransport::Udp),
        is_active: Set(true),
        register_enabled: Set(false),
        rewrite_hostport: Set(true),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(state.db()).await.expect("insert trunk")
}

async fn insert_trunk_group(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    members: &[&str],
) -> trunk_group::Model {
    let now = Utc::now();
    let group_am = trunk_group::ActiveModel {
        name: Set(name.to_string()),
        display_name: Set(Some(format!("{} display", name))),
        direction: Set(SipTrunkDirection::Outbound),
        distribution_mode: Set(TrunkGroupDistributionMode::RoundRobin),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let group = group_am.insert(db).await.expect("insert trunk group");

    for (i, gw_name) in members.iter().enumerate() {
        let member_am = trunk_group_member::ActiveModel {
            trunk_group_id: Set(group.id),
            gateway_name: Set(gw_name.to_string()),
            weight: Set(100),
            priority: Set(0),
            position: Set(i as i32),
            ..Default::default()
        };
        member_am
            .insert(db)
            .await
            .expect("insert trunk group member");
    }

    group
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

fn auth_header(token: &str) -> (axum::http::HeaderName, String) {
    (header::AUTHORIZATION, format!("Bearer {}", token))
}

// =========================================================================
// 1. Auth (401)
// =========================================================================

#[tokio::test]
async fn get_capacity_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/any-tg/capacity")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
}

// =========================================================================
// 2. GET parent missing → 404
// =========================================================================

#[tokio::test]
async fn get_capacity_parent_missing_returns_404() {
    let (state, token) =
        test_state_with_api_key("cap-parent-missing").await;

    let app = rustpbx::app::create_router(state);
    let (h, v) = auth_header(&token);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/no_such_trunk/capacity")
                .header(h, v)
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
// 3. GET no row → defaults (D-04)
// =========================================================================

#[tokio::test]
async fn get_capacity_no_row_returns_defaults() {
    let (state, token) =
        test_state_with_api_key("cap-defaults").await;
    insert_trunk(&state, "gw-cap-d").await;
    insert_trunk_group(state.db(), "tg-cap-d", &["gw-cap-d"]).await;

    let app = rustpbx::app::create_router(state);
    let (h, v) = auth_header(&token);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-cap-d/capacity")
                .header(h, v)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["max_calls"].is_null(), "expected null, got {:?}", body["max_calls"]);
    assert!(body["max_cps"].is_null(), "expected null, got {:?}", body["max_cps"]);
    assert_eq!(body["current_active"], 0);
    assert_eq!(body["current_cps_rate"], 0);
}

// =========================================================================
// 4. PUT happy + GET round-trip
// =========================================================================

#[tokio::test]
async fn put_capacity_happy_round_trips_via_get() {
    let (state, token) =
        test_state_with_api_key("cap-put-happy").await;
    insert_trunk(&state, "gw-cap-h").await;
    insert_trunk_group(state.db(), "tg-cap-h", &["gw-cap-h"]).await;

    // PUT
    let app = rustpbx::app::create_router(state.clone());
    let (h, v) = auth_header(&token);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-cap-h/capacity")
                .header(h, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"max_calls": 100, "max_cps": 10}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["max_calls"], 100);
    assert_eq!(body["max_cps"], 10);
    assert_eq!(body["current_active"], 0);
    assert_eq!(body["current_cps_rate"], 0);

    // GET round-trip
    let app2 = rustpbx::app::create_router(state);
    let (h, v) = auth_header(&token);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-cap-h/capacity")
                .header(h, v)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::OK);
    let body2 = body_json(resp2).await;
    assert_eq!(body2["max_calls"], 100);
    assert_eq!(body2["max_cps"], 10);
    assert_eq!(body2["current_active"], 0);
    assert_eq!(body2["current_cps_rate"], 0);
}

// =========================================================================
// 5. PUT both null = unlimited
// =========================================================================

#[tokio::test]
async fn put_capacity_both_null_persists_unlimited() {
    let (state, token) =
        test_state_with_api_key("cap-put-null").await;
    insert_trunk(&state, "gw-cap-n").await;
    insert_trunk_group(state.db(), "tg-cap-n", &["gw-cap-n"]).await;

    let app = rustpbx::app::create_router(state.clone());
    let (h, v) = auth_header(&token);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-cap-n/capacity")
                .header(h, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"max_calls": null, "max_cps": null}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    // GET shows both null
    let app2 = rustpbx::app::create_router(state);
    let (h, v) = auth_header(&token);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-cap-n/capacity")
                .header(h, v)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::OK);
    let body = body_json(resp2).await;
    assert!(body["max_calls"].is_null());
    assert!(body["max_cps"].is_null());
}

// =========================================================================
// 6. PUT max_calls=0 → 400 (D-05)
// =========================================================================

#[tokio::test]
async fn put_capacity_zero_max_calls_returns_400() {
    let (state, token) =
        test_state_with_api_key("cap-put-zero-calls").await;
    insert_trunk(&state, "gw-cap-z1").await;
    insert_trunk_group(state.db(), "tg-cap-z1", &["gw-cap-z1"]).await;

    let app = rustpbx::app::create_router(state);
    let (h, v) = auth_header(&token);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-cap-z1/capacity")
                .header(h, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"max_calls": 0}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("use null for unlimited"),
        "expected 'use null for unlimited' in error, got: {}",
        msg
    );
}

// =========================================================================
// 7. PUT max_cps=0 → 400 (D-05)
// =========================================================================

#[tokio::test]
async fn put_capacity_zero_max_cps_returns_400() {
    let (state, token) =
        test_state_with_api_key("cap-put-zero-cps").await;
    insert_trunk(&state, "gw-cap-z2").await;
    insert_trunk_group(state.db(), "tg-cap-z2", &["gw-cap-z2"]).await;

    let app = rustpbx::app::create_router(state);
    let (h, v) = auth_header(&token);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-cap-z2/capacity")
                .header(h, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"max_cps": 0}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("use null for unlimited"),
        "expected 'use null for unlimited' in error, got: {}",
        msg
    );
}

// =========================================================================
// 8. PUT replaces existing (idempotent upsert)
// =========================================================================

#[tokio::test]
async fn put_capacity_replaces_existing_row() {
    let (state, token) =
        test_state_with_api_key("cap-put-replace").await;
    insert_trunk(&state, "gw-cap-r").await;
    insert_trunk_group(state.db(), "tg-cap-r", &["gw-cap-r"]).await;

    // PUT 100/10
    let app = rustpbx::app::create_router(state.clone());
    let (h, v) = auth_header(&token);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-cap-r/capacity")
                .header(h, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"max_calls": 100, "max_cps": 10}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    // PUT 50/null
    let app2 = rustpbx::app::create_router(state.clone());
    let (h, v) = auth_header(&token);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-cap-r/capacity")
                .header(h, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"max_calls": 50, "max_cps": null}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::OK);

    // GET shows 50 / null
    let app3 = rustpbx::app::create_router(state);
    let (h, v) = auth_header(&token);
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-cap-r/capacity")
                .header(h, v)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp3.status(), axum::http::StatusCode::OK);
    let body = body_json(resp3).await;
    assert_eq!(body["max_calls"], 50);
    assert!(body["max_cps"].is_null());
}

// =========================================================================
// 9. PUT parent missing → 404
// =========================================================================

#[tokio::test]
async fn put_capacity_parent_missing_returns_404() {
    let (state, token) =
        test_state_with_api_key("cap-put-parent-missing").await;

    let app = rustpbx::app::create_router(state);
    let (h, v) = auth_header(&token);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/no_such_trunk/capacity")
                .header(h, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"max_calls": 100, "max_cps": 10}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_found");
}
