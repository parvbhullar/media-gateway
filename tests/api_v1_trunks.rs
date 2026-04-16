//! Integration tests for `/api/v1/trunks` (Phase 2 Plan 02-01 Task 3).
//!
//! Tests cover 401-without-auth, happy-path list/get, 404-missing, and
//! deliberate 501 stubs for write endpoints. The 501 tests are deleted
//! in Plan 02-02 when write handlers land.

use axum::{
    body::Body,
    http::{Request, header},
};
use chrono::Utc;
use rustpbx::models::sip_trunk::{self, SipTrunkDirection, SipTrunkStatus, SipTransport};
use rustpbx::models::trunk_group::{self, TrunkGroupDistributionMode};
use rustpbx::models::trunk_group_member;
use sea_orm::{ActiveModelTrait, Set};
use serde_json::Value;
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

/// Insert a gateway (sip_trunk) so we can reference it as a trunk group member.
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

/// Insert a trunk group and its members, returning the group model.
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
        member_am.insert(db).await.expect("insert trunk group member");
    }

    group
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

// ---------------------------------------------------------------------------
// Auth tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_trunks_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn get_trunk_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

// ---------------------------------------------------------------------------
// Happy-path read tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_trunks_returns_empty_paginated_response() {
    let (state, token) = test_state_with_api_key("trunks-list-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 20);
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn list_trunks_returns_seeded_group_with_members() {
    let (state, token) = test_state_with_api_key("trunks-list-seeded").await;
    insert_trunk(&state, "gw-alpha").await;
    insert_trunk_group(state.db(), "tg-primary", &["gw-alpha"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "tg-primary");
    assert_eq!(items[0]["distribution_mode"], "round_robin");
    assert_eq!(items[0]["direction"], "outbound");
    assert_eq!(items[0]["is_active"], true);
    let members = items[0]["members"].as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["gateway_name"], "gw-alpha");
}

#[tokio::test]
async fn get_trunk_returns_seeded_group() {
    let (state, token) = test_state_with_api_key("trunks-get-seeded").await;
    insert_trunk(&state, "gw-beta").await;
    insert_trunk_group(state.db(), "tg-secondary", &["gw-beta"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-secondary")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["name"], "tg-secondary");
    assert_eq!(body["distribution_mode"], "round_robin");
    assert_eq!(body["direction"], "outbound");
    assert_eq!(body["is_active"], true);
    assert!(body["display_name"].is_string());
    assert!(body["created_at"].is_string());
    assert!(body["updated_at"].is_string());
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["gateway_name"], "gw-beta");
    assert_eq!(members[0]["weight"], 100);
    assert_eq!(members[0]["priority"], 0);
    assert_eq!(members[0]["position"], 0);
}

// ---------------------------------------------------------------------------
// 404 test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_trunk_missing_returns_404() {
    let (state, token) = test_state_with_api_key("trunks-get-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/does-not-exist")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_found");
}

// ---------------------------------------------------------------------------
// 501 stub tests — deleted in Plan 02-02 when write handlers land
// TODO(plan-02-02): remove these tests and replace with real write tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_trunk_returns_501_in_plan_01() {
    let (state, token) = test_state_with_api_key("trunks-create-501").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"new-tg"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 501);
}

#[tokio::test]
async fn delete_trunk_returns_501_in_plan_01() {
    let (state, token) = test_state_with_api_key("trunks-delete-501").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/trunks/some-tg")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 501);
}
