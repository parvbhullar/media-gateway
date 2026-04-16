//! Integration tests for `/api/v1/trunks` (Phase 2).
//!
//! Plan 02-01 shipped read-only tests (8). Plan 02-02 replaces the two
//! 501-stub tests and adds the full write-path matrix covering TRK-02
//! (happy paths), TRK-03 (validation 400s), TRK-04 (engagement 409s),
//! and auth 401s.
//!
//! NOTE: atomicity verified by inspection of the tx boundary in
//! create_trunk / update_trunk / delete_trunk.

use axum::{
    body::Body,
    http::{Request, header},
};
use chrono::Utc;
use rustpbx::models::did;
use rustpbx::models::routing;
use rustpbx::models::sip_trunk::{
    self, SipTrunkDirection, SipTrunkStatus, SipTransport,
};
use rustpbx::models::trunk_group::{self, TrunkGroupDistributionMode};
use rustpbx::models::trunk_group_member;
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

/// Insert a gateway (sip_trunk) so we can reference it as a trunk
/// group member.
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

/// Insert a trunk group and its members directly via SeaORM.
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

// =========================================================================
// Auth tests (401)
// =========================================================================

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

#[tokio::test]
async fn create_trunk_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"x","members":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn delete_trunk_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/trunks/some-tg")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

// =========================================================================
// Happy-path read tests (carried from Plan 02-01)
// =========================================================================

#[tokio::test]
async fn list_trunks_returns_empty_paginated_response() {
    let (state, token) =
        test_state_with_api_key("trunks-list-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
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
    let (state, token) =
        test_state_with_api_key("trunks-list-seeded").await;
    insert_trunk(&state, "gw-alpha").await;
    insert_trunk_group(state.db(), "tg-primary", &["gw-alpha"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
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
    let (state, token) =
        test_state_with_api_key("trunks-get-seeded").await;
    insert_trunk(&state, "gw-beta").await;
    insert_trunk_group(state.db(), "tg-secondary", &["gw-beta"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-secondary")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
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

// =========================================================================
// 404 test
// =========================================================================

#[tokio::test]
async fn get_trunk_missing_returns_404() {
    let (state, token) =
        test_state_with_api_key("trunks-get-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/does-not-exist")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_found");
}

// =========================================================================
// TRK-02 happy paths (6)
// =========================================================================

#[tokio::test]
async fn create_trunk_happy_path_returns_201() {
    let (state, token) =
        test_state_with_api_key("trunks-create-happy").await;
    insert_trunk(&state, "gw-one").await;
    insert_trunk(&state, "gw-two").await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-happy",
                        "members": [
                            {"gateway_name": "gw-one", "weight": 80},
                            {"gateway_name": "gw-two", "priority": 1}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let body = body_json(resp).await;
    assert_eq!(body["name"], "tg-happy");
    assert_eq!(body["direction"], "bidirectional");
    assert_eq!(body["distribution_mode"], "round_robin");
    assert_eq!(body["is_active"], true);
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[0]["gateway_name"], "gw-one");
    assert_eq!(members[0]["weight"], 80);
    assert_eq!(members[0]["position"], 0);
    assert_eq!(members[1]["gateway_name"], "gw-two");
    assert_eq!(members[1]["priority"], 1);
    assert_eq!(members[1]["position"], 1);
}

#[tokio::test]
async fn create_trunk_persists_credentials_acl_nofailover() {
    let (state, token) =
        test_state_with_api_key("trunks-create-creds").await;
    insert_trunk(&state, "gw-creds").await;

    let creds = json!({"auth_username": "user1", "auth_password": "pw"});
    let acl =
        json!({"allowed_cidrs": ["10.0.0.0/8"], "denied_cidrs": []});
    let nofailover = json!([503, 502]);

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-creds",
                        "members": [{"gateway_name": "gw-creds"}],
                        "credentials": creds,
                        "acl": acl,
                        "nofailover_sip_codes": nofailover
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // GET to verify round-trip
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-creds")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    let body = body_json(resp2).await;
    assert_eq!(body["credentials"]["auth_username"], "user1");
    assert_eq!(body["acl"]["allowed_cidrs"][0], "10.0.0.0/8");
    assert_eq!(body["nofailover_sip_codes"][0], 503);
    assert_eq!(body["nofailover_sip_codes"][1], 502);
}

#[tokio::test]
async fn get_trunk_returns_members_in_position_order() {
    let (state, token) =
        test_state_with_api_key("trunks-member-order").await;
    insert_trunk(&state, "gw-z").await;
    insert_trunk(&state, "gw-a").await;

    // Insert group manually with non-sorted positions
    let now = Utc::now();
    let db = state.db();
    let group_am = trunk_group::ActiveModel {
        name: Set("tg-order".to_string()),
        display_name: Set(None),
        direction: Set(SipTrunkDirection::Outbound),
        distribution_mode: Set(TrunkGroupDistributionMode::RoundRobin),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let group = group_am.insert(db).await.unwrap();

    // Insert member with position=1 first
    let m1 = trunk_group_member::ActiveModel {
        trunk_group_id: Set(group.id),
        gateway_name: Set("gw-z".to_string()),
        weight: Set(100),
        priority: Set(0),
        position: Set(1),
        ..Default::default()
    };
    m1.insert(db).await.unwrap();

    // Then member with position=0
    let m0 = trunk_group_member::ActiveModel {
        trunk_group_id: Set(group.id),
        gateway_name: Set("gw-a".to_string()),
        weight: Set(100),
        priority: Set(0),
        position: Set(0),
        ..Default::default()
    };
    m0.insert(db).await.unwrap();

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-order")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    let members = body["members"].as_array().unwrap();
    assert_eq!(members[0]["gateway_name"], "gw-a");
    assert_eq!(members[0]["position"], 0);
    assert_eq!(members[1]["gateway_name"], "gw-z");
    assert_eq!(members[1]["position"], 1);
}

#[tokio::test]
async fn update_trunk_replaces_members_atomically() {
    let (state, token) =
        test_state_with_api_key("trunks-update-members").await;
    insert_trunk(&state, "gw-old1").await;
    insert_trunk(&state, "gw-old2").await;
    insert_trunk(&state, "gw-new1").await;
    insert_trunk(&state, "gw-new2").await;
    insert_trunk(&state, "gw-new3").await;

    // Create with 2 members via POST
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-replace",
                        "members": [
                            {"gateway_name": "gw-old1"},
                            {"gateway_name": "gw-old2"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // PUT with 3 different members
    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-replace")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "members": [
                            {"gateway_name": "gw-new1"},
                            {"gateway_name": "gw-new2"},
                            {"gateway_name": "gw-new3"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    let body = body_json(resp2).await;
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(members[0]["gateway_name"], "gw-new1");
    assert_eq!(members[1]["gateway_name"], "gw-new2");
    assert_eq!(members[2]["gateway_name"], "gw-new3");

    // GET to confirm persistence
    let app3 = rustpbx::app::create_router(state);
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-replace")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp3.status().as_u16(), 200);
    let body3 = body_json(resp3).await;
    let members3 = body3["members"].as_array().unwrap();
    assert_eq!(members3.len(), 3);
    // Zero old members
    for m in members3 {
        let gn = m["gateway_name"].as_str().unwrap();
        assert!(
            gn.starts_with("gw-new"),
            "expected new member, got {}",
            gn
        );
    }
}

#[tokio::test]
async fn update_trunk_patches_scalar_columns() {
    let (state, token) =
        test_state_with_api_key("trunks-update-scalar").await;
    insert_trunk(&state, "gw-scalar").await;

    // Create
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-scalar",
                        "members": [{"gateway_name": "gw-scalar"}],
                        "direction": "outbound"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // PUT with only display_name changed (members kept same)
    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-scalar")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "display_name": "New Display",
                        "members": [{"gateway_name": "gw-scalar"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    let body = body_json(resp2).await;
    assert_eq!(body["display_name"], "New Display");
    // Direction unchanged
    assert_eq!(body["direction"], "outbound");
    assert_eq!(body["distribution_mode"], "round_robin");
}

#[tokio::test]
async fn delete_trunk_happy_path_returns_204() {
    let (state, token) =
        test_state_with_api_key("trunks-delete-happy").await;
    insert_trunk(&state, "gw-del").await;

    // Create
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-delete-me",
                        "members": [{"gateway_name": "gw-del"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Delete
    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/trunks/tg-delete-me")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 204);

    // GET should 404
    let app3 = rustpbx::app::create_router(state);
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-delete-me")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp3.status().as_u16(), 404);
}

// =========================================================================
// TRK-03 validation 400s (6)
// =========================================================================

#[tokio::test]
async fn create_trunk_missing_members_returns_400() {
    let (state, token) =
        test_state_with_api_key("trunks-no-members").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-empty",
                        "members": []
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = body_json(resp).await;
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("at least one member"),
        "unexpected error: {}",
        msg
    );
}

#[tokio::test]
async fn create_trunk_unknown_gateway_returns_400() {
    let (state, token) =
        test_state_with_api_key("trunks-unknown-gw").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-bad-gw",
                        "members": [
                            {"gateway_name": "no-such-gateway"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = body_json(resp).await;
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("unknown gateway(s):"),
        "unexpected error: {}",
        msg
    );
    assert!(msg.contains("no-such-gateway"));
}

#[tokio::test]
async fn create_trunk_invalid_name_returns_400() {
    let (state, token) =
        test_state_with_api_key("trunks-bad-name").await;
    insert_trunk(&state, "gw-for-name-test").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "bad name with spaces",
                        "members": [
                            {"gateway_name": "gw-for-name-test"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = body_json(resp).await;
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("alphanumeric"),
        "unexpected error: {}",
        msg
    );
}

#[tokio::test]
async fn create_trunk_name_collides_with_gateway_returns_400() {
    let (state, token) =
        test_state_with_api_key("trunks-name-collision").await;
    insert_trunk(&state, "shared-name").await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "shared-name",
                        "members": [
                            {"gateway_name": "shared-name"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = body_json(resp).await;
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("collides with existing gateway"),
        "unexpected error: {}",
        msg
    );
}

#[cfg(not(feature = "parallel-trunk-dial"))]
#[tokio::test]
async fn create_trunk_parallel_mode_without_feature_returns_400() {
    let (state, token) =
        test_state_with_api_key("trunks-parallel-gate").await;
    insert_trunk(&state, "gw-parallel").await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-parallel",
                        "members": [
                            {"gateway_name": "gw-parallel"}
                        ],
                        "distribution_mode": "parallel"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = body_json(resp).await;
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("parallel-trunk-dial"),
        "unexpected error: {}",
        msg
    );
}

#[tokio::test]
async fn update_trunk_unknown_gateway_returns_400() {
    let (state, token) =
        test_state_with_api_key("trunks-update-bad-gw").await;
    insert_trunk(&state, "gw-update-ok").await;

    // Create first
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-update-bad",
                        "members": [
                            {"gateway_name": "gw-update-ok"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // PUT with unknown gateway
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-update-bad")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "members": [
                            {"gateway_name": "ghost-gateway"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 400);
    let body = body_json(resp2).await;
    let msg = body["error"].as_str().unwrap();
    assert!(msg.contains("unknown gateway(s):"));
}

// =========================================================================
// TRK-04 engagement 409s (3)
// =========================================================================

#[tokio::test]
async fn delete_trunk_blocked_by_did_reference_returns_409() {
    let (state, token) =
        test_state_with_api_key("trunks-del-did-block").await;
    insert_trunk(&state, "gw-did-block").await;

    // Create trunk group
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-did-block",
                        "members": [
                            {"gateway_name": "gw-did-block"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Directly insert a DID row referencing the trunk group
    let now = Utc::now();
    let did_am = did::ActiveModel {
        number: Set("+15551234567".to_string()),
        trunk_name: Set(None),
        extension_number: Set(None),
        failover_trunk: Set(None),
        label: Set(None),
        enabled: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        trunk_group_name: Set(Some("tg-did-block".to_string())),
    };
    did_am.insert(state.db()).await.expect("insert DID");

    // Attempt DELETE -- should get 409
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/trunks/tg-did-block")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 409);
    let body = body_json(resp2).await;
    let msg = body["error"].as_str().unwrap();
    assert!(msg.contains("referenced by DID"));
}

#[tokio::test]
async fn delete_trunk_blocked_by_route_reference_returns_409() {
    let (state, token) =
        test_state_with_api_key("trunks-del-route-block").await;
    insert_trunk(&state, "gw-route-block").await;

    // Create trunk group
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-route-block",
                        "members": [
                            {"gateway_name": "gw-route-block"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Directly insert a route row with target_trunks referencing it
    let now = Utc::now();
    let route_am = routing::ActiveModel {
        name: Set("route-blocks-delete".to_string()),
        description: Set(None),
        direction: Set(routing::RoutingDirection::Outbound),
        priority: Set(100),
        is_active: Set(true),
        selection_strategy: Set(
            routing::RoutingSelectionStrategy::RoundRobin,
        ),
        hash_key: Set(None),
        source_trunk_id: Set(None),
        default_trunk_id: Set(None),
        source_pattern: Set(None),
        destination_pattern: Set(None),
        header_filters: Set(None),
        rewrite_rules: Set(None),
        target_trunks: Set(Some(json!(["tg-route-block"]))),
        owner: Set(None),
        notes: Set(None),
        metadata: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        last_deployed_at: Set(None),
        ..Default::default()
    };
    route_am.insert(state.db()).await.expect("insert route");

    // Attempt DELETE -- should get 409
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/trunks/tg-route-block")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 409);
    let body = body_json(resp2).await;
    let msg = body["error"].as_str().unwrap();
    assert!(msg.contains("referenced by route"));
}

#[tokio::test]
async fn delete_trunk_not_blocked_by_unrelated_route_returns_204() {
    let (state, token) =
        test_state_with_api_key("trunks-del-unrelated").await;
    insert_trunk(&state, "gw-unrelated").await;

    // Create trunk group
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "tg-unrelated",
                        "members": [
                            {"gateway_name": "gw-unrelated"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Insert a route referencing a DIFFERENT trunk group name
    let now = Utc::now();
    let route_am = routing::ActiveModel {
        name: Set("route-other".to_string()),
        description: Set(None),
        direction: Set(routing::RoutingDirection::Outbound),
        priority: Set(100),
        is_active: Set(true),
        selection_strategy: Set(
            routing::RoutingSelectionStrategy::RoundRobin,
        ),
        hash_key: Set(None),
        source_trunk_id: Set(None),
        default_trunk_id: Set(None),
        source_pattern: Set(None),
        destination_pattern: Set(None),
        header_filters: Set(None),
        rewrite_rules: Set(None),
        target_trunks: Set(Some(json!(["some-other-tg"]))),
        owner: Set(None),
        notes: Set(None),
        metadata: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        last_deployed_at: Set(None),
        ..Default::default()
    };
    route_am.insert(state.db()).await.expect("insert route");

    // DELETE should succeed
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/trunks/tg-unrelated")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 204);
}
