//! Integration tests for `/api/v1/diagnostics/*` (Phase 1, Plan 01-04).

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use rustpbx::models::routing::{
    self, ActiveModel as RouteAm, RoutingDirection, RoutingSelectionStrategy,
};
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{Value, json};
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

async fn seed_route(
    state: &rustpbx::app::AppState,
    name: &str,
    direction: RoutingDirection,
    destination_pattern: Option<&str>,
    is_active: bool,
    priority: i32,
) -> routing::Model {
    let now = Utc::now();
    let am = RouteAm {
        name: Set(name.to_string()),
        direction: Set(direction),
        priority: Set(priority),
        is_active: Set(is_active),
        selection_strategy: Set(RoutingSelectionStrategy::RoundRobin),
        destination_pattern: Set(destination_pattern.map(String::from)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(state.db()).await.expect("seed route")
}

// ---------------------------------------------------------------------------
// POST /diagnostics/route-evaluate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn route_evaluate_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/diagnostics/route-evaluate")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"caller":"a","destination":"b"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn route_evaluate_happy_path_matches_rule() {
    let (state, token) = test_state_with_api_key("eval-happy").await;
    seed_route(
        &state,
        "us-outbound",
        RoutingDirection::Outbound,
        Some("^\\+1\\d+$"),
        true,
        10,
    )
    .await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/diagnostics/route-evaluate")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller":"+14155551111","destination":"+14155552222"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["matched"], true);
    assert_eq!(body["rule_name"], "us-outbound");
    assert_eq!(body["direction"], "outbound");
    assert_eq!(body["priority"], 10);
}

#[tokio::test]
async fn route_evaluate_no_match_returns_200_with_matched_false() {
    let (state, token) = test_state_with_api_key("eval-nomatch").await;
    seed_route(
        &state,
        "uk-only",
        RoutingDirection::Outbound,
        Some("^\\+44\\d+$"),
        true,
        10,
    )
    .await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/diagnostics/route-evaluate")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller":"+1","destination":"+1415"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["matched"], false);
    assert!(body["rule_id"].is_null());
}

#[tokio::test]
async fn route_evaluate_empty_fields_return_400() {
    let (state, token) = test_state_with_api_key("eval-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/diagnostics/route-evaluate")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller":"","destination":"+1415"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn route_evaluate_invalid_direction_returns_400() {
    let (state, token) = test_state_with_api_key("eval-bad-dir").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/diagnostics/route-evaluate")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller":"a","destination":"b","direction":"sideways"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn route_evaluate_filters_by_direction() {
    let (state, token) = test_state_with_api_key("eval-dir-filter").await;
    // Two rules; only the inbound one should match when direction=inbound.
    seed_route(
        &state,
        "rule-out",
        RoutingDirection::Outbound,
        Some(".*"),
        true,
        10,
    )
    .await;
    seed_route(
        &state,
        "rule-in",
        RoutingDirection::Inbound,
        Some(".*"),
        true,
        20,
    )
    .await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/diagnostics/route-evaluate")
                .header(header::AUTHORIZATION, bearer(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"caller":"a","destination":"b","direction":"inbound"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["matched"], true);
    assert_eq!(body["rule_name"], "rule-in");
}

// ---------------------------------------------------------------------------
// GET /diagnostics/registrations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_registrations_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/diagnostics/registrations")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_registrations_returns_empty_list_without_sip_server() {
    let (state, token) = test_state_with_api_key("regs-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/diagnostics/registrations")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body.is_array());
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn get_registration_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/diagnostics/registrations/alice")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_registration_missing_returns_404() {
    let (state, token) = test_state_with_api_key("regs-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/diagnostics/registrations/alice")
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
// GET /diagnostics/summary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn summary_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/diagnostics/summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn summary_returns_counts_for_active_and_inactive_routes() {
    let (state, token) = test_state_with_api_key("summary-counts").await;
    seed_route(&state, "r1", RoutingDirection::Outbound, None, true, 1).await;
    seed_route(&state, "r2", RoutingDirection::Outbound, None, true, 2).await;
    seed_route(&state, "r3", RoutingDirection::Inbound, None, false, 3).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/diagnostics/summary")
                .header(header::AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["routing"]["active_routes"], 2);
    assert_eq!(body["routing"]["inactive_routes"], 1);
    assert_eq!(body["registrations"]["count"], 0);
    assert!(body["registrations"]["users"].is_array());
}
