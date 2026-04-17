//! Integration tests for `/api/v1/trunks/{name}/credentials` (Phase 3
//! Plan 03-02 — TSUB-01).
//!
//! Matrix per `03-CONTEXT.md` Integration test convention (IT-01) and
//! Plan 03-02 <action>:
//!
//!   1. 401 without Bearer token
//!   2. list-empty returns `[]`
//!   3. POST happy round-trip (201 + follow-up GET)
//!   4. POST duplicate realm returns 409
//!   5. DELETE happy returns 204 (+ follow-up GET empty)
//!   6. DELETE missing realm returns 404
//!   7. GET on missing parent trunk returns 404
//!   8. POST realm containing '/' returns 400 (D-05)
//!
//! The fixture helpers (`insert_trunk`, `insert_trunk_group`, `body_json`)
//! mirror the pattern in `tests/api_v1_trunks.rs` — inline-copied instead
//! of hoisted into `tests/common` so the two files stay self-contained.

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

// =========================================================================
// 1. Auth (401)
// =========================================================================

#[tokio::test]
async fn list_credentials_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/any-tg/credentials")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

// =========================================================================
// 2. List empty
// =========================================================================

#[tokio::test]
async fn list_credentials_empty_returns_empty_array() {
    let (state, token) =
        test_state_with_api_key("creds-list-empty").await;
    insert_trunk(&state, "gw-empty").await;
    insert_trunk_group(state.db(), "tg-creds-empty", &["gw-empty"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-creds-empty/credentials")
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
    let arr = body.as_array().expect("body is a JSON array");
    assert!(arr.is_empty(), "expected [], got {:?}", arr);
}

// =========================================================================
// 3. POST happy → round-trip via GET
// =========================================================================

#[tokio::test]
async fn add_credential_happy_returns_201_and_round_trips_via_get() {
    let (state, token) =
        test_state_with_api_key("creds-add-happy").await;
    insert_trunk(&state, "gw-add").await;
    insert_trunk_group(state.db(), "tg-creds-add", &["gw-add"]).await;

    // POST
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-creds-add/credentials")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "realm": "sip.carrier.example",
                        "username": "alice",
                        "password": "s3cret!"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let body = body_json(resp).await;
    assert_eq!(body["realm"], "sip.carrier.example");
    assert_eq!(body["username"], "alice");
    assert_eq!(body["password"], "s3cret!");

    // GET round-trip — the new credential is listed
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-creds-add/credentials")
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
    let body2 = body_json(resp2).await;
    let arr = body2.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["realm"], "sip.carrier.example");
    assert_eq!(arr[0]["username"], "alice");
    assert_eq!(arr[0]["password"], "s3cret!");
}

// =========================================================================
// 4. POST duplicate realm → 409
// =========================================================================

#[tokio::test]
async fn add_credential_duplicate_realm_returns_409() {
    let (state, token) =
        test_state_with_api_key("creds-add-dup").await;
    insert_trunk(&state, "gw-dup").await;
    insert_trunk_group(state.db(), "tg-creds-dup", &["gw-dup"]).await;

    // First POST — 201
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-creds-dup/credentials")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "realm": "dup-realm.example",
                        "username": "u1",
                        "password": "p1"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Second POST with the same realm — 409
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-creds-dup/credentials")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "realm": "dup-realm.example",
                        "username": "u2",
                        "password": "p2"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 409);
    let body = body_json(resp2).await;
    assert_eq!(body["code"], "conflict");
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("dup-realm.example"),
        "expected error to name the realm, got: {}",
        msg
    );
}

// =========================================================================
// 5. DELETE happy → 204 + follow-up GET shows empty
// =========================================================================

#[tokio::test]
async fn delete_credential_happy_returns_204() {
    let (state, token) =
        test_state_with_api_key("creds-del-happy").await;
    insert_trunk(&state, "gw-del").await;
    insert_trunk_group(state.db(), "tg-creds-del", &["gw-del"]).await;

    // Seed a credential via POST
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-creds-del/credentials")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "realm": "to-delete.example.com",
                        "username": "u",
                        "password": "p"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // DELETE — 204
    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(
                    "/api/v1/trunks/tg-creds-del/credentials/to-delete.example.com",
                )
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

    // Follow-up GET — empty list
    let app3 = rustpbx::app::create_router(state);
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-creds-del/credentials")
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
    let arr = body3.as_array().expect("array");
    assert!(arr.is_empty(), "expected [] after delete, got {:?}", arr);
}

// =========================================================================
// 6. DELETE missing realm → strict 404 (D-04)
// =========================================================================

#[tokio::test]
async fn delete_credential_missing_realm_returns_404() {
    let (state, token) =
        test_state_with_api_key("creds-del-missing").await;
    insert_trunk(&state, "gw-dmiss").await;
    insert_trunk_group(state.db(), "tg-creds-dmiss", &["gw-dmiss"]).await;
    // NOTE: parent trunk exists; the realm inside it does not.

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/trunks/tg-creds-dmiss/credentials/no-such-realm")
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
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("no-such-realm"),
        "expected error to name the realm, got: {}",
        msg
    );
}

// =========================================================================
// 7. Parent trunk missing → 404 on list
// =========================================================================

#[tokio::test]
async fn list_credentials_parent_missing_returns_404() {
    let (state, token) =
        test_state_with_api_key("creds-parent-missing").await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/no-such-tg/credentials")
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
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("no-such-tg"),
        "expected error to name the trunk, got: {}",
        msg
    );
}

// =========================================================================
// 8. POST realm containing '/' → 400 (D-05 router-conflict guard)
// =========================================================================

#[tokio::test]
async fn add_credential_invalid_realm_returns_400() {
    let (state, token) =
        test_state_with_api_key("creds-add-bad-realm").await;
    insert_trunk(&state, "gw-bad").await;
    insert_trunk_group(state.db(), "tg-creds-bad", &["gw-bad"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-creds-bad/credentials")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "realm": "has/slash",
                        "username": "u",
                        "password": "p"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "bad_request");
    let msg = body["error"].as_str().unwrap();
    assert!(msg.contains('/'), "expected error to reference '/', got: {}", msg);
}
