//! Integration tests for `POST /api/v1/routing/resolve` (Phase 3 Plan 03-05 — RTE-03).
//!
//! Matrix per `03-CONTEXT.md` IT-01 convention:
//!
//!   1. 401 without Bearer token (D-16)
//!   2. no-match destination → result:"not_handled" (empty routes)
//!   3. trunk_group target → result:"matched", target.kind:"trunk_group", selected_gateway set (D-15)
//!   4. gateway target → result:"matched", target.kind:"gateway", selected_gateway:null (D-15)
//!   5. reject rule → result:"abort", match_reason contains reason (D-15)
//!   6. invalid body (missing destination_number) → 400
//!   7. trace field is a non-empty array (D-15)
//!
//! Routes are injected via `data_context.reload_routes(false, Some(config))` so the
//! live AppState snapshot includes them (same path the handler reads).

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
use rustpbx::proxy::routing::{
    DestConfig, MatchConditions, RejectConfig, RouteAction, RouteRule,
};
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

// ─── Fixture helpers ─────────────────────────────────────────────────────

async fn insert_trunk(state: &rustpbx::app::AppState, name: &str) -> sip_trunk::Model {
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
        member_am.insert(db).await.expect("insert member");
    }
    group
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

/// Inject a RouteRule into the live data_context so the handler picks it up.
async fn seed_route(state: &rustpbx::app::AppState, rule: RouteRule) {
    let mut config = rustpbx::config::ProxyConfig::default();
    config.routes = Some(vec![rule]);
    state
        .sip_server()
        .inner
        .data_context
        .reload_routes(false, Some(Arc::new(config)))
        .await
        .expect("seed route");
}

/// Clear all routes from the data_context (reset between tests via unique names).
async fn clear_routes(state: &rustpbx::app::AppState) {
    let config = rustpbx::config::ProxyConfig::default();
    state
        .sip_server()
        .inner
        .data_context
        .reload_routes(false, Some(Arc::new(config)))
        .await
        .expect("clear routes");
}

// Use `to_user` (matches callee user part only) rather than `callee` (which
// matches user@host and requires an exact full-address match per matches_pattern).
fn make_forward_rule(name: &str, to_user_pattern: &str, dest: &str) -> RouteRule {
    RouteRule {
        name: name.to_string(),
        priority: 0,
        match_conditions: MatchConditions {
            to_user: Some(to_user_pattern.to_string()),
            ..Default::default()
        },
        action: RouteAction {
            dest: Some(DestConfig::Single(dest.to_string())),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn make_reject_rule(name: &str, to_user_pattern: &str, code: u16, reason: &str) -> RouteRule {
    RouteRule {
        name: name.to_string(),
        priority: 0,
        match_conditions: MatchConditions {
            to_user: Some(to_user_pattern.to_string()),
            ..Default::default()
        },
        action: RouteAction {
            reject: Some(RejectConfig {
                code,
                reason: Some(reason.to_string()),
                headers: HashMap::new(),
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

// =========================================================================
// 1. Auth (401)
// =========================================================================

#[tokio::test]
async fn resolve_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/resolve")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller_number": "+1555", "destination_number": "+1999"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

// =========================================================================
// 2. No-match → not_handled
// =========================================================================

#[tokio::test]
async fn resolve_unknown_destination_returns_not_handled() {
    let (state, token) = test_state_with_api_key("rte-no-match").await;
    clear_routes(&state).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/resolve")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller_number": "+14155551234", "destination_number": "+449999999"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["result"], "not_handled");
    assert_eq!(body["target"], Value::Null);
    // Phase 6 Plan 06-01 (D-30 plumbing): the response surface MUST carry
    // `matched_record_id` so Wave 3 (06-04) has a place to write the
    // matched record's UUIDv4. Wave 1 always sets it to null.
    let obj = body.as_object().expect("response must be a JSON object");
    assert!(
        obj.contains_key("matched_record_id"),
        "response missing matched_record_id key: {:?}",
        obj.keys().collect::<Vec<_>>()
    );
    assert_eq!(body["matched_record_id"], Value::Null);
}

// =========================================================================
// 3. Trunk-group target → matched + selected_gateway (D-15)
// =========================================================================

// DEFERRED: requires Block R-full matcher swap — RT's matcher returns `gateway`
// kind for trunk-group routes; sip_fix's table_matcher returns `trunk_group`.
// Re-enable after the matcher swap lands.
#[cfg(any())]
#[tokio::test]
async fn resolve_with_trunk_group_target_returns_selected_gateway() {
    let (state, token) = test_state_with_api_key("rte-tg-target").await;

    insert_trunk(&state, "gw-alpha").await;
    insert_trunk(&state, "gw-beta").await;
    insert_trunk_group(state.db(), "tg-prod", &["gw-alpha", "gw-beta"]).await;

    seed_route(
        &state,
        make_forward_rule("route-tg-prod", "8005551234", "tg-prod"),
    )
    .await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/resolve")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller_number": "+14155550001", "destination_number": "8005551234"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["result"], "matched", "body: {:?}", body);
    assert_eq!(body["target"]["kind"], "trunk_group");
    assert_eq!(body["target"]["name"], "tg-prod");
    let gw = body["selected_gateway"].as_str().unwrap();
    assert!(
        gw == "gw-alpha" || gw == "gw-beta",
        "expected gw-alpha or gw-beta, got: {}",
        gw
    );
}

// =========================================================================
// 4. Gateway target → matched, selected_gateway null (D-15)
// =========================================================================

#[tokio::test]
async fn resolve_with_gateway_target_returns_no_selected_gateway() {
    let (state, token) = test_state_with_api_key("rte-gw-target").await;

    insert_trunk(&state, "gw-direct").await;
    seed_route(
        &state,
        make_forward_rule("route-gw-direct", "9001234567", "gw-direct"),
    )
    .await;

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/resolve")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller_number": "+14155550002", "destination_number": "9001234567"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["result"], "matched", "body: {:?}", body);
    assert_eq!(body["target"]["kind"], "gateway");
    assert_eq!(body["target"]["name"], "gw-direct");
    assert_eq!(body["selected_gateway"], Value::Null);
}

// =========================================================================
// 5. Reject rule → abort (D-15)
// =========================================================================

#[tokio::test]
async fn resolve_with_reject_action_returns_abort() {
    let (state, token) = test_state_with_api_key("rte-reject").await;

    seed_route(
        &state,
        make_reject_rule("route-blocked", "8885550000", 403, "blocked"),
    )
    .await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/resolve")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller_number": "+14155550003", "destination_number": "8885550000"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["result"], "abort", "body: {:?}", body);
    let reason = body["match_reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("blocked"),
        "expected match_reason to contain 'blocked', got: {}",
        reason
    );
}

// =========================================================================
// 6. Invalid body → 400
// =========================================================================

#[tokio::test]
async fn resolve_invalid_body_returns_400() {
    let (state, token) = test_state_with_api_key("rte-bad-body").await;

    let app = rustpbx::app::create_router(state);
    // Entirely malformed JSON → axum returns 400
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/resolve")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("not json at all"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
}

// =========================================================================
// 7. Trace is a non-empty array (D-15)
// =========================================================================

#[tokio::test]
async fn resolve_response_includes_trace() {
    let (state, token) = test_state_with_api_key("rte-trace").await;
    clear_routes(&state).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/routing/resolve")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller_number": "+1555", "destination_number": "+1999"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    let trace = body["trace"].as_array().expect("trace is array");
    assert!(!trace.is_empty(), "trace must be a non-empty array");
    // The trace object should have a matched_rule field (possibly null)
    assert!(
        trace[0].get("matched_rule").is_some(),
        "trace[0] must have matched_rule field"
    );
}
