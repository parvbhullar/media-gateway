//! Integration tests for `/api/v1/trunks/{name}/origination_uris` (Phase 3
//! Plan 03-03 — TSUB-02).
//!
//! Matrix per `03-CONTEXT.md` Integration test convention (IT-01) and
//! Plan 03-03 <action>:
//!
//!   1. 401 without Bearer token
//!   2. list-empty returns `[]`
//!   3. POST happy round-trip (201 + follow-up GET, first position = 0)
//!   4. POST auto-assigns incrementing positions (0, 1, 2)
//!   5. POST invalid URI returns 400 (via length validation — D-08
//!      rsipstack parser is very forgiving; the length ceiling is the
//!      reliably-triggerable 400 path)
//!   6. POST duplicate URI returns 409 (UNIQUE per D-06)
//!   7. DELETE happy returns 204 (URL-encoded path; +follow-up GET empty)
//!   8. DELETE missing URI returns 404 (D-07 strict)
//!   9. GET on missing parent trunk returns 404
//!
//! The fixture helpers (`insert_trunk`, `insert_trunk_group`, `body_json`)
//! mirror the pattern in `tests/api_v1_trunk_credentials.rs` — inline-copied
//! instead of hoisted into `tests/common` so the two files stay
//! self-contained (see Plan 03-02 hand-off note 7).

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

/// Percent-encode `:` and `;` (reserved in path-param) so that a URI like
/// `sip:host:5060` can ride in a `{uri}` path segment without tripping
/// axum's URI parser. Anything else is left alone — tests pick URIs that
/// don't need further encoding.
fn encode_uri_path(uri: &str) -> String {
    uri.replace(':', "%3A").replace(';', "%3B")
}

// =========================================================================
// 1. Auth (401)
// =========================================================================

#[tokio::test]
async fn list_origination_uris_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/any-tg/origination_uris")
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
async fn list_origination_uris_empty_returns_empty_array() {
    let (state, token) =
        test_state_with_api_key("ouri-list-empty").await;
    insert_trunk(&state, "gw-ouri-empty").await;
    insert_trunk_group(state.db(), "tg-ouri-empty", &["gw-ouri-empty"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-ouri-empty/origination_uris")
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
// 3. POST happy → 201 + follow-up GET (first row has position 0)
// =========================================================================

#[tokio::test]
async fn add_uri_happy_returns_201_and_round_trips() {
    let (state, token) =
        test_state_with_api_key("ouri-add-happy").await;
    insert_trunk(&state, "gw-ouri-add").await;
    insert_trunk_group(state.db(), "tg-ouri-add", &["gw-ouri-add"]).await;

    // POST
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-ouri-add/origination_uris")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "uri": "sip:carrier1.example.com:5060"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let body = body_json(resp).await;
    assert_eq!(body["uri"], "sip:carrier1.example.com:5060");
    // D-06: first row must be position 0 (MAX+1 with no rows = 0).
    assert_eq!(body["position"], 0);

    // GET round-trip — the new URI is listed.
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-ouri-add/origination_uris")
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
    assert_eq!(arr[0]["uri"], "sip:carrier1.example.com:5060");
    assert_eq!(arr[0]["position"], 0);
}

// =========================================================================
// 4. POST auto-assigns incrementing positions (D-06)
// =========================================================================

#[tokio::test]
async fn add_uri_auto_assigns_incrementing_positions() {
    let (state, token) =
        test_state_with_api_key("ouri-add-pos").await;
    insert_trunk(&state, "gw-ouri-pos").await;
    insert_trunk_group(state.db(), "tg-ouri-pos", &["gw-ouri-pos"]).await;

    let uris = [
        "sip:gw1.example.com",
        "sip:gw2.example.com",
        "sip:gw3.example.com",
    ];

    for (i, uri) in uris.iter().enumerate() {
        let app = rustpbx::app::create_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/trunks/tg-ouri-pos/origination_uris")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", token),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"uri": uri}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            201,
            "POST {} expected 201",
            uri
        );
        let body = body_json(resp).await;
        assert_eq!(
            body["position"],
            i as i64,
            "POST {} should get position {}",
            uri,
            i
        );
    }

    // GET returns all three in position order (0, 1, 2).
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-ouri-pos/origination_uris")
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
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 3);
    for (i, expected_uri) in uris.iter().enumerate() {
        assert_eq!(arr[i]["uri"], *expected_uri);
        assert_eq!(arr[i]["position"], i as i64);
    }
}

// =========================================================================
// 5. POST invalid URI → 400 (D-08 rsipstack parser)
// =========================================================================

/// The rsipstack URI parser is extremely permissive — empirical probe
/// (tested inline during Plan 03-03 execution) showed it accepts
/// "not a sip uri at all", "<<<bad>>>", "@@@", "http://example.com",
/// empty strings, whitespace, and "sip:[unclosed" as valid Uri values.
/// Per Plan 03-03 guidance: when no string reliably trips the parser,
/// exercise the 400 code path through the length-validation branch of
/// `validate_sip_uri` instead. Both branches return the same
/// ApiError::bad_request shape, so this preserves IT-01 coverage of the
/// 400 surface on POST.
#[tokio::test]
async fn add_uri_invalid_uri_returns_400() {
    let (state, token) =
        test_state_with_api_key("ouri-add-bad").await;
    insert_trunk(&state, "gw-ouri-bad").await;
    insert_trunk_group(state.db(), "tg-ouri-bad", &["gw-ouri-bad"]).await;

    // Build a URI longer than the 500-char DB column bound to reliably
    // trigger the validate_sip_uri length check.
    let oversized = format!("sip:{}@example.com", "a".repeat(600));
    assert!(oversized.len() > 500);

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-ouri-bad/origination_uris")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"uri": oversized}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "bad_request");
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("1-500 chars"),
        "expected length-bound error, got: {}",
        msg
    );
}

// =========================================================================
// 6. POST duplicate URI → 409 (D-06 UNIQUE)
// =========================================================================

#[tokio::test]
async fn add_uri_duplicate_returns_409() {
    let (state, token) =
        test_state_with_api_key("ouri-add-dup").await;
    insert_trunk(&state, "gw-ouri-dup").await;
    insert_trunk_group(state.db(), "tg-ouri-dup", &["gw-ouri-dup"]).await;

    // First POST — 201
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-ouri-dup/origination_uris")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"uri": "sip:dup.example.com:5060"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Second POST with same URI — 409
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-ouri-dup/origination_uris")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"uri": "sip:dup.example.com:5060"}).to_string(),
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
        msg.contains("sip:dup.example.com:5060"),
        "expected error to name the uri, got: {}",
        msg
    );
}

// =========================================================================
// 7. DELETE happy → 204 + follow-up GET shows empty
// =========================================================================

#[tokio::test]
async fn delete_uri_happy_returns_204() {
    let (state, token) =
        test_state_with_api_key("ouri-del-happy").await;
    insert_trunk(&state, "gw-ouri-del").await;
    insert_trunk_group(state.db(), "tg-ouri-del", &["gw-ouri-del"]).await;

    let uri = "sip:carrier1.example.com:5060";

    // Seed a URI via POST
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/trunks/tg-ouri-del/origination_uris")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"uri": uri}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // DELETE — 204 (URL-encoded path segment for the colons in the URI)
    let encoded = encode_uri_path(uri);
    let path = format!(
        "/api/v1/trunks/tg-ouri-del/origination_uris/{}",
        encoded
    );
    let app2 = rustpbx::app::create_router(state.clone());
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&path)
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp2.status().as_u16(),
        204,
        "DELETE {} expected 204",
        path
    );

    // Follow-up GET — empty list
    let app3 = rustpbx::app::create_router(state);
    let resp3 = app3
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-ouri-del/origination_uris")
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
// 8. DELETE missing URI → strict 404 (D-07)
// =========================================================================

#[tokio::test]
async fn delete_uri_missing_returns_404() {
    let (state, token) =
        test_state_with_api_key("ouri-del-missing").await;
    insert_trunk(&state, "gw-ouri-dmiss").await;
    insert_trunk_group(state.db(), "tg-ouri-dmiss", &["gw-ouri-dmiss"]).await;
    // NOTE: parent trunk exists; the URI inside it does not.

    let uri = "sip:never-added.example.com";
    let encoded = encode_uri_path(uri);
    let path = format!(
        "/api/v1/trunks/tg-ouri-dmiss/origination_uris/{}",
        encoded
    );

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&path)
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
        msg.contains(uri),
        "expected error to name the uri, got: {}",
        msg
    );
}

// =========================================================================
// 9. Parent trunk missing → 404 on list
// =========================================================================

#[tokio::test]
async fn list_origination_uris_parent_missing_returns_404() {
    let (state, token) =
        test_state_with_api_key("ouri-parent-missing").await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/no-such-tg/origination_uris")
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
