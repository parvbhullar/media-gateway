//! dispatch_livekit — entry point parallel to dispatch_webrtc.
//!
//! Flow:
//!   1. Build inbound SIP-side RTP PC + SDP answer.
//!   2. Template room/identity/metadata from kind_config + DispatchContext.
//!   3. Optional webhook (Decision API): may override room/identity/metadata,
//!      may apply wait_ms, may reject the call.
//!   4. Mint JWT.
//!   5. Room::connect + publish (via client::connect_and_publish).
//!   6. Build LiveKitBridge.
//!   7. Build LiveKitTeardown.
//!   8. Return generalized DispatchOutcome.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use livekit_api::services::room::RoomClient;
use rustrtc::IceServer;
use tokio_util::sync::CancellationToken;

use crate::models::trunk;
use crate::proxy::bridge::common::{DispatchContext, build_inbound_rtp_pc};
use crate::proxy::bridge::session::{DispatchOutcome, MediaBridge};

use super::client::connect_and_publish;
use super::decision::Decision;
use super::media::LiveKitBridge;
use super::teardown::LiveKitTeardown;
use super::{template, token, webhook};

/// Carrier signal for a webhook-driven rejection. `call.rs` catches this
/// via `downcast_ref` and emits the SIP failure response with the
/// supplied code + reason rather than the generic 503.
#[derive(Debug, thiserror::Error)]
#[error("webhook rejected call: {code} {reason:?}")]
pub struct DispatchRejection {
    pub code: u16,
    pub reason: Option<String>,
}

pub async fn dispatch_livekit(
    trunk: &trunk::Model,
    invite_offer_sdp: &str,
    _global_ice_servers: Option<&[IceServer]>,
    ctx: &DispatchContext,
) -> Result<DispatchOutcome> {
    let cfg = trunk.livekit()?;

    // 1. Inbound SIP-side RTP PC + answer.
    let (sip_pc, sip_sdp_answer, sip_codec, sip_dtmf_pt) =
        build_inbound_rtp_pc(invite_offer_sdp).await?;

    // 2. Template substitution.
    let mut vars: HashMap<&str, &str> = HashMap::new();
    vars.insert("call_id", &ctx.call_id);
    vars.insert("from_user", &ctx.from_user);
    vars.insert("to_user", &ctx.to_user);
    vars.insert("did", &ctx.to_user); // alias
    vars.insert("trunk_name", &trunk.name);

    let room = template::render(&cfg.room_template, &vars)
        .map_err(|e| anyhow!("room_template: {e}"))?;
    let identity = template::render(&cfg.identity_template, &vars)
        .map_err(|e| anyhow!("identity_template: {e}"))?;
    let metadata = match &cfg.metadata_template {
        Some(t) => Some(
            // JSON-safe render: vars are JSON-escaped before
            // substitution so a quote or backslash in a SIP user-part
            // doesn't break the rendered JSON (the bot parses
            // `participant.metadata()` as JSON).
            template::render_for_json_string(t, &vars)
                .map_err(|e| anyhow!("metadata_template: {e}"))?,
        ),
        None => None,
    };

    // 3. Optional webhook with Decision API.
    let (final_room, final_identity, final_metadata) = if let Some(endpoint) =
        &cfg.dispatch_endpoint
    {
        // Need room/identity to be in vars for the webhook body.
        let mut wvars = vars.clone();
        wvars.insert("room", &room);
        wvars.insert("identity", &identity);
        let client = reqwest::Client::new();
        let decision = webhook::post(
            &client,
            webhook::WebhookInput {
                endpoint_url: endpoint,
                auth_header: cfg.dispatch_endpoint_auth_header.as_deref(),
                protocol: cfg.dispatch_endpoint_protocol.as_ref(),
                vars: &wvars,
                timeout_ms: cfg.signaling_timeout_ms.unwrap_or(5_000),
                require_ack: cfg.require_webhook_ack,
            },
        )
        .await?;
        match decision {
            Decision::Reject { code, reason } => {
                return Err(DispatchRejection { code, reason }.into());
            }
            Decision::Accept {
                room: r_o,
                identity: i_o,
                metadata: m_o,
                wait_ms,
            } => {
                if wait_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                }
                (r_o.unwrap_or(room), i_o.unwrap_or(identity), m_o.or(metadata))
            }
        }
    } else {
        (room, identity, metadata)
    };

    // 4. Mint JWT. TTL governs the SDK's auto-reconnect window after a
    //    network blip on an already-connected session (see
    //    LiveKitTrunkConfig::jwt_ttl_secs docs). Default 1800s = 30 min.
    let jwt = token::mint(token::MintInput {
        api_key: &cfg.api_key,
        api_secret: &cfg.api_secret,
        identity: &final_identity,
        room: &final_room,
        metadata: final_metadata.as_deref(),
        ttl_secs: cfg.jwt_ttl_secs.unwrap_or(30 * 60),
    })?;

    // 5. Connect + publish.
    let connected = connect_and_publish(&cfg.server_url, &jwt).await?;

    // `livekit::Room` is not `Clone` — wrap in Arc so the media bridge and
    // teardown can both observe the same session.
    let room_arc = Arc::new(connected.room);

    // 5b. Optional LiveKit-native agent dispatch. When the trunk config
    // names an agent, ask the LiveKit server to spin that agent into the
    // same room as our caller participant. The webhook path (3) and this
    // path are independent: a caller may have BOTH (webhook gets a
    // heads-up + can override room/identity, then we still dispatch the
    // named agent to the final room), OR just one. validate() refuses
    // configs that have neither, so the caller can never land in an
    // empty room with no path to a bot.
    if let Some(agent_name) = cfg.agent_name.as_deref().filter(|s| !s.trim().is_empty()) {
        let agent_client = livekit_api::services::agent_dispatch::AgentDispatchClient::with_api_key(
            &cfg.server_url,
            &cfg.api_key,
            &cfg.api_secret,
        );
        let req = livekit_protocol::CreateAgentDispatchRequest {
            agent_name: agent_name.to_string(),
            room: final_room.clone(),
            metadata: final_metadata.clone().unwrap_or_default(),
            // restart_policy=0 maps to the default JobRestartPolicy
            // (server picks its own; matches not-set semantics).
            restart_policy: 0,
            deployment: String::new(),
        };
        match agent_client.create_dispatch(req).await {
            Ok(_dispatch) => {
                tracing::info!(
                    agent = %agent_name,
                    room = %final_room,
                    "livekit agent dispatched to room"
                );
            }
            Err(e) if cfg.require_agent_dispatch => {
                // Strict mode: abort the call rather than land the caller in
                // a room without a bot. We've already opened the LiveKit
                // room (Room::connect succeeded) — close it before
                // bubbling the error so the LiveKit-side state is clean.
                tracing::warn!(
                    agent = %agent_name,
                    room = %final_room,
                    error = %e,
                    "livekit agent dispatch failed; require_agent_dispatch=true → aborting"
                );
                if let Err(close_err) = room_arc.close().await {
                    tracing::warn!(
                        error = %close_err,
                        "post-agent-dispatch-failure room.close also failed; \
                         LiveKit room may linger until its idle timeout"
                    );
                }
                return Err(anyhow!(
                    "livekit agent dispatch required but failed: {e}"
                ));
            }
            Err(e) => {
                // Soft-fail (default): failure here is logged but doesn't
                // fail the call. The room is still up and the caller is
                // still in it; the bot may simply never appear. Mirrors
                // the "soft-fail" semantics of the webhook path with
                // require_webhook_ack=false. The bot_join_timeout_ms
                // watchdog (if configured) will catch a no-show.
                tracing::warn!(
                    agent = %agent_name,
                    room = %final_room,
                    error = %e,
                    "livekit agent dispatch failed; call continues but bot may not appear"
                );
            }
        }
    }

    // 6. Build the media-plane bridge.
    let cancel = CancellationToken::new();
    let bridge = Arc::new(LiveKitBridge {
        sip_pc,
        sip_codec,
        sip_dtmf_pt,
        local_source: connected.local_source,
        _room: room_arc.clone(),
        subscribers: Arc::new(dashmap::DashMap::new()),
        events_rx: tokio::sync::Mutex::new(Some(connected.events)),
        cancel_token: cancel.clone(),
        trunk_name: trunk.name.clone(),
        bot_join_timeout_ms: cfg.bot_join_timeout_ms,
        hold_tone_hz: cfg.hold_tone_hz,
        // Default to ByCallee — overwritten by Task D if the watchdog
        // fires first. Task C uses the default for room Disconnected.
        disconnect_cause: Arc::new(parking_lot::Mutex::new(
            crate::proxy::bridge::session::BridgeHangupCause::ByCallee,
        )),
    });

    // 7. Build the signaling-plane teardown.
    let room_service =
        RoomClient::with_api_key(&cfg.server_url, &cfg.api_key, &cfg.api_secret);
    let teardown = Box::new(LiveKitTeardown {
        room: room_arc,
        room_service: Some(room_service),
        room_name: final_room.clone(),
        delete_room_on_hangup: cfg.delete_room_on_hangup,
    });

    Ok(DispatchOutcome {
        sip_sdp_answer,
        bridge: bridge as Arc<dyn MediaBridge>,
        teardown,
    })
}
