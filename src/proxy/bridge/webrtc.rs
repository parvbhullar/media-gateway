//! Provider-agnostic SIP↔WebRTC bridge dispatcher.
//!
//! See plan: `/home/anuj/.claude/plans/imperative-sauteeing-cake.md` (Phase 6).
//!
//! Given a WebRTC trunk row and the inbound SIP INVITE's SDP offer, this
//! module:
//!
//! 1. Resolves the configured signaling adapter via [`signaling::lookup`]
//!    (looking up the trunk's `kind_config.signaling` field).
//! 2. Builds an inbound `TransportMode::Rtp` `PeerConnection` and feeds it
//!    the SIP-side offer SDP, producing an SDP answer for the INVITE.
//! 3. Builds an outbound `TransportMode::WebRtc` `PeerConnection` (offerer)
//!    using the trunk's per-row ICE servers (falling back to the global ICE
//!    list).
//! 4. Calls into the adapter to drive offer→answer with the remote
//!    signaling peer.
//! 5. Sets the WebRTC PC's remote description to the negotiated answer.
//! 6. Wires both PCs into a [`BridgePeer`] and arms it with Opus↔PCMU
//!    transcoding via `setup_bridge_with_codecs`.
//!
//! No vendor names appear in this file — all vendor-specific logic is
//! confined to the adapter (selected by name) and the per-trunk `protocol`
//! blob it interprets.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use rustrtc::{
    IceServer, MediaKind, PeerConnection, RtcConfiguration, RtpCodecParameters,
    SdpType, TransceiverDirection, TransportMode,
    config::{AudioCapability, MediaCapabilities, SdpCompatibilityMode, VideoCapability},
    sdp::SessionDescription,
};
use serde_json::Value;

use crate::media::bridge::BridgePeer;
use crate::models::trunk;

use super::signaling::{
    self, NegotiateOutcome, SessionHandle, SignalingContext, WebRtcSignalingAdapter,
};

/// Successful dispatch outcome — the SDP answer to return on the SIP INVITE,
/// plus the constructed bridge and the adapter+session for later teardown.
pub struct DispatchOutcome {
    /// SDP answer to write back into the SIP 200 OK for the inbound INVITE.
    pub sip_sdp_answer: String,
    /// The wired bridge connecting the inbound RTP leg to the outbound
    /// WebRTC leg.
    pub bridge: Arc<BridgePeer>,
    /// The signaling adapter used; preserved so the caller can invoke
    /// `adapter.close(ctx, &session)` on hangup.
    pub adapter: Arc<dyn WebRtcSignalingAdapter>,
    /// Opaque adapter-defined session handle.
    pub session: SessionHandle,
}

/// Resolve the effective ICE-server list for this trunk.
///
/// Precedence: per-trunk `kind_config.ice_servers` (a JSON array) wins; if
/// absent or empty, falls back to `global_ice_servers`; otherwise empty
/// (host candidates only).
fn resolve_ice_servers(
    per_trunk: Option<&Value>,
    global_ice_servers: Option<&[IceServer]>,
) -> Result<Vec<IceServer>> {
    if let Some(v) = per_trunk
        && !v.is_null()
    {
        let parsed: Vec<IceServer> = serde_json::from_value(v.clone())
            .map_err(|e| anyhow!("failed to parse per-trunk ice_servers: {e}"))?;
        if !parsed.is_empty() {
            return Ok(parsed);
        }
    }
    Ok(global_ice_servers.map(|s| s.to_vec()).unwrap_or_default())
}

fn audio_capability_for(codec: &str) -> Result<AudioCapability> {
    match codec.to_ascii_lowercase().as_str() {
        "opus" => Ok(AudioCapability::opus()),
        "g722" => Ok(AudioCapability::g722()),
        other => Err(anyhow!(
            "audio_codec '{other}' not supported (allowed: opus, g722)"
        )),
    }
}

fn codec_params_from_capability(cap: &AudioCapability) -> RtpCodecParameters {
    RtpCodecParameters {
        payload_type: cap.payload_type,
        clock_rate: cap.clock_rate,
        channels: cap.channels,
    }
}

/// Build the outbound WebRTC PeerConnection (offerer role) — fresh PC with a
/// single audio SendRecv transceiver using the requested codec.
pub fn build_outbound_webrtc_pc(
    ice: Option<&Value>,
    audio_codec: &str,
    global_ice_servers: Option<&[IceServer]>,
) -> Result<PeerConnection> {
    let ice_servers = resolve_ice_servers(ice, global_ice_servers)?;
    let audio_cap = audio_capability_for(audio_codec)?;
    let cfg = RtcConfiguration {
        transport_mode: TransportMode::WebRtc,
        ice_servers,
        media_capabilities: Some(MediaCapabilities {
            audio: vec![audio_cap],
            video: Vec::<VideoCapability>::new(),
            application: None,
        }),
        sdp_compatibility: SdpCompatibilityMode::Standard,
        ..Default::default()
    };
    let pc = PeerConnection::new(cfg);
    pc.add_transceiver(MediaKind::Audio, TransceiverDirection::SendRecv);
    Ok(pc)
}

/// Build the inbound SIP-side RTP PeerConnection from the INVITE's offer
/// SDP, producing an SDP answer suitable for the 200 OK.
///
/// Returns the configured PC together with the answer SDP string.
async fn build_inbound_rtp_pc(invite_offer_sdp: &str) -> Result<(PeerConnection, String)> {
    let cfg = RtcConfiguration {
        transport_mode: TransportMode::Rtp,
        media_capabilities: Some(MediaCapabilities {
            audio: vec![AudioCapability::pcmu(), AudioCapability::pcma()],
            video: Vec::<VideoCapability>::new(),
            application: None,
        }),
        sdp_compatibility: SdpCompatibilityMode::Standard,
        ..Default::default()
    };
    let pc = PeerConnection::new(cfg);
    let offer = SessionDescription::parse(SdpType::Offer, invite_offer_sdp)
        .map_err(|e| anyhow!("failed to parse INVITE offer SDP: {e:?}"))?;
    pc.set_remote_description(offer)
        .await
        .map_err(|e| anyhow!("set_remote_description failed on RTP leg: {e}"))?;
    let answer = pc
        .create_answer()
        .await
        .map_err(|e| anyhow!("create_answer failed on RTP leg: {e}"))?;
    pc.set_local_description(answer)
        .map_err(|e| anyhow!("set_local_description failed on RTP leg: {e}"))?;
    let answer_sdp = pc
        .local_description()
        .ok_or_else(|| anyhow!("RTP leg has no local description after set_local_description"))?
        .to_sdp_string();
    Ok((pc, answer_sdp))
}

/// Provider-agnostic dispatcher for `kind="webrtc"` trunks.
///
/// On success, returns the SDP answer to send back on the SIP INVITE (200
/// OK), the wired [`BridgePeer`], and the adapter+session handle so the
/// caller can invoke `adapter.close(ctx, &session)` at hangup time.
///
/// `global_ice_servers` is the process-wide ICE list (from `config.toml`'s
/// `[ice_servers]`). It's consulted only if the trunk's `kind_config.ice_servers`
/// is missing or empty.
pub async fn dispatch_webrtc(
    trunk: &trunk::Model,
    invite_offer_sdp: &str,
    global_ice_servers: Option<&[IceServer]>,
) -> Result<DispatchOutcome> {
    let cfg = trunk.webrtc()?;
    let adapter = signaling::lookup(&cfg.signaling).ok_or_else(|| {
        anyhow!(
            "signaling adapter '{}' not registered for trunk '{}'",
            cfg.signaling,
            trunk.name
        )
    })?;

    // 1. Inbound RTP leg + SDP answer for the SIP 200 OK.
    let (rtp_pc, sip_sdp_answer) = build_inbound_rtp_pc(invite_offer_sdp).await?;

    // 2. Outbound WebRTC leg as offerer.
    let webrtc_pc = build_outbound_webrtc_pc(
        cfg.ice_servers.as_ref(),
        &cfg.audio_codec,
        global_ice_servers,
    )?;
    webrtc_pc
        .wait_for_gathering_complete()
        .await;
    let offer = webrtc_pc
        .create_offer()
        .await
        .map_err(|e| anyhow!("create_offer failed on WebRTC leg: {e}"))?;
    let offer_sdp = offer.to_sdp_string();
    webrtc_pc
        .set_local_description(offer)
        .map_err(|e| anyhow!("set_local_description failed on WebRTC leg: {e}"))?;

    // 3. Drive signaling.
    let ctx = SignalingContext {
        endpoint_url: cfg.endpoint_url.clone(),
        auth_header: cfg.auth_header.clone(),
        timeout_ms: 5_000,
        protocol: cfg.protocol.clone(),
    };
    let NegotiateOutcome {
        answer_sdp,
        session,
    } = adapter
        .negotiate(&ctx, &offer_sdp)
        .await
        .map_err(|e| anyhow!("signaling negotiate failed: {e}"))?;

    // 4. Apply the negotiated answer to the WebRTC leg.
    let answer_desc = SessionDescription::parse(SdpType::Answer, &answer_sdp)
        .map_err(|e| anyhow!("failed to parse signaling answer SDP: {e:?}"))?;
    webrtc_pc
        .set_remote_description(answer_desc)
        .await
        .map_err(|e| anyhow!("set_remote_description failed on WebRTC leg: {e}"))?;

    // 5. Wire the two PCs into a bridge with Opus(WebRTC)↔PCMU(SIP)
    // transcoding caps. Codec choice on the SIP side mirrors the standard
    // PSTN baseline — Phase 7 may re-derive this from the negotiated SDP.
    let webrtc_caps = codec_params_from_capability(&audio_capability_for(&cfg.audio_codec)?);
    let rtp_caps = codec_params_from_capability(&AudioCapability::pcmu());

    let bridge = Arc::new(BridgePeer::new(trunk.name.clone(), webrtc_pc, rtp_pc));
    bridge
        .setup_bridge_with_codecs(webrtc_caps, rtp_caps)
        .await
        .map_err(|e| anyhow!("bridge setup_bridge_with_codecs failed: {e}"))?;

    Ok(DispatchOutcome {
        sip_sdp_answer,
        bridge,
        adapter,
        session,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::trunk::{TrunkDirection, TrunkStatus};
    use serde_json::json;

    fn webrtc_trunk(signaling_name: &str) -> trunk::Model {
        trunk::Model {
            id: 1,
            name: "test_webrtc_trunk".into(),
            kind: "webrtc".into(),
            status: TrunkStatus::Healthy,
            direction: TrunkDirection::Outbound,
            is_active: true,
            kind_config: json!({
                "signaling": signaling_name,
                "endpoint_url": "http://127.0.0.1:1/offer",
                "audio_codec": "opus",
                "protocol": {
                    "request_body_template": r#"{"sdp":"{offer_sdp}"}"#,
                    "response_answer_path": "$.sdp",
                }
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn build_outbound_webrtc_pc_creates_pc_with_audio_transceiver() {
        let pc = build_outbound_webrtc_pc(None, "opus", None).unwrap();
        let transceivers = pc.get_transceivers();
        let audio_count = transceivers
            .iter()
            .filter(|t| matches!(t.kind(), MediaKind::Audio))
            .count();
        assert_eq!(audio_count, 1, "expected exactly one audio transceiver");
    }

    #[tokio::test]
    async fn build_outbound_webrtc_pc_rejects_unknown_codec() {
        match build_outbound_webrtc_pc(None, "carrier-pigeon", None) {
            Ok(_) => panic!("expected unknown-codec error"),
            Err(e) => assert!(e.to_string().contains("carrier-pigeon")),
        }
    }

    #[test]
    fn resolve_ice_servers_prefers_per_trunk() {
        let per_trunk = json!([{"urls": ["stun:per-trunk:3478"]}]);
        let global = vec![IceServer::new(vec!["stun:global:3478".to_string()])];
        let out = resolve_ice_servers(Some(&per_trunk), Some(&global)).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].urls, vec!["stun:per-trunk:3478".to_string()]);
    }

    #[test]
    fn resolve_ice_servers_falls_back_to_global() {
        let global = vec![IceServer::new(vec!["stun:global:3478".to_string()])];
        let out = resolve_ice_servers(None, Some(&global)).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].urls, vec!["stun:global:3478".to_string()]);
    }

    #[test]
    fn resolve_ice_servers_empty_when_neither_set() {
        let out = resolve_ice_servers(None, None).unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn dispatch_rejects_unknown_adapter() {
        // Make sure the *known* adapters are registered, but use a name
        // guaranteed not to exist.
        signaling::register_builtins();
        let trunk = webrtc_trunk("frobnicate");
        let result = dispatch_webrtc(&trunk, "v=0\r\n", None).await;
        let err = match result {
            Ok(_) => panic!("expected unknown-adapter error"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("frobnicate"),
            "expected error to mention adapter name, got: {msg}"
        );
        assert!(
            msg.contains("not registered"),
            "expected `not registered` in error, got: {msg}"
        );
    }
}
