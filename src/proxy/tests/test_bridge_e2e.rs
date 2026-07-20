//! External-bridge (`kind="webrtc"`) end-to-end tests.
//!
//! Drives the full pre-answer engine over real loopback UDP SIP plus a real
//! in-process WebRTC peer:
//!
//! ```text
//! TestUa (carrier) ──SIP/UDP──▶ CallModule::dispatch_external_bridge
//!      │                              │ http_json signaling (axum)
//!      │                              ▼
//!      │                         FakeBot ── rustrtc answerer PC
//!      └────── RTP leg ◀────────  bridge PC (loopback ICE + DTLS)
//! ```
//!
//! The bot modes cover the engine's outcome arms: `Answer` (200 OK gated on
//! ICE-connected over loopback), `Hang` (signaling never returns → CANCEL /
//! ring-timeout paths), `Fail` (HTTP 500 → mapped dispatch failure).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use axum::{
    Router,
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::config::ProxyConfig;
use crate::models::migration::Migrator;
use crate::models::trunk;
use crate::proxy::routing::{DestConfig, MatchConditions, RouteAction, RouteRule, TrunkConfig};
use crate::proxy::tests::e2e_test_server::E2eTestServer;
use crate::proxy::tests::test_ua::{TestUa, TestUaEvent};

// ─── Fake bot: HTTP signaling endpoint + rustrtc answerer PC ────────────────

#[derive(Clone, Copy, PartialEq)]
enum BotMode {
    /// Answer the offer with a real in-process rustrtc PeerConnection —
    /// ICE then completes over loopback host candidates.
    Answer,
    /// Never respond (sleep past every timeout) — keeps dispatch in flight
    /// so the CANCEL and ring-timeout arms can win the engine's select.
    Hang,
    /// HTTP 500 — exercises the dispatch-failure arm.
    Fail,
}

#[derive(Clone)]
struct BotState {
    mode: BotMode,
    offers: Arc<Mutex<Vec<String>>>,
    closes: Arc<Mutex<Vec<Value>>>,
    pcs: Arc<Mutex<Vec<Arc<rustrtc::PeerConnection>>>>,
}

struct FakeBot {
    offer_url: String,
    close_url: String,
    state: BotState,
    _handle: tokio::task::JoinHandle<()>,
}

/// The rustrtc non-trickle answerer pattern (the crate's own
/// `tests/media_flow.rs`): trigger gathering with a throwaway
/// `create_answer`, wait for completion, then produce the real answer so all
/// host candidates are inline — one-shot SDP, exactly what the bridge's
/// http_json exchange needs.
async fn answer_with_rustrtc(state: &BotState, offer_sdp: &str) -> Result<String> {
    use rustrtc::{
        MediaKind, PeerConnection, RtcConfiguration, SdpType, TransceiverDirection,
        sdp::SessionDescription,
    };
    let pc = Arc::new(PeerConnection::new(RtcConfiguration::default()));
    pc.add_transceiver(MediaKind::Audio, TransceiverDirection::SendRecv);
    let offer = SessionDescription::parse(SdpType::Offer, offer_sdp)
        .map_err(|e| anyhow!("bad offer from bridge: {e:?}"))?;
    pc.set_remote_description(offer)
        .await
        .map_err(|e| anyhow!("set_remote_description: {e}"))?;
    let _ = pc
        .create_answer()
        .await
        .map_err(|e| anyhow!("create_answer (gather trigger): {e}"))?;
    pc.wait_for_gathering_complete().await;
    let answer = pc
        .create_answer()
        .await
        .map_err(|e| anyhow!("create_answer: {e}"))?;
    pc.set_local_description(answer.clone())
        .map_err(|e| anyhow!("set_local_description: {e}"))?;
    state.pcs.lock().await.push(pc);
    Ok(answer.to_sdp_string())
}

async fn handle_offer(State(s): State<BotState>, Json(body): Json<Value>) -> impl IntoResponse {
    let offer_sdp = body["sdp"].as_str().unwrap_or_default().to_string();
    s.offers.lock().await.push(offer_sdp.clone());
    match s.mode {
        BotMode::Fail => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"err": "boom"})),
        ),
        BotMode::Hang => {
            sleep(Duration::from_secs(120)).await;
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"err": "hung"})),
            )
        }
        BotMode::Answer => match answer_with_rustrtc(&s, &offer_sdp).await {
            Ok(answer_sdp) => (
                StatusCode::OK,
                Json(json!({"sdp": answer_sdp, "pc_id": "fake-bot-1"})),
            ),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"err": e.to_string()})),
            ),
        },
    }
}

async fn handle_close(State(s): State<BotState>, Json(body): Json<Value>) -> impl IntoResponse {
    s.closes.lock().await.push(body);
    (StatusCode::OK, Json(json!({})))
}

async fn spawn_fake_bot(mode: BotMode) -> Result<FakeBot> {
    let state = BotState {
        mode,
        offers: Arc::new(Mutex::new(Vec::new())),
        closes: Arc::new(Mutex::new(Vec::new())),
        pcs: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/offer", post(handle_offer))
        .route("/close", post(handle_close))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(FakeBot {
        offer_url: format!("http://{addr}/offer"),
        close_url: format!("http://{addr}/close"),
        state,
        _handle: handle,
    })
}

// ─── Trunk fixtures ──────────────────────────────────────────────────────────

const TRUNK: &str = "bot_trunk";

/// kind_config for the webrtc trunk pointing at the fake bot. `overrides`
/// merges over the base (top-level keys only).
fn webrtc_kind_config(bot: &FakeBot, overrides: Value) -> Value {
    let mut cfg = json!({
        "signaling": "http_json",
        "endpoint_url": bot.offer_url,
        "audio_codec": "opus",
        "protocol": {
            "request_body_template": r#"{"sdp":"{offer_sdp}","type":"offer"}"#,
            "response_answer_path": "$.sdp",
            "response_session_path": "$.pc_id",
            "close_url": bot.close_url,
        },
        // No RTP flows from the TestUa caller — disable the media-inactivity
        // reaper so it can't race the assertions.
        "media_timeout_initial_ms": 0,
        "media_timeout_ms": 0,
    });
    if let (Some(base), Some(over)) = (cfg.as_object_mut(), overrides.as_object()) {
        for (k, v) in over {
            base.insert(k.clone(), v.clone());
        }
    }
    cfg
}

/// In-memory sqlite with migrations + the `rustpbx_trunks` row that
/// `fetch_external_trunk` reads at dispatch time.
async fn bridge_test_db(kind_config: &Value) -> Result<DatabaseConnection> {
    let db = Database::connect("sqlite::memory:").await?;
    Migrator::up(&db, None).await?;
    let now = chrono::Utc::now();
    trunk::ActiveModel {
        name: Set(TRUNK.to_string()),
        kind: Set("webrtc".to_string()),
        status: Set(trunk::TrunkStatus::Healthy),
        direction: Set(trunk::TrunkDirection::Bidirectional),
        is_active: Set(true),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        kind_config: Set(kind_config.clone()),
        org_id: Set("default".to_string()),
        ..Default::default()
    }
    .insert(&db)
    .await?;
    Ok(db)
}

/// Embedded-config trunk (for the routing matcher) + a route sending
/// `7xx…` callees to it. The matcher branches into the bridge dispatcher on
/// `kind="webrtc"`; the DB row above is what dispatch itself reads.
fn bridge_proxy_config(kind_config: &Value) -> ProxyConfig {
    let mut trunks = HashMap::new();
    trunks.insert(
        TRUNK.to_string(),
        TrunkConfig {
            kind: "webrtc".to_string(),
            kind_config: Some(kind_config.clone()),
            ..Default::default()
        },
    );
    let routes = vec![RouteRule {
        name: "to_bot".to_string(),
        priority: 1,
        match_conditions: MatchConditions {
            to_user: Some("^7\\d+$".to_string()),
            ..Default::default()
        },
        action: RouteAction {
            action: Some("forward".to_string()),
            dest: Some(DestConfig::Single(TRUNK.to_string())),
            ..Default::default()
        },
        ..Default::default()
    }];
    ProxyConfig {
        trunks,
        routes: Some(routes),
        ..Default::default()
    }
}

async fn start_bridge_server(kind_config: &Value) -> Result<(Arc<E2eTestServer>, TestUa)> {
    crate::proxy::bridge::signaling::register_builtins();
    let db = bridge_test_db(kind_config).await?;
    let server =
        E2eTestServer::start_with_config_and_db(bridge_proxy_config(kind_config), Some(db))
            .await?;
    let alice = server.create_ua("alice").await?;
    Ok((Arc::new(server), alice))
}

/// Carrier-style PCMU offer. The port carries no traffic — these tests
/// assert signaling, not media.
fn pcmu_offer() -> String {
    let sid = chrono::Utc::now().timestamp();
    format!(
        "v=0\r\n\
         o=- {sid} {sid} IN IP4 127.0.0.1\r\n\
         s=-\r\n\
         c=IN IP4 127.0.0.1\r\n\
         t=0 0\r\n\
         m=audio 40002 RTP/AVP 0 101\r\n\
         a=rtpmap:0 PCMU/8000\r\n\
         a=rtpmap:101 telephone-event/8000\r\n\
         a=sendrecv\r\n"
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn bridge_cancel_before_answer_returns_487() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();

    let bot = spawn_fake_bot(BotMode::Hang).await?;
    // Signaling settles (times out) 2s in — after the CANCEL below, so the
    // post-loop dispatch drain finishes promptly and the CDR lands fast.
    let cfg = webrtc_kind_config(
        &bot,
        json!({
            "signaling_timeout_ms": 2_000u64,
            "ring_timeout_ms": 30_000u64,
        }),
    );
    let (server, alice) = start_bridge_server(&cfg).await?;

    let caller = {
        let alice = alice.clone();
        tokio::spawn(async move { alice.make_call("7001", Some(pcmu_offer())).await })
    };

    // The engine sends 180 Ringing right after the gates; wait for it.
    let mut dialog_id = None;
    for _ in 0..100 {
        for ev in alice.process_dialog_events().await? {
            if let TestUaEvent::CallRinging(id) = ev {
                dialog_id = Some(id);
            }
        }
        if dialog_id.is_some() {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    let dialog_id = dialog_id.expect("no 180 Ringing within 5s");

    // CANCEL while dispatch is still hung on the bot. Look the in-flight
    // dialog up by Call-ID: the ringing event's DialogId carries the To-tag,
    // but `do_invite` registers the client dialog under its early (tag-less) ID.
    alice.cancel_pending_call(&dialog_id.call_id).await?;

    let err = caller.await?.expect_err("cancelled call must not succeed");
    assert!(
        err.to_string().contains("487"),
        "expected 487 Request Terminated, got: {err}"
    );

    let record = server
        .cdr_capture
        .wait_for_record(&dialog_id.call_id, Duration::from_secs(10))
        .await
        .expect("no CDR for cancelled bridge call");
    assert_eq!(record.details.status, "failed");
    assert_eq!(
        record.details.last_error.as_ref().map(|e| e.code),
        Some(487),
        "CDR should carry the 487: {:?}",
        record.details.last_error
    );

    server.stop();
    Ok(())
}

#[tokio::test]
async fn bridge_ring_timeout_returns_480() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();

    let bot = spawn_fake_bot(BotMode::Hang).await?;
    let cfg = webrtc_kind_config(
        &bot,
        json!({
            "signaling_timeout_ms": 2_000u64,
            "ring_timeout_ms": 1_200u64,
        }),
    );
    let (server, alice) = start_bridge_server(&cfg).await?;

    let err = alice
        .make_call("7001", Some(pcmu_offer()))
        .await
        .expect_err("ring-timeout call must not succeed");
    assert!(
        err.to_string().contains("480"),
        "expected 480 Temporarily Unavailable, got: {err}"
    );

    // CDR lands after the post-loop dispatch drain settles.
    let mut saw_480 = false;
    for _ in 0..100 {
        if server
            .cdr_capture
            .get_all_records()
            .await
            .iter()
            .any(|r| r.details.last_error.as_ref().map(|e| e.code) == Some(480))
        {
            saw_480 = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(saw_480, "no CDR with 480 recorded");

    server.stop();
    Ok(())
}

#[tokio::test]
async fn bridge_answered_call_over_loopback_ice_then_bye() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();

    let bot = spawn_fake_bot(BotMode::Answer).await?;
    // answer_on defaults to ice_connected — the 200 OK is gated on the
    // bridge↔bot PeerConnection pair actually reaching Connected.
    let cfg = webrtc_kind_config(
        &bot,
        json!({
            "signaling_timeout_ms": 10_000u64,
            "ring_timeout_ms": 20_000u64,
        }),
    );
    let (server, alice) = start_bridge_server(&cfg).await?;

    let dialog_id = alice.make_call("7001", Some(pcmu_offer())).await?;

    // The 200 OK carried the SIP-leg SDP answer.
    let answer = alice
        .get_negotiated_answer_sdp(&dialog_id)
        .await
        .expect("no answer SDP on 200 OK");
    assert!(
        answer.contains("m=audio"),
        "SIP answer has no audio m-line: {answer}"
    );

    // The bot side must also reach Connected (the 200 already proves the
    // bridge side did, since answer_on=ice_connected gates on it).
    let pc = bot
        .state
        .pcs
        .lock()
        .await
        .first()
        .cloned()
        .expect("bot never built an answer PC");
    tokio::time::timeout(Duration::from_secs(10), pc.wait_for_connected())
        .await
        .map_err(|_| anyhow!("bot PC did not reach Connected within 10s"))??;

    // BYE tears the bridge down and closes the bot session via close_url.
    alice.hangup(&dialog_id).await?;

    let mut closed = false;
    for _ in 0..100 {
        if bot
            .state
            .closes
            .lock()
            .await
            .iter()
            .any(|c| c["pc_id"] == "fake-bot-1")
        {
            closed = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(closed, "BYE did not reach the bot's close endpoint");

    let record = server
        .cdr_capture
        .wait_for_record(&dialog_id.call_id, Duration::from_secs(10))
        .await
        .expect("no CDR for answered bridge call");
    assert_eq!(record.details.status, "completed");

    server.stop();
    Ok(())
}

#[tokio::test]
async fn bridge_dispatch_failure_maps_to_503() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();

    let bot = spawn_fake_bot(BotMode::Fail).await?;
    let cfg = webrtc_kind_config(&bot, json!({ "ring_timeout_ms": 10_000u64 }));
    let (server, alice) = start_bridge_server(&cfg).await?;

    let err = alice
        .make_call("7001", Some(pcmu_offer()))
        .await
        .expect_err("dispatch-failure call must not succeed");
    assert!(
        err.to_string().contains("503"),
        "expected 503 mapped from the signaling failure, got: {err}"
    );

    // Exactly one signaling attempt reached the bot.
    assert_eq!(bot.state.offers.lock().await.len(), 1);

    let mut saw_503 = false;
    for _ in 0..100 {
        if server
            .cdr_capture
            .get_all_records()
            .await
            .iter()
            .any(|r| r.details.last_error.as_ref().map(|e| e.code) == Some(503))
        {
            saw_503 = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(saw_503, "no CDR with the mapped 503 recorded");

    server.stop();
    Ok(())
}
