//! Integration tests for `/api/v1/routing/tables/{name}/records[/{record_id}]`
//! (Phase 6 Plan 06-03 — RTE-02 / IT-01).
//!
//! Matrix per `06-CONTEXT.md` (D-02..D-04, D-08..D-12, D-16, D-18, D-24, D-28)
//! and Plan 06-03 <behavior>.
//!
//! NOTE: This test file is parallel-safe with Plan 06-02 (which owns
//! `routing_tables.rs` handlers). To avoid coupling, we seed routing tables
//! by inserting directly into the `supersip_routing_tables` entity rather
//! than via the (still-stub) `POST /routing/tables` endpoint.

use axum::{
    body::Body,
    http::{Request, header},
};
use chrono::Utc;
use rustpbx::models::routing_tables;
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::{test_state_empty, test_state_with_api_key};

// ─── Fixture helpers ─────────────────────────────────────────────────────

async fn seed_routing_table(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    direction: &str,
) -> routing_tables::Model {
    let now = Utc::now();
    let am = routing_tables::ActiveModel {
        name: Set(name.to_string()),
        description: Set(None),
        direction: Set(direction.to_string()),
        priority: Set(100),
        is_active: Set(true),
        records: Set(serde_json::json!([])),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(db).await.expect("insert routing table")
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse json")
}

fn auth(token: &str) -> String {
    format!("Bearer {}", token)
}

async fn post_record(
    state: rustpbx::app::AppState,
    token: &str,
    table: &str,
    body: Value,
) -> axum::response::Response {
    let app = rustpbx::app::create_router(state);
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/routing/tables/{}/records", table))
            .header(header::AUTHORIZATION, auth(token))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn list_records(
    state: rustpbx::app::AppState,
    token: &str,
    table: &str,
) -> axum::response::Response {
    let app = rustpbx::app::create_router(state);
    app.oneshot(
        Request::builder()
            .uri(format!("/api/v1/routing/tables/{}/records", table))
            .header(header::AUTHORIZATION, auth(token))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

// =========================================================================
// 1. list_records_unauthenticated_returns_401
// =========================================================================

#[tokio::test]
async fn list_records_unauthenticated_returns_401() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/routing/tables/any/records")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
}

// =========================================================================
// 2. list_records_missing_table_returns_404
// =========================================================================

#[tokio::test]
async fn list_records_missing_table_returns_404() {
    let (state, token) = test_state_with_api_key("rr-list-miss").await;
    let resp = list_records(state, &token, "no-such-table").await;
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

// =========================================================================
// 3. get_record_missing_record_id_returns_404
// =========================================================================

#[tokio::test]
async fn get_record_missing_record_id_returns_404() {
    let (state, token) = test_state_with_api_key("rr-get-miss").await;
    seed_routing_table(state.db(), "tbl-getmiss", "outbound").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/routing/tables/tbl-getmiss/records/00000000-0000-0000-0000-000000000000")
                .header(header::AUTHORIZATION, auth(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

// =========================================================================
// 4. delete_record_missing_returns_404
// =========================================================================

#[tokio::test]
async fn delete_record_missing_returns_404() {
    let (state, token) = test_state_with_api_key("rr-del-miss").await;
    seed_routing_table(state.db(), "tbl-delmiss", "outbound").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/routing/tables/tbl-delmiss/records/00000000-0000-0000-0000-000000000000")
                .header(header::AUTHORIZATION, auth(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

// =========================================================================
// 5. post_lpm_record_returns_201_with_uuid
// =========================================================================

#[tokio::test]
async fn post_lpm_record_returns_201_with_uuid() {
    let (state, token) = test_state_with_api_key("rr-lpm").await;
    seed_routing_table(state.db(), "tbl-lpm", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-lpm",
        json!({
            "match": {"type": "lpm", "prefix": "+1415"},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
    let body = body_json(resp).await;
    let rid = body["record_id"].as_str().expect("record_id present");
    // UUID v4 36-char form
    assert_eq!(rid.len(), 36, "record_id should be a 36-char UUID, got {}", rid);
    assert_eq!(body["position"], 0);
}

// =========================================================================
// 6. post_exact_match_record_returns_201
// =========================================================================

#[tokio::test]
async fn post_exact_match_record_returns_201() {
    let (state, token) = test_state_with_api_key("rr-exact").await;
    seed_routing_table(state.db(), "tbl-exact", "inbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-exact",
        json!({
            "match": {"type": "exact_match", "value": "+18005551212"},
            "target": {"kind": "gateway", "name": "gw1"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
}

// =========================================================================
// 7. post_regex_record_validates_at_write_time
// =========================================================================

#[tokio::test]
async fn post_regex_record_validates_at_write_time() {
    let (state, token) = test_state_with_api_key("rr-regex").await;
    seed_routing_table(state.db(), "tbl-regex", "outbound").await;

    // Invalid regex → 400
    let resp_bad = post_record(
        state.clone(),
        &token,
        "tbl-regex",
        json!({
            "match": {"type": "regex", "pattern": "[invalid("},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp_bad.status(), axum::http::StatusCode::BAD_REQUEST);

    // Valid regex → 201
    let resp_ok = post_record(
        state,
        &token,
        "tbl-regex",
        json!({
            "match": {"type": "regex", "pattern": "^\\+1[0-9]{10}$"},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp_ok.status(), axum::http::StatusCode::CREATED);
}

// =========================================================================
// 8. post_compare_record_returns_201 (eq + in/range)
// =========================================================================

#[tokio::test]
async fn post_compare_record_returns_201() {
    let (state, token) = test_state_with_api_key("rr-cmp").await;
    seed_routing_table(state.db(), "tbl-cmp", "outbound").await;

    let resp1 = post_record(
        state.clone(),
        &token,
        "tbl-cmp",
        json!({
            "match": {"type": "compare", "op": "eq", "value": 11},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp1.status(), axum::http::StatusCode::CREATED);

    let resp2 = post_record(
        state,
        &token,
        "tbl-cmp",
        json!({
            "match": {"type": "compare", "op": "in", "value": [7, 15]},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp2.status(), axum::http::StatusCode::CREATED);
}

// =========================================================================
// 9. post_http_query_record_returns_201
// =========================================================================

#[tokio::test]
async fn post_http_query_record_returns_201() {
    let (state, token) = test_state_with_api_key("rr-http").await;
    seed_routing_table(state.db(), "tbl-http", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-http",
        json!({
            "match": {"type": "http_query", "url": "https://example.com/route", "timeout_ms": 1500},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
}

// =========================================================================
// 10. list_records_returns_position_ordered
// =========================================================================

#[tokio::test]
async fn list_records_returns_position_ordered() {
    let (state, token) = test_state_with_api_key("rr-ord").await;
    seed_routing_table(state.db(), "tbl-ord", "outbound").await;

    for prefix in &["+1", "+44", "+91"] {
        let resp = post_record(
            state.clone(),
            &token,
            "tbl-ord",
            json!({
                "match": {"type": "lpm", "prefix": *prefix},
                "target": {"kind": "trunk_group", "name": "us"},
            }),
        )
        .await;
        assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
    }

    let resp = list_records(state, &token, "tbl-ord").await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["position"], 0);
    assert_eq!(arr[1]["position"], 1);
    assert_eq!(arr[2]["position"], 2);
    assert_eq!(arr[0]["match"]["prefix"], "+1");
    assert_eq!(arr[2]["match"]["prefix"], "+91");
}

// =========================================================================
// 11. post_record_without_position_appends_at_end
// =========================================================================

#[tokio::test]
async fn post_record_without_position_appends_at_end() {
    let (state, token) = test_state_with_api_key("rr-app").await;
    seed_routing_table(state.db(), "tbl-app", "outbound").await;

    for _ in 0..2 {
        post_record(
            state.clone(),
            &token,
            "tbl-app",
            json!({
                "match": {"type": "lpm", "prefix": "+1"},
                "target": {"kind": "trunk_group", "name": "us"},
            }),
        )
        .await;
    }

    let resp = post_record(
        state.clone(),
        &token,
        "tbl-app",
        json!({
            "match": {"type": "exact_match", "value": "999"},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["position"], 2);
}

// =========================================================================
// 12. post_record_with_position_inserts_and_shifts
// =========================================================================

#[tokio::test]
async fn post_record_with_position_inserts_and_shifts() {
    let (state, token) = test_state_with_api_key("rr-ins").await;
    seed_routing_table(state.db(), "tbl-ins", "outbound").await;

    for prefix in &["+1", "+44"] {
        post_record(
            state.clone(),
            &token,
            "tbl-ins",
            json!({
                "match": {"type": "lpm", "prefix": *prefix},
                "target": {"kind": "trunk_group", "name": "us"},
            }),
        )
        .await;
    }

    // Insert at position 1 → existing pos-1 should shift to pos-2
    let resp = post_record(
        state.clone(),
        &token,
        "tbl-ins",
        json!({
            "match": {"type": "lpm", "prefix": "+91"},
            "target": {"kind": "trunk_group", "name": "us"},
            "position": 1,
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["position"], 1);

    let resp = list_records(state, &token, "tbl-ins").await;
    let body = body_json(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["match"]["prefix"], "+1");
    assert_eq!(arr[1]["match"]["prefix"], "+91");
    assert_eq!(arr[2]["match"]["prefix"], "+44");
    assert_eq!(arr[0]["position"], 0);
    assert_eq!(arr[1]["position"], 1);
    assert_eq!(arr[2]["position"], 2);
}

// =========================================================================
// 13. delete_record_does_not_renumber_remaining_positions
// =========================================================================

#[tokio::test]
async fn delete_record_does_not_renumber_remaining_positions() {
    let (state, token) = test_state_with_api_key("rr-delkeep").await;
    seed_routing_table(state.db(), "tbl-delkeep", "outbound").await;

    let mut ids = Vec::new();
    for prefix in &["+1", "+44", "+91"] {
        let r = post_record(
            state.clone(),
            &token,
            "tbl-delkeep",
            json!({
                "match": {"type": "lpm", "prefix": *prefix},
                "target": {"kind": "trunk_group", "name": "us"},
            }),
        )
        .await;
        let body = body_json(r).await;
        ids.push(body["record_id"].as_str().unwrap().to_string());
    }

    // Delete the middle (pos=1)
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/api/v1/routing/tables/tbl-delkeep/records/{}",
                    ids[1]
                ))
                .header(header::AUTHORIZATION, auth(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);

    let resp = list_records(state, &token, "tbl-delkeep").await;
    let body = body_json(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Survivors keep their original positions (sparse, stable)
    assert_eq!(arr[0]["position"], 0);
    assert_eq!(arr[1]["position"], 2);
}

// =========================================================================
// 14. put_record_preserves_record_id_and_position
// =========================================================================

#[tokio::test]
async fn put_record_preserves_record_id_and_position() {
    let (state, token) = test_state_with_api_key("rr-put").await;
    seed_routing_table(state.db(), "tbl-put", "outbound").await;

    let post_resp = post_record(
        state.clone(),
        &token,
        "tbl-put",
        json!({
            "match": {"type": "lpm", "prefix": "+1"},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    let body = body_json(post_resp).await;
    let rid = body["record_id"].as_str().unwrap().to_string();
    let original_pos = body["position"].as_i64().unwrap();

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!(
                    "/api/v1/routing/tables/tbl-put/records/{}",
                    rid
                ))
                .header(header::AUTHORIZATION, auth(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "match": {"type": "exact_match", "value": "+18005551212"},
                        "target": {"kind": "gateway", "name": "gw2"},
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let updated = body_json(resp).await;
    assert_eq!(updated["record_id"], rid);
    assert_eq!(updated["position"].as_i64().unwrap(), original_pos);
    assert_eq!(updated["match"]["type"], "exact_match");
    assert_eq!(updated["target"]["kind"], "gateway");
}

// =========================================================================
// 15. post_two_defaults_returns_400
// =========================================================================

#[tokio::test]
async fn post_two_defaults_returns_400() {
    let (state, token) = test_state_with_api_key("rr-2def").await;
    seed_routing_table(state.db(), "tbl-2def", "outbound").await;

    let r1 = post_record(
        state.clone(),
        &token,
        "tbl-2def",
        json!({
            "match": {"type": "lpm", "prefix": "+1"},
            "target": {"kind": "trunk_group", "name": "us"},
            "is_default": true,
        }),
    )
    .await;
    assert_eq!(r1.status(), axum::http::StatusCode::CREATED);

    let r2 = post_record(
        state,
        &token,
        "tbl-2def",
        json!({
            "match": {"type": "lpm", "prefix": "+44"},
            "target": {"kind": "trunk_group", "name": "uk"},
            "is_default": true,
        }),
    )
    .await;
    assert_eq!(r2.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 16. put_default_when_other_default_exists_returns_400
// =========================================================================

#[tokio::test]
async fn put_default_when_other_default_exists_returns_400() {
    let (state, token) = test_state_with_api_key("rr-putdef").await;
    seed_routing_table(state.db(), "tbl-putdef", "outbound").await;

    // First record: is_default=true
    post_record(
        state.clone(),
        &token,
        "tbl-putdef",
        json!({
            "match": {"type": "lpm", "prefix": "+1"},
            "target": {"kind": "trunk_group", "name": "us"},
            "is_default": true,
        }),
    )
    .await;

    // Second record: not default
    let r = post_record(
        state.clone(),
        &token,
        "tbl-putdef",
        json!({
            "match": {"type": "lpm", "prefix": "+44"},
            "target": {"kind": "trunk_group", "name": "uk"},
        }),
    )
    .await;
    let body = body_json(r).await;
    let rid = body["record_id"].as_str().unwrap().to_string();

    // PUT trying to make second one default → 400
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!(
                    "/api/v1/routing/tables/tbl-putdef/records/{}",
                    rid
                ))
                .header(header::AUTHORIZATION, auth(&token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "match": {"type": "lpm", "prefix": "+44"},
                        "target": {"kind": "trunk_group", "name": "uk"},
                        "is_default": true,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 17. post_target_reject_with_code_and_reason_returns_201
// =========================================================================

#[tokio::test]
async fn post_target_reject_with_code_and_reason_returns_201() {
    let (state, token) = test_state_with_api_key("rr-rej").await;
    seed_routing_table(state.db(), "tbl-rej", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-rej",
        json!({
            "match": {"type": "lpm", "prefix": "+999"},
            "target": {"kind": "reject", "code": 503, "reason": "Service Unavailable"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
}

// =========================================================================
// 18. post_target_next_table_returns_201
// =========================================================================

#[tokio::test]
async fn post_target_next_table_returns_201() {
    let (state, token) = test_state_with_api_key("rr-nxt").await;
    seed_routing_table(state.db(), "tbl-nxt", "outbound").await;
    seed_routing_table(state.db(), "other-tbl", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-nxt",
        json!({
            "match": {"type": "lpm", "prefix": "+1"},
            "target": {"kind": "next_table", "name": "other-tbl"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
}

// =========================================================================
// 19. post_regex_pattern_too_long_returns_400
// =========================================================================

#[tokio::test]
async fn post_regex_pattern_too_long_returns_400() {
    let (state, token) = test_state_with_api_key("rr-rx-long").await;
    seed_routing_table(state.db(), "tbl-rxlong", "outbound").await;
    let big = "a".repeat(4097);
    let resp = post_record(
        state,
        &token,
        "tbl-rxlong",
        json!({
            "match": {"type": "regex", "pattern": big},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 20. post_http_query_url_localhost_returns_400 (SSRF mitigation)
// =========================================================================

#[tokio::test]
async fn post_http_query_url_localhost_returns_400() {
    let (state, token) = test_state_with_api_key("rr-ssrf-lo").await;
    seed_routing_table(state.db(), "tbl-ssrf-lo", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-ssrf-lo",
        json!({
            "match": {"type": "http_query", "url": "http://127.0.0.1/x"},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 21. post_http_query_url_private_ip_returns_400
// =========================================================================

#[tokio::test]
async fn post_http_query_url_private_ip_returns_400() {
    let (state, token) = test_state_with_api_key("rr-ssrf-pi").await;
    seed_routing_table(state.db(), "tbl-ssrf-pi", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-ssrf-pi",
        json!({
            "match": {"type": "http_query", "url": "http://10.0.0.1/x"},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 22. post_http_query_url_non_http_scheme_returns_400
// =========================================================================

#[tokio::test]
async fn post_http_query_url_non_http_scheme_returns_400() {
    let (state, token) = test_state_with_api_key("rr-ssrf-sch").await;
    seed_routing_table(state.db(), "tbl-ssrf-sch", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-ssrf-sch",
        json!({
            "match": {"type": "http_query", "url": "file:///etc/passwd"},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 23. post_http_query_timeout_above_5000_returns_400
// =========================================================================

#[tokio::test]
async fn post_http_query_timeout_above_5000_returns_400() {
    let (state, token) = test_state_with_api_key("rr-tmo").await;
    seed_routing_table(state.db(), "tbl-tmo", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-tmo",
        json!({
            "match": {"type": "http_query", "url": "https://example.com/route", "timeout_ms": 9000},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
}

// =========================================================================
// 24. post_unknown_match_type_returns_400
// =========================================================================

#[tokio::test]
async fn post_unknown_match_type_returns_400() {
    let (state, token) = test_state_with_api_key("rr-bad-mt").await;
    seed_routing_table(state.db(), "tbl-bad-mt", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-bad-mt",
        json!({
            "match": {"type": "weird", "x": 1},
            "target": {"kind": "trunk_group", "name": "us"},
        }),
    )
    .await;
    // Either rejected at deserialization (axum returns 400/422) or by handler
    // validation. Either way must be a 4xx client error.
    let s = resp.status().as_u16();
    assert!(
        (400..500).contains(&s),
        "expected 4xx for unknown match type, got {}",
        s
    );
}

// =========================================================================
// 25. post_unknown_target_kind_returns_400
// =========================================================================

#[tokio::test]
async fn post_unknown_target_kind_returns_400() {
    let (state, token) = test_state_with_api_key("rr-bad-tk").await;
    seed_routing_table(state.db(), "tbl-bad-tk", "outbound").await;
    let resp = post_record(
        state,
        &token,
        "tbl-bad-tk",
        json!({
            "match": {"type": "lpm", "prefix": "+1"},
            "target": {"kind": "weird-kind", "name": "x"},
        }),
    )
    .await;
    let s = resp.status().as_u16();
    assert!(
        (400..500).contains(&s),
        "expected 4xx for unknown target kind, got {}",
        s
    );
}
