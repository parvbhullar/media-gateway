//! Integration tests for `/api/v1/trunks/{name}/acl` (Phase 5 Plan 05-03 — TSUB-05).
//!
//! Matrix per `05-CONTEXT.md` (D-12, D-13, D-22) and Plan 05-03 <behavior>:
//!
//!   1.  401 without Bearer token (auth)
//!   2.  Parent-missing GET → 404
//!   3.  Parent-missing POST → 404
//!   4.  Parent-missing DELETE → 404
//!   5.  list-empty returns `[]`
//!   6.  POST happy round-trip (201 + GET shows position 0)
//!   7.  POST positions auto-increment (0, 1, 2)
//!   8.  POST duplicate rule → 409 (UNIQUE per D-10/D-12)
//!   9.  POST invalid syntax (`permit ...`) → 400 (D-13)
//!   10. POST invalid CIDR prefix → 400
//!   11. POST invalid IP literal → 400
//!   12. POST `allow all` → 201
//!   13. DELETE happy → 204 (URL-encoded path; +follow-up GET empty)
//!   14. DELETE missing rule → 404 (D-12 strict)
//!
//! Fixture helpers mirror `tests/api_v1_trunk_origination_uris.rs` (D-22).

use axum::{
    body::Body,
    http::{Request, header},
};
use chrono::Utc;
use rustpbx::models::sip_trunk::{
    self, SipTransport, SipTrunkConfig, SipTrunkDirection, SipTrunkStatus,
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
    let cfg = SipTrunkConfig {
        sip_server: Some("sip.example.com:5060".to_string()),
        sip_transport: SipTransport::Udp,
        register_enabled: false,
        rewrite_hostport: true,
        ..Default::default()
    };
    let kind_config = serde_json::to_value(&cfg).unwrap();
    let am = sip_trunk::ActiveModel {
        name: Set(name.to_string()),
        kind: Set("sip".into()),
        display_name: Set(Some(format!("{} display", name))),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        is_active: Set(true),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        kind_config: Set(kind_config),
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
async fn list_acl_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/any-tg/acl")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
}

// =========================================================================
// 2. Parent-missing GET → 404
// =========================================================================

#[tokio::test]
async fn list_acl_parent_missing_returns_404() {
    let (state, token) = test_state_with_api_key("acl-parent-miss-get").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/no-such-tg/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
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
// 3. Parent-missing POST → 404
// =========================================================================

#[tokio::test]
async fn add_acl_parent_missing_returns_404() {
    let (state, token) = test_state_with_api_key("acl-parent-miss-post").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/no-such-tg/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"rule": "allow all"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

// =========================================================================
// 4. Parent-missing DELETE → 404
// =========================================================================

#[tokio::test]
async fn delete_acl_parent_missing_returns_404() {
    let (state, token) = test_state_with_api_key("acl-parent-miss-del").await;
    let app = rustpbx::app::create_router(state);
    let encoded = urlencoding::encode("allow all").to_string();
    let path = format!("/api/v1/trunks/no-such-tg/acl/{}", encoded);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&path)
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

// =========================================================================
// 5. list-empty returns []
// =========================================================================

#[tokio::test]
async fn list_acl_empty_returns_empty_array() {
    let (state, token) = test_state_with_api_key("acl-list-empty").await;
    insert_trunk(&state, "gw-acl-empty").await;
    insert_trunk_group(state.db(), "tg-acl-empty", &["gw-acl-empty"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-acl-empty/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("body is a JSON array");
    assert!(arr.is_empty(), "expected [], got {:?}", arr);
}

// =========================================================================
// 6. POST happy → 201 + follow-up GET (first row position 0)
// =========================================================================

#[tokio::test]
async fn add_acl_happy_returns_201_and_round_trips() {
    let (state, token) = test_state_with_api_key("acl-add-happy").await;
    insert_trunk(&state, "gw-acl-add").await;
    insert_trunk_group(state.db(), "tg-acl-add", &["gw-acl-add"]).await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-acl-add/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"rule": "allow 1.2.3.0/24"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["rule"], "allow 1.2.3.0/24");
    assert_eq!(body["position"], 0);

    // GET round-trip
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-acl-add/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::OK);
    let body2 = body_json(resp2).await;
    let arr = body2.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["rule"], "allow 1.2.3.0/24");
    assert_eq!(arr[0]["position"], 0);
}

// =========================================================================
// 7. POST positions auto-increment (0, 1, 2)
// =========================================================================

#[tokio::test]
async fn add_acl_auto_assigns_incrementing_positions() {
    let (state, token) = test_state_with_api_key("acl-add-pos").await;
    insert_trunk(&state, "gw-acl-pos").await;
    insert_trunk_group(state.db(), "tg-acl-pos", &["gw-acl-pos"]).await;

    let rules = [
        "allow 10.0.0.0/8",
        "deny 192.168.1.5",
        "allow all",
    ];

    for (i, rule) in rules.iter().enumerate() {
        let app = rustpbx::app::create_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/trunks/tg-acl-pos/acl")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", token),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"rule": rule}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            axum::http::StatusCode::CREATED,
            "POST {} expected 201",
            rule
        );
        let body = body_json(resp).await;
        assert_eq!(body["position"], i as i64);
    }

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-acl-pos/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 3);
    for (i, expected_rule) in rules.iter().enumerate() {
        assert_eq!(arr[i]["rule"], *expected_rule);
        assert_eq!(arr[i]["position"], i as i64);
    }
}

// =========================================================================
// 8. POST duplicate rule → 409
// =========================================================================

#[tokio::test]
async fn add_acl_duplicate_returns_409() {
    let (state, token) = test_state_with_api_key("acl-add-dup").await;
    insert_trunk(&state, "gw-acl-dup").await;
    insert_trunk_group(state.db(), "tg-acl-dup", &["gw-acl-dup"]).await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-acl-dup/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"rule": "deny 10.0.0.5"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);

    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-acl-dup/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"rule": "deny 10.0.0.5"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), axum::http::StatusCode::CONFLICT);
    let body = body_json(resp2).await;
    assert_eq!(body["code"], "conflict");
}

// =========================================================================
// 9. POST invalid syntax (action token wrong) → 400
// =========================================================================

#[tokio::test]
async fn add_acl_invalid_action_returns_400() {
    let (state, token) = test_state_with_api_key("acl-add-bad-action").await;
    insert_trunk(&state, "gw-acl-ba").await;
    insert_trunk_group(state.db(), "tg-acl-ba", &["gw-acl-ba"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-acl-ba/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"rule": "permit 1.2.3.4"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "bad_request");
}

// =========================================================================
// 10. POST invalid CIDR prefix → 400
// =========================================================================

#[tokio::test]
async fn add_acl_invalid_cidr_returns_400() {
    let (state, token) = test_state_with_api_key("acl-add-bad-cidr").await;
    insert_trunk(&state, "gw-acl-bc").await;
    insert_trunk_group(state.db(), "tg-acl-bc", &["gw-acl-bc"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-acl-bc/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"rule": "allow 1.2.3.4/99"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 11. POST invalid IP literal → 400
// =========================================================================

#[tokio::test]
async fn add_acl_invalid_ip_returns_400() {
    let (state, token) = test_state_with_api_key("acl-add-bad-ip").await;
    insert_trunk(&state, "gw-acl-bi").await;
    insert_trunk_group(state.db(), "tg-acl-bi", &["gw-acl-bi"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-acl-bi/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"rule": "deny 999.999.0.0"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 12. POST `allow all` → 201
// =========================================================================

#[tokio::test]
async fn add_acl_allow_all_returns_201() {
    let (state, token) = test_state_with_api_key("acl-add-allow-all").await;
    insert_trunk(&state, "gw-acl-aa").await;
    insert_trunk_group(state.db(), "tg-acl-aa", &["gw-acl-aa"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-acl-aa/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"rule": "allow all"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["rule"], "allow all");
    assert_eq!(body["position"], 0);
}

// =========================================================================
// 13. DELETE happy → 204 + GET shows empty
// =========================================================================

#[tokio::test]
async fn delete_acl_happy_returns_204() {
    let (state, token) = test_state_with_api_key("acl-del-happy").await;
    insert_trunk(&state, "gw-acl-del").await;
    insert_trunk_group(state.db(), "tg-acl-del", &["gw-acl-del"]).await;

    let rule = "allow 1.2.3.0/24";

    // Seed
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-acl-del/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"rule": rule}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);

    // DELETE — URL-encoded path segment
    let encoded = urlencoding::encode(rule).to_string();
    let path = format!("/api/v1/trunks/tg-acl-del/acl/{}", encoded);

    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&path)
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp2.status(),
        axum::http::StatusCode::NO_CONTENT,
        "DELETE {} expected 204",
        path
    );

    // Follow-up GET — empty
    let app3 = rustpbx::app::create_router(state);
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-acl-del/acl")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp3.status(), axum::http::StatusCode::OK);
    let body3 = body_json(resp3).await;
    let arr = body3.as_array().expect("array");
    assert!(arr.is_empty(), "expected [] after delete, got {:?}", arr);
}

// =========================================================================
// 14. DELETE missing rule → strict 404
// =========================================================================

#[tokio::test]
async fn delete_acl_missing_returns_404() {
    let (state, token) = test_state_with_api_key("acl-del-missing").await;
    insert_trunk(&state, "gw-acl-dm").await;
    insert_trunk_group(state.db(), "tg-acl-dm", &["gw-acl-dm"]).await;

    let rule = "deny 9.9.9.9";
    let encoded = urlencoding::encode(rule).to_string();
    let path = format!("/api/v1/trunks/tg-acl-dm/acl/{}", encoded);

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&path)
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "not_found");
}
