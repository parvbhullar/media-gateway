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
            template::render(t, &vars).map_err(|e| anyhow!("metadata_template: {e}"))?,
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

    // 4. Mint JWT.
    let jwt = token::mint(token::MintInput {
        api_key: &cfg.api_key,
        api_secret: &cfg.api_secret,
        identity: &final_identity,
        room: &final_room,
        metadata: final_metadata.as_deref(),
        ttl_secs: 15 * 60,
    })?;

    // 5. Connect + publish.
    let connected = connect_and_publish(&cfg.server_url, &jwt).await?;

    // `livekit::Room` is not `Clone` — wrap in Arc so the media bridge and
    // teardown can both observe the same session.
    let room_arc = Arc::new(connected.room);

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
