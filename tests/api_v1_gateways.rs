//! Integration tests for `/api/v1/gateways` and `/api/v1/diagnostics/trunk-test`.

use axum::{
    body::Body,
    http::{Request, header},
};
use chrono::Utc;
use rustpbx::models::sip_trunk::{self, SipTrunkDirection, SipTrunkStatus, SipTransport};
use sea_orm::{ActiveModelTrait, Set};
use serde_json::Value;
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

async fn insert_trunk(
    state: &rustpbx::app::AppState,
    name: &str,
    sip_server: Option<&str>,
) -> sip_trunk::Model {
    let now = Utc::now();
    let am = sip_trunk::ActiveModel {
        name: Set(name.to_string()),
        display_name: Set(Some(format!("{} display", name))),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        sip_server: Set(sip_server.map(|s| s.to_string())),
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

#[tokio::test]
async fn list_gateways_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/gateways")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn list_gateways_returns_empty_array_when_no_trunks() {
    let (state, token) = test_state_with_api_key("list-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/gateways")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert!(body.is_array(), "body must be a JSON array: {body}");
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_gateways_returns_inserted_trunk() {
    let (state, token) = test_state_with_api_key("list-one").await;
    insert_trunk(&state, "carrier-a", Some("sip.example.com:5060")).await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/gateways")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "carrier-a");
    assert_eq!(arr[0]["status"], "healthy");
    assert_eq!(arr[0]["transport"], "udp");
    assert_eq!(arr[0]["proxy_addr"], "sip.example.com:5060");
    assert_eq!(arr[0]["consecutive_failures"], 0);
    assert_eq!(arr[0]["failure_threshold"], 3);
    assert_eq!(arr[0]["recovery_threshold"], 2);
    assert_eq!(arr[0]["health_check_interval_secs"], 30);
}

#[tokio::test]
async fn get_gateway_404_on_missing() {
    let (state, token) = test_state_with_api_key("get-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/gateways/nope")
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

#[tokio::test]
async fn get_gateway_returns_inserted_trunk() {
    let (state, token) = test_state_with_api_key("get-one").await;
    insert_trunk(&state, "carrier-b", Some("sip.example.net:5060")).await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/gateways/carrier-b")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["name"], "carrier-b");
}

#[tokio::test]
async fn trunk_test_returns_ok_false_for_unreachable() {
    let (state, token) = test_state_with_api_key("trunk-test-unreach").await;
    insert_trunk(&state, "carrier-c", Some("127.0.0.1:1")).await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/diagnostics/trunk-test")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"carrier-c"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["ok"], false);
    // Scaffolded probe returns this deterministic detail; real probe will
    // return e.g. "timeout" or "send err: …". Either is a valid signal —
    // just assert the response shape. See `probe_trunk` TODO.
    assert!(body["detail"].is_string());
}

#[tokio::test]
async fn trunk_test_404_on_missing_gateway() {
    let (state, token) = test_state_with_api_key("trunk-test-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/diagnostics/trunk-test")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"ghost"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}
