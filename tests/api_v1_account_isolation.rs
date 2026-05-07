//! IT-05 — Sub-account isolation matrix (Phase 13 Plan 13-05).
#![allow(dead_code)]
//!
//! Verifies that resources created under one tenant account are invisible to
//! another tenant, while the master (`root`) account with `?include=all` can
//! see resources from all tenants.
//!
//! Coverage matrix (per 13-05-PLAN.md):
//!   1. isolation_gateways  — sip_trunks table (account_id column)
//!   2. isolation_endpoints — supersip_endpoints table
//!   3. isolation_applications — twiml_applications table
//!   4. isolation_webhooks  — webhooks table
//!   5. isolation_trunks    — trunk_groups table (POST requires member gateway)
//!   6. isolation_recordings — call_records table (read-only; direct DB insert)

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use chrono::Utc;
use rustpbx::models::call_record;
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::test_state_with_three_accounts;

// ─── Shared HTTP helpers ─────────────────────────────────────────────────────

async fn post_json_as(
    app: axum::Router,
    uri: &str,
    token: &str,
    body: Value,
) -> Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn get_as(app: axum::Router, uri: &str, token: &str) -> Response {
    app.oneshot(
        Request::builder()
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn put_json_as(
    app: axum::Router,
    uri: &str,
    token: &str,
    body: Value,
) -> Response {
    app.oneshot(
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn delete_as(app: axum::Router, uri: &str, token: &str) -> Response {
    app.oneshot(
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn body_json(resp: Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

fn names_in_list(body: &Value) -> Vec<String> {
    body.as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
        .collect()
}

fn names_in_paginated(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
        .collect()
}

// ─── 1. Gateways (sip_trunks) ────────────────────────────────────────────────

#[tokio::test]
async fn isolation_gateways() {
    let (state, root_token, acme_token, globex_token) =
        test_state_with_three_accounts().await;

    // acme creates a gateway
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/gateways",
        &acme_token,
        json!({"name": "gw-acme", "sip_server": "10.0.0.1:5060"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "acme create gateway");

    // globex creates a gateway
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/gateways",
        &globex_token,
        json!({"name": "gw-globex", "sip_server": "10.0.0.2:5060"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "globex create gateway");

    // acme list: contains gw-acme, NOT gw-globex
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/gateways", &acme_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let names = names_in_list(&body);
    assert!(
        names.contains(&"gw-acme".to_string()),
        "acme list must contain gw-acme, got {names:?}"
    );
    assert!(
        !names.contains(&"gw-globex".to_string()),
        "acme list must NOT contain gw-globex, got {names:?}"
    );

    // acme GET globex gateway → 404
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/gateways/gw-globex", &acme_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "acme must get 404 for gw-globex"
    );

    // acme ?include=all → 403
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/gateways?include=all", &acme_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "sub-account must get 403 for ?include=all"
    );

    // acme ?account_id=globex → 403
    let app = rustpbx::app::create_router(state.clone());
    let resp =
        get_as(app, "/api/v1/gateways?account_id=globex", &acme_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "sub-account must get 403 for ?account_id=globex"
    );

    // root ?include=all → contains both gw-acme and gw-globex
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/gateways?include=all", &root_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let names = names_in_list(&body);
    assert!(
        names.contains(&"gw-acme".to_string()),
        "root include=all must contain gw-acme, got {names:?}"
    );
    assert!(
        names.contains(&"gw-globex".to_string()),
        "root include=all must contain gw-globex, got {names:?}"
    );
}

// ─── 2. Endpoints (supersip_endpoints) ───────────────────────────────────────

#[tokio::test]
async fn isolation_endpoints() {
    let (state, root_token, acme_token, globex_token) =
        test_state_with_three_accounts().await;

    // acme creates endpoint alice
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/endpoints",
        &acme_token,
        json!({"username": "alice", "password": "secret"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "acme create alice");

    // globex creates endpoint bob
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/endpoints",
        &globex_token,
        json!({"username": "bob", "password": "secret"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "globex create bob");
    let bob_body = body_json(resp).await;
    let bob_id = bob_body["id"].as_str().expect("bob id").to_string();

    // acme list: contains alice, NOT bob
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/endpoints", &acme_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    let usernames: Vec<&str> = arr
        .iter()
        .filter_map(|v| v["username"].as_str())
        .collect();
    assert!(
        usernames.contains(&"alice"),
        "acme list must contain alice, got {usernames:?}"
    );
    assert!(
        !usernames.contains(&"bob"),
        "acme list must NOT contain bob, got {usernames:?}"
    );

    // acme GET bob's endpoint → 404
    let app = rustpbx::app::create_router(state.clone());
    let resp =
        get_as(app, &format!("/api/v1/endpoints/{bob_id}"), &acme_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "acme must get 404 for bob's endpoint"
    );

    // acme ?include=all → 403
    let app = rustpbx::app::create_router(state.clone());
    let resp =
        get_as(app, "/api/v1/endpoints?include=all", &acme_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "sub-account must get 403 for ?include=all"
    );

    // root ?include=all → contains both alice and bob
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/endpoints?include=all", &root_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    let usernames: Vec<&str> = arr
        .iter()
        .filter_map(|v| v["username"].as_str())
        .collect();
    assert!(
        usernames.contains(&"alice"),
        "root include=all must contain alice, got {usernames:?}"
    );
    assert!(
        usernames.contains(&"bob"),
        "root include=all must contain bob, got {usernames:?}"
    );
}

// ─── 3. Applications (twiml_applications) ────────────────────────────────────

#[tokio::test]
async fn isolation_applications() {
    let (state, root_token, acme_token, globex_token) =
        test_state_with_three_accounts().await;

    // acme creates app-acme
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/applications",
        &acme_token,
        json!({"name": "app-acme", "answer_url": "https://acme.example/twiml"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "acme create app");

    // globex creates app-globex
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/applications",
        &globex_token,
        json!({"name": "app-globex", "answer_url": "https://globex.example/twiml"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "globex create app");
    let globex_app_body = body_json(resp).await;
    let globex_app_id = globex_app_body["id"]
        .as_str()
        .expect("globex app id")
        .to_string();

    // acme list: contains app-acme, NOT app-globex
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/applications", &acme_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    let app_names: Vec<&str> =
        arr.iter().filter_map(|v| v["name"].as_str()).collect();
    assert!(
        app_names.contains(&"app-acme"),
        "acme list must contain app-acme, got {app_names:?}"
    );
    assert!(
        !app_names.contains(&"app-globex"),
        "acme list must NOT contain app-globex, got {app_names:?}"
    );

    // acme GET globex app → 404
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(
        app,
        &format!("/api/v1/applications/{globex_app_id}"),
        &acme_token,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "acme must get 404 for globex's application"
    );

    // acme ?include=all → 403
    let app = rustpbx::app::create_router(state.clone());
    let resp =
        get_as(app, "/api/v1/applications?include=all", &acme_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "sub-account must get 403 for ?include=all"
    );

    // root ?include=all → contains both
    let app = rustpbx::app::create_router(state.clone());
    let resp =
        get_as(app, "/api/v1/applications?include=all", &root_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    let app_names: Vec<&str> =
        arr.iter().filter_map(|v| v["name"].as_str()).collect();
    assert!(
        app_names.contains(&"app-acme"),
        "root include=all must contain app-acme, got {app_names:?}"
    );
    assert!(
        app_names.contains(&"app-globex"),
        "root include=all must contain app-globex, got {app_names:?}"
    );
}

// ─── 4. Webhooks ─────────────────────────────────────────────────────────────
//
// NOTE: The webhooks handler uses a direct `scope.account_id` filter without
// the CommonScopeQuery mechanism. Basic tenant list isolation and cross-account
// GET-by-id isolation are tested here. The `?include=all` scope query is not
// supported by the webhooks endpoint (it does not accept CommonScopeQuery) so
// those assertions are omitted for this resource type.

#[tokio::test]
async fn isolation_webhooks() {
    let (state, _root_token, acme_token, globex_token) =
        test_state_with_three_accounts().await;

    // acme creates wh-acme
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/webhooks",
        &acme_token,
        json!({"name": "wh-acme", "url": "https://acme.example/hook", "secret": "s1", "events": []}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "acme create webhook");

    // globex creates wh-globex
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/webhooks",
        &globex_token,
        json!({"name": "wh-globex", "url": "https://globex.example/hook", "secret": "s2", "events": []}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "globex create webhook");
    let globex_wh_body = body_json(resp).await;
    let globex_wh_id = globex_wh_body["id"]
        .as_str()
        .expect("globex webhook id")
        .to_string();

    // acme list: contains wh-acme, NOT wh-globex
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/webhooks", &acme_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    let wh_names: Vec<&str> =
        arr.iter().filter_map(|v| v["name"].as_str()).collect();
    assert!(
        wh_names.contains(&"wh-acme"),
        "acme list must contain wh-acme, got {wh_names:?}"
    );
    assert!(
        !wh_names.contains(&"wh-globex"),
        "acme list must NOT contain wh-globex, got {wh_names:?}"
    );

    // acme GET globex webhook → 404
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(
        app,
        &format!("/api/v1/webhooks/{globex_wh_id}"),
        &acme_token,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "acme must get 404 for globex's webhook"
    );
}

// ─── 5. Trunk groups ─────────────────────────────────────────────────────────
//
// The trunks CRUD (POST /api/v1/trunks) requires at least one gateway member.
// We create a gateway per account first, then create the trunk group.

#[tokio::test]
async fn isolation_trunks() {
    let (state, root_token, acme_token, globex_token) =
        test_state_with_three_accounts().await;

    // Create a gateway for each tenant (gateways are scope-filtered too)
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/gateways",
        &acme_token,
        json!({"name": "trk-gw-acme", "sip_server": "10.1.0.1:5060"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "acme create gateway for trunk");

    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/gateways",
        &globex_token,
        json!({"name": "trk-gw-globex", "sip_server": "10.1.0.2:5060"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "globex create gateway for trunk");

    // acme creates trunk-acme
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/trunks",
        &acme_token,
        json!({"name": "trunk-acme", "members": [{"gateway_name": "trk-gw-acme"}]}),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "acme create trunk: body={:?}",
        body_json(resp).await
    );

    // globex creates trunk-globex
    let app = rustpbx::app::create_router(state.clone());
    let resp = post_json_as(
        app,
        "/api/v1/trunks",
        &globex_token,
        json!({"name": "trunk-globex", "members": [{"gateway_name": "trk-gw-globex"}]}),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "globex create trunk"
    );

    // acme list: contains trunk-acme, NOT trunk-globex
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/trunks", &acme_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let names = names_in_paginated(&body);
    assert!(
        names.contains(&"trunk-acme".to_string()),
        "acme list must contain trunk-acme, got {names:?}"
    );
    assert!(
        !names.contains(&"trunk-globex".to_string()),
        "acme list must NOT contain trunk-globex, got {names:?}"
    );

    // acme GET globex trunk → 404
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/trunks/trunk-globex", &acme_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "acme must get 404 for trunk-globex"
    );

    // acme ?include=all → 403
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/trunks?include=all", &acme_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "sub-account must get 403 for ?include=all on trunks"
    );

    // root ?include=all → contains both
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/trunks?include=all", &root_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let names = names_in_paginated(&body);
    assert!(
        names.contains(&"trunk-acme".to_string()),
        "root include=all must contain trunk-acme, got {names:?}"
    );
    assert!(
        names.contains(&"trunk-globex".to_string()),
        "root include=all must contain trunk-globex, got {names:?}"
    );
}

// ─── 6. Recordings / CDRs (read-only isolation) ──────────────────────────────

#[tokio::test]
async fn isolation_recordings() {
    let (state, root_token, acme_token, _globex_token) =
        test_state_with_three_accounts().await;

    let now = Utc::now();

    // Insert a CDR for acme directly into the DB
    call_record::ActiveModel {
        call_id: Set("cdr-acme-001".to_string()),
        display_id: Set(None),
        direction: Set("outbound".to_string()),
        status: Set("answered".to_string()),
        started_at: Set(now),
        ended_at: Set(Some(now)),
        duration_secs: Set(60),
        from_number: Set(Some("+12025550001".to_string())),
        to_number: Set(Some("+12025550002".to_string())),
        caller_name: Set(None),
        agent_name: Set(None),
        queue: Set(None),
        department_id: Set(None),
        extension_id: Set(None),
        sip_trunk_id: Set(None),
        route_id: Set(None),
        sip_gateway: Set(None),
        rewrite_original_from: Set(None),
        rewrite_original_to: Set(None),
        caller_uri: Set(None),
        callee_uri: Set(None),
        recording_url: Set(None),
        recording_duration_secs: Set(None),
        has_transcript: Set(false),
        transcript_status: Set("none".to_string()),
        transcript_language: Set(None),
        tags: Set(None),
        leg_timeline: Set(None),
        metadata: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        archived_at: Set(None),
        account_id: Set("acme".to_string()),
        ..Default::default()
    }
    .insert(state.db())
    .await
    .expect("insert acme CDR");

    // Insert a CDR for globex directly into the DB
    call_record::ActiveModel {
        call_id: Set("cdr-globex-001".to_string()),
        display_id: Set(None),
        direction: Set("inbound".to_string()),
        status: Set("answered".to_string()),
        started_at: Set(now),
        ended_at: Set(Some(now)),
        duration_secs: Set(30),
        from_number: Set(Some("+12025550003".to_string())),
        to_number: Set(Some("+12025550004".to_string())),
        caller_name: Set(None),
        agent_name: Set(None),
        queue: Set(None),
        department_id: Set(None),
        extension_id: Set(None),
        sip_trunk_id: Set(None),
        route_id: Set(None),
        sip_gateway: Set(None),
        rewrite_original_from: Set(None),
        rewrite_original_to: Set(None),
        caller_uri: Set(None),
        callee_uri: Set(None),
        recording_url: Set(None),
        recording_duration_secs: Set(None),
        has_transcript: Set(false),
        transcript_status: Set("none".to_string()),
        transcript_language: Set(None),
        tags: Set(None),
        leg_timeline: Set(None),
        metadata: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        archived_at: Set(None),
        account_id: Set("globex".to_string()),
        ..Default::default()
    }
    .insert(state.db())
    .await
    .expect("insert globex CDR");

    // acme GET /api/v1/cdrs → contains acme's CDR, NOT globex's
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/cdrs", &acme_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().expect("items array");
    let call_ids: Vec<&str> = items
        .iter()
        .filter_map(|v| v["call_id"].as_str())
        .collect();
    assert!(
        call_ids.contains(&"cdr-acme-001"),
        "acme CDR list must contain cdr-acme-001, got {call_ids:?}"
    );
    assert!(
        !call_ids.contains(&"cdr-globex-001"),
        "acme CDR list must NOT contain cdr-globex-001, got {call_ids:?}"
    );

    // acme ?include=all → 403
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/cdrs?include=all", &acme_token).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "sub-account must get 403 for CDRs ?include=all"
    );

    // root ?include=all → contains both
    let app = rustpbx::app::create_router(state.clone());
    let resp = get_as(app, "/api/v1/cdrs?include=all", &root_token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().expect("items array");
    let call_ids: Vec<&str> = items
        .iter()
        .filter_map(|v| v["call_id"].as_str())
        .collect();
    assert!(
        call_ids.contains(&"cdr-acme-001"),
        "root include=all CDR list must contain cdr-acme-001, got {call_ids:?}"
    );
    assert!(
        call_ids.contains(&"cdr-globex-001"),
        "root include=all CDR list must contain cdr-globex-001, got {call_ids:?}"
    );
}
