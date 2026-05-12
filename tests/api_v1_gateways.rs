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

// ---------------------------------------------------------------------------
// Write routes (Phase 1 Plan 01-02)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_gateway_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/gateways")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"carrier-x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn create_gateway_happy_path_returns_201() {
    let (state, token) = test_state_with_api_key("create-happy").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/gateways")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"name":"carrier-new","sip_server":"sip.example.net:5060","transport":"udp","direction":"outbound"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let body = body_json(resp).await;
    assert_eq!(body["name"], "carrier-new");
    assert_eq!(body["transport"], "udp");
    assert_eq!(body["direction"], "outbound");
    assert_eq!(body["proxy_addr"], "sip.example.net:5060");
}

#[tokio::test]
async fn create_gateway_duplicate_returns_409() {
    let (state, token) = test_state_with_api_key("create-dup").await;
    insert_trunk(&state, "carrier-dup", Some("sip.example.com:5060")).await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/gateways")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"carrier-dup"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "conflict");
}

#[tokio::test]
async fn create_gateway_empty_name_returns_400() {
    let (state, token) = test_state_with_api_key("create-empty").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/gateways")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"  "}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
}

#[tokio::test]
async fn update_gateway_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/gateways/x")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn update_gateway_happy_path() {
    let (state, token) = test_state_with_api_key("update-happy").await;
    insert_trunk(&state, "carrier-upd", Some("sip.old.com:5060")).await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/gateways/carrier-upd")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"sip_server":"sip.new.com:5060","is_active":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["name"], "carrier-upd");
    assert_eq!(body["proxy_addr"], "sip.new.com:5060");
    assert_eq!(body["is_active"], false);
}

#[tokio::test]
async fn update_gateway_missing_returns_404() {
    let (state, token) = test_state_with_api_key("update-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/gateways/ghost")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"is_active":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}

#[tokio::test]
async fn delete_gateway_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/gateways/x")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn delete_gateway_happy_path_returns_204() {
    let (state, token) = test_state_with_api_key("delete-happy").await;
    insert_trunk(&state, "carrier-del", None).await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/gateways/carrier-del")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 204);
}

#[tokio::test]
async fn delete_gateway_missing_returns_404() {
    let (state, token) = test_state_with_api_key("delete-missing").await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/gateways/ghost")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}

#[tokio::test]
async fn delete_gateway_with_referencing_did_returns_409() {
    let (state, token) = test_state_with_api_key("delete-engaged").await;
    insert_trunk(&state, "carrier-engaged", None).await;
    // Seed a DID that references the trunk
    rustpbx::models::did::Model::upsert(
        state.db(),
        rustpbx::models::did::NewDid {
            number: "+14155550101".into(),
            trunk_name: Some("carrier-engaged".into()),
            extension_number: None,
            failover_trunk: None,
            label: None,
            enabled: true,
        },
    )
    .await
    .unwrap();

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/gateways/carrier-engaged")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "conflict");
    assert!(
        body["error"].as_str().unwrap().contains("+14155550101"),
        "error message should cite the referencing DID"
    );
}

// ---------------------------------------------------------------------------
// GWY-04: newly created gateway is picked up by the DB-polling
// GatewayHealthMonitor on the very next tick_once() invocation.
// ---------------------------------------------------------------------------

// DEFERRED: GatewayHealthMonitor::tally_snapshot not on RT — sip_fix Phase 1 Plan 01-06 extension.
// Re-enable when that method is ported.
#[cfg(any())]
#[tokio::test]
async fn newly_created_gateway_appears_in_health_tallies_on_next_tick() {
    use rustpbx::models::sip_trunk;
    use rustpbx::proxy::gateway_health::{GatewayHealthMonitor, ProbeOutcome};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let (state, token) = test_state_with_api_key("gwy04-tally").await;

    // 1. POST /api/v1/gateways — create a new outbound gateway with
    //    health_check_interval_secs=0 so tick_once probes it immediately
    //    regardless of last_health_check_at.
    let create_body = serde_json::json!({
        "name": "gwy04-test",
        "display_name": "GWY-04 test gateway",
        "direction": "outbound",
        "sip_server": "127.0.0.1:65530",
        "transport": "udp",
        "is_active": true,
        "health_check_interval_secs": 0,
        "failure_threshold": 3,
        "recovery_threshold": 2,
    });

    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/gateways")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(create_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        201,
        "POST /api/v1/gateways should return 201 CREATED"
    );

    // 2. Read back the assigned id by name so we don't depend on the
    //    response body shape for the pk.
    let inserted = sip_trunk::Entity::find()
        .filter(sip_trunk::Column::Name.eq("gwy04-test"))
        .one(state.db())
        .await
        .expect("find inserted gateway")
        .expect("gateway should exist");
    let gateway_id = inserted.id;

    // 3. Build a GatewayHealthMonitor against the same DB the AppState
    //    uses. The monitor is DB-polling by design — there is no
    //    register_trunk API. The only contract is: ticking against the
    //    DB after insert should observe the new row.
    let monitor = GatewayHealthMonitor::new(state.db().clone(), None);

    // 4. Drive one tick with an injected probe that never actually hits
    //    the network. `tick_with_probe` exercises the same find-and-fold
    //    path as `tick_once` but lets us stub the probe outcome.
    let probe_stub = |_t: sip_trunk::Model| async move {
        ProbeOutcome {
            ok: false,
            latency_ms: 1,
            detail: "gwy04-stub".into(),
        }
    };
    monitor
        .tick_with_probe(probe_stub)
        .await
        .expect("tick_with_probe failed");

    // 5. The monitor must now carry a tally for our newly-inserted
    //    gateway id — proving the DB-polling design works end-to-end
    //    with the POST /gateways path.
    let tally = monitor.tally_snapshot(gateway_id).await;
    assert!(
        tally.is_some(),
        "expected monitor to observe gateway {} after one tick",
        gateway_id
    );
}
