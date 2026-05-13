//! Integration tests for `/api/v1/trunks/{name}/media` (Phase 3
//! Plan 03-04 — TSUB-03).
//!
//! Matrix per `03-CONTEXT.md` IT-01 convention:
//!
//!   1. 401 without Bearer token
//!   2. GET returns defaults when media_config column is NULL (D-11)
//!   3. PUT happy + GET round-trip
//!   4. PUT invalid codec (uppercase) → 400 (D-10)
//!   5. PUT invalid dtmf_mode → 400 (D-12)
//!   6. PUT invalid srtp → 400 (D-12)
//!   7. PUT invalid media_mode → 400 (D-12)
//!   8. GET on missing parent trunk → 404
//!   9. PUT with all-null enums stores Some(json), not NULL (D-11)

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
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
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
        member_am.insert(db).await.expect("insert trunk group member");
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
async fn get_media_requires_auth() {
    let state = test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/any-tg/media")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

// =========================================================================
// 2. GET returns defaults when column is NULL (D-11)
// =========================================================================

#[tokio::test]
async fn get_media_returns_defaults_when_column_null() {
    let (state, token) =
        test_state_with_api_key("media-get-defaults").await;
    insert_trunk(&state, "gw-media-null").await;
    // insert_trunk_group does NOT set media_config — column stays NULL
    insert_trunk_group(state.db(), "tg-media-null", &["gw-media-null"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-media-null/media")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["codecs"], json!([]));
    assert_eq!(body["dtmf_mode"], Value::Null);
    assert_eq!(body["srtp"], Value::Null);
    assert_eq!(body["media_mode"], Value::Null);
}

// =========================================================================
// 3. PUT happy + GET round-trip
// =========================================================================

#[tokio::test]
async fn put_media_happy_round_trips_full_config() {
    let (state, token) =
        test_state_with_api_key("media-put-happy").await;
    insert_trunk(&state, "gw-media-happy").await;
    insert_trunk_group(state.db(), "tg-media-happy", &["gw-media-happy"]).await;

    let payload = json!({
        "codecs": ["pcmu", "pcma"],
        "dtmf_mode": "rfc2833",
        "srtp": null,
        "media_mode": null
    });

    // PUT
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-media-happy/media")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let put_body = body_json(resp).await;
    assert_eq!(put_body["codecs"], json!(["pcmu", "pcma"]));
    assert_eq!(put_body["dtmf_mode"], "rfc2833");
    assert_eq!(put_body["srtp"], Value::Null);
    assert_eq!(put_body["media_mode"], Value::Null);

    // GET round-trip
    let app2 = rustpbx::app::create_router(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/tg-media-happy/media")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    let get_body = body_json(resp2).await;
    assert_eq!(get_body["codecs"], json!(["pcmu", "pcma"]));
    assert_eq!(get_body["dtmf_mode"], "rfc2833");
}

// =========================================================================
// 4. PUT invalid codec (uppercase) → 400 (D-10)
// =========================================================================

#[tokio::test]
async fn put_media_invalid_codec_uppercase_returns_400() {
    let (state, token) =
        test_state_with_api_key("media-put-upper").await;
    insert_trunk(&state, "gw-media-upper").await;
    insert_trunk_group(state.db(), "tg-media-upper", &["gw-media-upper"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-media-upper/media")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "codecs": ["PCMU"],
                        "dtmf_mode": null,
                        "srtp": null,
                        "media_mode": null
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
    assert!(
        msg.contains("lowercase"),
        "expected error to mention 'lowercase', got: {}",
        msg
    );
}

// =========================================================================
// 5. PUT invalid dtmf_mode → 400 (D-12)
// =========================================================================

#[tokio::test]
async fn put_media_invalid_dtmf_mode_returns_400() {
    let (state, token) =
        test_state_with_api_key("media-put-dtmf").await;
    insert_trunk(&state, "gw-media-dtmf").await;
    insert_trunk_group(state.db(), "tg-media-dtmf", &["gw-media-dtmf"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-media-dtmf/media")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "codecs": [],
                        "dtmf_mode": "morse",
                        "srtp": null,
                        "media_mode": null
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
    assert!(
        msg.contains("dtmf_mode"),
        "expected error to mention 'dtmf_mode', got: {}",
        msg
    );
}

// =========================================================================
// 6. PUT invalid srtp → 400 (D-12)
// =========================================================================

#[tokio::test]
async fn put_media_invalid_srtp_returns_400() {
    let (state, token) =
        test_state_with_api_key("media-put-srtp").await;
    insert_trunk(&state, "gw-media-srtp").await;
    insert_trunk_group(state.db(), "tg-media-srtp", &["gw-media-srtp"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-media-srtp/media")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "codecs": [],
                        "dtmf_mode": null,
                        "srtp": "sometimes",
                        "media_mode": null
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
    assert!(
        msg.contains("srtp"),
        "expected error to mention 'srtp', got: {}",
        msg
    );
}

// =========================================================================
// 7. PUT invalid media_mode → 400 (D-12)
// =========================================================================

#[tokio::test]
async fn put_media_invalid_media_mode_returns_400() {
    let (state, token) =
        test_state_with_api_key("media-put-mode").await;
    insert_trunk(&state, "gw-media-mode").await;
    insert_trunk_group(state.db(), "tg-media-mode", &["gw-media-mode"]).await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-media-mode/media")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "codecs": [],
                        "dtmf_mode": null,
                        "srtp": null,
                        "media_mode": "holographic"
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
    assert!(
        msg.contains("media_mode"),
        "expected error to mention 'media_mode', got: {}",
        msg
    );
}

// =========================================================================
// 8. GET on missing parent trunk → 404
// =========================================================================

#[tokio::test]
async fn get_media_parent_missing_returns_404() {
    let (state, token) =
        test_state_with_api_key("media-parent-missing").await;

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/trunks/no-such-tg/media")
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

// =========================================================================
// 9. PUT with all-null enums stores Some(json), not NULL (D-11)
// =========================================================================

#[tokio::test]
async fn put_media_with_all_nulls_stores_some_not_null() {
    let (state, token) =
        test_state_with_api_key("media-allnull").await;
    insert_trunk(&state, "gw-media-allnull").await;
    insert_trunk_group(state.db(), "tg-media-allnull", &["gw-media-allnull"]).await;

    // PUT with all-null enum fields
    let app = rustpbx::app::create_router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/trunks/tg-media-allnull/media")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "codecs": [],
                        "dtmf_mode": null,
                        "srtp": null,
                        "media_mode": null
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    // D-11: verify the DB column is Some(json), not NULL
    use rustpbx::models::trunk_group::{Column as TgCol, Entity as TgEntity};
    let row = TgEntity::find()
        .filter(TgCol::Name.eq("tg-media-allnull"))
        .one(state.db())
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.media_config.is_some(),
        "D-11: PUT must store Some(json), not NULL in the media_config column"
    );
    let stored = row.media_config.unwrap();
    assert_eq!(stored["codecs"], json!([]));
    assert_eq!(stored["dtmf_mode"], Value::Null);
}
