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

use tracing::warn;

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
    /// Signaling context used during negotiation. Returned so the caller
    /// can drive `adapter.close()` on failure paths that occur after this
    /// function returned Ok (e.g. SIP `reply_with` failure) without
    /// re-deriving endpoint_url / auth_header from the DB.
    pub ctx: SignalingContext,
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
    // Always advertise `telephone-event` (RFC 2833 DTMF) on the WebRTC side
    // alongside the audio codec. Without this, DTMF events arriving from
    // the SIP carrier have nowhere to go on the bot side. The bridge data
    // plane forwards them as raw RTP once `set_dtmf_sink` is installed in
    // `dispatch_webrtc` after negotiation completes.
    let cfg = RtcConfiguration {
        transport_mode: TransportMode::WebRtc,
        ice_servers,
        media_capabilities: Some(MediaCapabilities {
            audio: vec![audio_cap, AudioCapability::telephone_event()],
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

/// SIP-side audio capabilities offered for negotiation. Ordered by
/// quality-preference: the carrier's offer is matched against this list in
/// the order it appears in the carrier's m=audio line, so this ordering
/// only matters when we're the offerer (we're the answerer in the inbound
/// flow, so the carrier's preference wins). Each entry is one we can
/// actually transcode to/from the WebRTC side via `BridgePeer`.
///
/// G.729 is intentionally omitted — it's patent-encumbered and
/// rustrtc's default build doesn't carry a real encoder/decoder.
///
/// `telephone-event` (RFC 2833 / RFC 4733 DTMF) is offered so the carrier
/// can negotiate out-of-band DTMF. The bridge's data plane already
/// detects DTMF packets and passes them through without transcoding
/// (`BridgePeer::forward_track_to_sender` checks `dtmf_sink`), provided
/// we call `set_dtmf_sink` on both endpoints after negotiation —
/// `dispatch_webrtc` does that below.
fn sip_side_audio_offer() -> Vec<AudioCapability> {
    vec![
        AudioCapability::opus(),
        AudioCapability::g722(),
        AudioCapability::pcmu(),
        AudioCapability::pcma(),
        AudioCapability::telephone_event(),
    ]
}

/// Build the inbound SIP-side RTP PeerConnection from the INVITE's offer
/// SDP, producing an SDP answer suitable for the 200 OK.
///
/// Returns the configured PC, the answer SDP string, the
/// [`AudioCapability`] that the SIP-side negotiation actually settled on
/// (the audio voice codec — drives the transcoder), and the
/// telephone-event payload type negotiated for RFC 2833 DTMF
/// pass-through (`None` if the carrier didn't offer telephone-event).
async fn build_inbound_rtp_pc(
    invite_offer_sdp: &str,
) -> Result<(PeerConnection, String, AudioCapability, Option<u8>)> {
    let cfg = RtcConfiguration {
        transport_mode: TransportMode::Rtp,
        media_capabilities: Some(MediaCapabilities {
            audio: sip_side_audio_offer(),
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
    let local_desc = pc
        .local_description()
        .ok_or_else(|| anyhow!("RTP leg has no local description after set_local_description"))?;
    let answer_sdp = local_desc.to_sdp_string();

    // Compute the negotiated codec by intersecting the carrier's offer
    // (remote_description) with our supported set. In RFC 3264 terms the
    // answerer picks the first codec in the offer that it also supports;
    // we mirror that here against `sip_side_audio_offer()`.
    //
    // We can't rely on `local_desc.first_audio_section().to_audio_capabilities()`
    // because rustrtc's Rtp-mode "answer" enumerates *our* full offer set
    // (not the single chosen codec), which would make every call look
    // like Opus regardless of what the carrier actually wanted.
    let offer_parsed = SessionDescription::parse(SdpType::Offer, invite_offer_sdp)
        .map_err(|e| anyhow!("failed to re-parse INVITE offer for codec negotiation: {e:?}"))?;
    let supported = sip_side_audio_offer();
    let offered: Vec<AudioCapability> = offer_parsed
        .audio_sections()
        .flat_map(|sec| sec.to_audio_capabilities())
        .collect();
    let is_dtmf = |c: &AudioCapability| c.codec_name.eq_ignore_ascii_case("telephone-event");
    // Voice codec: first offered audio codec that's in our supported set,
    // excluding telephone-event (which is signaling, not voice).
    let negotiated = offered
        .iter()
        .find(|c| {
            !is_dtmf(c)
                && supported
                    .iter()
                    .any(|s| s.codec_name.eq_ignore_ascii_case(&c.codec_name))
        })
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "carrier offered no voice codecs the bridge supports — supported: {:?}, \
                 offered: {:?}",
                supported
                    .iter()
                    .filter(|c| !is_dtmf(c))
                    .map(|c| c.codec_name.as_str())
                    .collect::<Vec<_>>(),
                offered.iter().map(|c| c.codec_name.clone()).collect::<Vec<_>>()
            )
        })?;
    // Telephone-event (RFC 2833 DTMF) PT: the carrier picks the dynamic PT
    // for it (often 101 but can be anything ≥ 96). `None` when not offered.
    let dtmf_pt = offered.iter().find(|c| is_dtmf(c)).map(|c| c.payload_type);
    Ok((pc, answer_sdp, negotiated, dtmf_pt))
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
    let cfg = trunk.webrtc().map_err(|e| {
        crate::metrics::bridge::dispatch_outcome("rtp_setup_error");
        e
    })?;
    let adapter_name = cfg.signaling.clone();
    let adapter = signaling::lookup(&cfg.signaling).ok_or_else(|| {
        // Treat an unknown adapter the same as any other RTP/setup-time
        // failure for outcome accounting — we never made it to signaling.
        crate::metrics::bridge::dispatch_outcome("rtp_setup_error");
        anyhow!(
            "signaling adapter '{}' not registered for trunk '{}'",
            cfg.signaling,
            trunk.name
        )
    })?;

    // Helper: classify any setup-time (non-signaling) error.
    let setup_err = |e: anyhow::Error| -> anyhow::Error {
        crate::metrics::bridge::dispatch_outcome("rtp_setup_error");
        e
    };

    // 1. Inbound RTP leg + SDP answer for the SIP 200 OK + the codec the
    // SIP negotiation settled on + the carrier's DTMF (RFC 2833) payload
    // type if any. We use that voice codec to drive the bridge's
    // transcoder, and install a `dtmf_sink` keyed on the RFC 2833 PT so
    // the bridge data plane forwards DTMF events through verbatim
    // instead of feeding them to the audio transcoder.
    let (rtp_pc, sip_sdp_answer, negotiated_sip_cap, sip_dtmf_pt) =
        build_inbound_rtp_pc(invite_offer_sdp)
            .await
            .map_err(setup_err)?;

    // 2. Outbound WebRTC leg as offerer.
    //
    // Order matters: ICE gathering does not begin until `set_local_description`
    // is called (per the WebRTC spec). Awaiting `wait_for_gathering_complete`
    // *before* that would either return immediately (gathering state == new)
    // or — worse — produce an offer SDP with no `a=candidate:` lines, which
    // breaks any peer that doesn't tolerate trickle ICE. Pull the SDP from
    // `local_description()` *after* gathering so the candidates are folded in.
    let webrtc_pc = build_outbound_webrtc_pc(
        cfg.ice_servers.as_ref(),
        &cfg.audio_codec,
        global_ice_servers,
    )
    .map_err(setup_err)?;
    let offer = webrtc_pc
        .create_offer()
        .await
        .map_err(|e| setup_err(anyhow!("create_offer failed on WebRTC leg: {e}")))?;
    webrtc_pc
        .set_local_description(offer)
        .map_err(|e| setup_err(anyhow!("set_local_description failed on WebRTC leg: {e}")))?;
    webrtc_pc.wait_for_gathering_complete().await;
    let offer_sdp = webrtc_pc
        .local_description()
        .ok_or_else(|| {
            setup_err(anyhow!(
                "WebRTC leg has no local description after gathering"
            ))
        })?
        .to_sdp_string();

    // 3. Drive signaling. Time the adapter call so on-call can see signaling
    // latency per adapter as a histogram.
    let ctx = SignalingContext {
        endpoint_url: cfg.endpoint_url.clone(),
        auth_header: cfg.auth_header.clone(),
        timeout_ms: cfg.signaling_timeout_ms.unwrap_or(5_000),
        protocol: cfg.protocol.clone(),
    };
    let signaling_start = std::time::Instant::now();
    let negotiate_res = adapter.negotiate(&ctx, &offer_sdp).await;
    crate::metrics::bridge::signaling_latency_seconds(
        &adapter_name,
        signaling_start.elapsed().as_secs_f64(),
    );
    let NegotiateOutcome {
        answer_sdp,
        session,
    } = negotiate_res.map_err(|e| {
        crate::metrics::bridge::dispatch_outcome("signaling_error");
        anyhow!("signaling negotiate failed: {e}")
    })?;

    // After this point the remote bot has committed a session. Any failure
    // must close it remotely before returning Err, or it leaks until the
    // bot's own idle timeout (often minutes) — and a flood of dispatch
    // failures will exhaust the bot's session capacity. Wrap subsequent
    // steps so the close-on-error is uniform.
    let close_on_setup_failure =
        |e: anyhow::Error,
         adapter: Arc<dyn WebRtcSignalingAdapter>,
         ctx: SignalingContext,
         session: SessionHandle| async move {
            if let Err(close_err) = adapter.close(&ctx, &session).await {
                warn!(
                    error = %close_err,
                    adapter = %ctx.endpoint_url,
                    "post-negotiate setup failed and adapter.close also failed; \
                     bot session may leak until idle timeout"
                );
            }
            setup_err(e)
        };

    // 4. Apply the negotiated answer to the WebRTC leg.
    let answer_desc = match SessionDescription::parse(SdpType::Answer, &answer_sdp) {
        Ok(desc) => desc,
        Err(e) => {
            return Err(close_on_setup_failure(
                anyhow!("failed to parse signaling answer SDP: {e:?}"),
                adapter.clone(),
                ctx,
                session,
            )
            .await);
        }
    };
    // Extract the WebRTC-side telephone-event PT from the bot's answer
    // *before* we hand the SDP off to `set_remote_description` (which
    // consumes the value). This is the PT the bot will use when sending
    // DTMF from the WebRTC side towards us, so the symmetric `dtmf_sink`
    // on `BridgeEndpoint::WebRtc` keys on it.
    let webrtc_dtmf_pt: Option<u8> = answer_desc
        .audio_sections()
        .flat_map(|sec| sec.to_audio_capabilities())
        .find(|c| c.codec_name.eq_ignore_ascii_case("telephone-event"))
        .map(|c| c.payload_type);
    if let Err(e) = webrtc_pc.set_remote_description(answer_desc).await {
        return Err(close_on_setup_failure(
            anyhow!("set_remote_description failed on WebRTC leg: {e}"),
            adapter.clone(),
            ctx,
            session,
        )
        .await);
    }

    // 5. Wire the two PCs into a bridge. Codec on each side now follows
    // what was actually negotiated:
    //   * WebRTC side: from the trunk's configured `audio_codec`
    //     (operator-controlled; defaults to opus).
    //   * SIP side: from whatever the SIP negotiation in step 1 settled
    //     on (was hardcoded to PCMU previously, which silenced audio
    //     when the carrier accepted PCMA / G.722 / Opus-on-SIP).
    let audio_cap = match audio_capability_for(&cfg.audio_codec) {
        Ok(cap) => cap,
        Err(e) => {
            return Err(close_on_setup_failure(e, adapter.clone(), ctx, session).await);
        }
    };
    let webrtc_caps = codec_params_from_capability(&audio_cap);
    let rtp_caps = codec_params_from_capability(&negotiated_sip_cap);
    tracing::info!(
        trunk = %trunk.name,
        sip_codec = %negotiated_sip_cap.codec_name,
        sip_pt = negotiated_sip_cap.payload_type,
        webrtc_codec = %cfg.audio_codec,
        "webrtc bridge negotiated codecs"
    );

    let bridge = Arc::new(BridgePeer::new(trunk.name.clone(), webrtc_pc, rtp_pc));
    if let Err(e) = bridge.setup_bridge_with_codecs(webrtc_caps, rtp_caps).await {
        return Err(close_on_setup_failure(
            anyhow!("bridge setup_bridge_with_codecs failed: {e}"),
            adapter.clone(),
            ctx,
            session,
        )
        .await);
    }

    // 6. DTMF pass-through (RFC 2833) — install per-direction sinks. Each
    // sink keys on the PT *the sender* uses on that leg:
    //   * SIP → WebRTC: `sip_dtmf_pt` is the PT the carrier advertised in
    //     its INVITE offer (i.e. the PT the carrier will *send* DTMF as).
    //   * WebRTC → SIP: `webrtc_dtmf_pt` is the PT the bot accepted in its
    //     answer (i.e. the PT the bot will *send* DTMF as).
    //
    // The bridge data plane consults the sink slot for the source endpoint
    // on every incoming sample; matching packets bypass the audio
    // transcoder (which would corrupt the telephone-event payload) and
    // are forwarded verbatim. The sink handler logs only; actual delivery
    // is the existing forwarding path.
    if let Some(pt) = sip_dtmf_pt {
        let trunk_name_for_log = trunk.name.clone();
        bridge.set_dtmf_sink(
            crate::media::bridge::BridgeEndpoint::Rtp,
            pt,
            Arc::new(move |digit: char| {
                tracing::debug!(
                    trunk = %trunk_name_for_log,
                    digit = %digit,
                    "DTMF event received from SIP carrier"
                );
            }),
        );
        tracing::info!(
            trunk = %trunk.name,
            dtmf_pt = pt,
            "DTMF pass-through enabled (RFC 2833) for SIP → WebRTC direction"
        );
    } else {
        tracing::debug!(
            trunk = %trunk.name,
            "carrier did not offer telephone-event; SIP → WebRTC DTMF pass-through \
             disabled (DTMF tones would be conveyed as in-band audio only)"
        );
    }
    if let Some(pt) = webrtc_dtmf_pt {
        let trunk_name_for_log = trunk.name.clone();
        bridge.set_dtmf_sink(
            crate::media::bridge::BridgeEndpoint::WebRtc,
            pt,
            Arc::new(move |digit: char| {
                tracing::debug!(
                    trunk = %trunk_name_for_log,
                    digit = %digit,
                    "DTMF event received from WebRTC bot"
                );
            }),
        );
        tracing::info!(
            trunk = %trunk.name,
            dtmf_pt = pt,
            "DTMF pass-through enabled (RFC 2833) for WebRTC → SIP direction"
        );
    } else {
        tracing::debug!(
            trunk = %trunk.name,
            "bot answer omitted telephone-event; WebRTC → SIP DTMF pass-through disabled"
        );
    }

    // Note: "success" is counted by the caller (proxy::call) once reply_with
    // also succeeds — that keeps the four `rustpbx_bridge_dispatch_total`
    // outcomes mutually exclusive per call.
    Ok(DispatchOutcome {
        sip_sdp_answer,
        bridge,
        adapter,
        session,
        ctx,
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

    /// Adapter that always succeeds on `negotiate` (returning an
    /// intentionally invalid answer SDP so the post-negotiate parse step
    /// fails) and records every `close` call. Used to prove the
    /// session-leak fix: when a post-negotiate step errors, `close` must
    /// fire before the error propagates.
    struct LeakProbeAdapter {
        closed_count: std::sync::Arc<std::sync::Mutex<usize>>,
        last_closed_session: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
    }

    #[async_trait::async_trait]
    impl WebRtcSignalingAdapter for LeakProbeAdapter {
        async fn negotiate(
            &self,
            _ctx: &SignalingContext,
            _offer_sdp: &str,
        ) -> Result<NegotiateOutcome, crate::proxy::bridge::signaling::SignalingError> {
            Ok(NegotiateOutcome {
                // Intentionally garbage — parse will fail.
                answer_sdp: "not a valid SDP".to_string(),
                session: SessionHandle(json!({"id": "leak-probe-sess-1"})),
            })
        }
        async fn close(
            &self,
            _ctx: &SignalingContext,
            session: &SessionHandle,
        ) -> Result<(), crate::proxy::bridge::signaling::SignalingError> {
            *self.closed_count.lock().unwrap() += 1;
            *self.last_closed_session.lock().unwrap() = Some(session.0.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn dispatch_closes_session_when_post_negotiate_step_fails() {
        // Register the leak probe under a unique adapter name and point a
        // trunk at it.
        let closed_count = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let last_session = std::sync::Arc::new(std::sync::Mutex::new(None));
        let probe = std::sync::Arc::new(LeakProbeAdapter {
            closed_count: closed_count.clone(),
            last_closed_session: last_session.clone(),
        });
        signaling::register("leak_probe", probe);

        let trunk = webrtc_trunk("leak_probe");
        // Use a minimal valid SDP offer so steps 1-3 succeed; step 4
        // (SDP parse of the bogus answer) is what we want to trigger.
        let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                     t=0 0\r\nm=audio 10000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let result = dispatch_webrtc(&trunk, offer, None).await;

        assert!(result.is_err(), "expected post-negotiate failure");
        let n = *closed_count.lock().unwrap();
        assert_eq!(
            n, 1,
            "adapter.close should fire exactly once on post-negotiate failure, got {n}"
        );
        let session = last_session.lock().unwrap().clone();
        assert_eq!(
            session,
            Some(json!({"id": "leak-probe-sess-1"})),
            "close should be invoked with the session handle returned by negotiate"
        );
    }

    /// Carrier offers only PCMA — the bridge's SIP-side answer must
    /// negotiate PCMA (not silently fall back to PCMU as the old hardcoded
    /// `AudioCapability::pcmu()` would). Proves codec passthrough works
    /// for the simplest case.
    #[tokio::test]
    async fn inbound_rtp_pc_negotiates_pcma_when_carrier_only_offers_pcma() {
        let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                     t=0 0\r\nm=audio 10000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
        let (_pc, answer_sdp, negotiated, _dtmf_pt) = build_inbound_rtp_pc(offer)
            .await
            .expect("PCMA-only offer should negotiate");
        assert_eq!(
            negotiated.codec_name, "PCMA",
            "negotiated codec should be PCMA when that's all the carrier offered"
        );
        assert_eq!(negotiated.payload_type, 8);
        assert!(
            answer_sdp.to_lowercase().contains("pcma"),
            "answer SDP must advertise PCMA, got:\n{}",
            answer_sdp
        );
    }

    /// Carrier offers Opus-on-SIP — the bridge must accept that too,
    /// not force PCMU/PCMA. Proves we genuinely picked from the carrier's
    /// list rather than imposing our own.
    #[tokio::test]
    async fn inbound_rtp_pc_negotiates_opus_when_carrier_offers_it() {
        let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                     t=0 0\r\nm=audio 10000 RTP/AVP 111\r\n\
                     a=rtpmap:111 opus/48000/2\r\n";
        let (_pc, _answer_sdp, negotiated, _dtmf_pt) = build_inbound_rtp_pc(offer)
            .await
            .expect("opus-on-SIP offer should negotiate");
        assert_eq!(
            negotiated.codec_name.to_lowercase(),
            "opus",
            "negotiated codec should be opus when the carrier offered it"
        );
    }

    /// Carrier offers PCMU + PCMA + G722 → answerer picks the carrier's
    /// preferred (first listed) codec. With the new intersection logic
    /// we walk the carrier's offer in order; PCMU is listed first so it
    /// wins.
    #[tokio::test]
    async fn inbound_rtp_pc_honours_carrier_codec_preference() {
        let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                     t=0 0\r\nm=audio 10000 RTP/AVP 0 8 9\r\n\
                     a=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\n\
                     a=rtpmap:9 G722/8000\r\n";
        let (_pc, _answer_sdp, negotiated, _dtmf_pt) = build_inbound_rtp_pc(offer)
            .await
            .expect("multi-codec offer should negotiate");
        assert_eq!(
            negotiated.codec_name.to_uppercase(),
            "PCMU",
            "negotiated codec should be PCMU (carrier's first preference), got: {}",
            negotiated.codec_name
        );
    }

    /// Carrier offers PCMU + telephone-event PT 101 → voice codec lands
    /// on PCMU (NOT telephone-event), and the DTMF payload type is
    /// captured separately for `set_dtmf_sink`.
    #[tokio::test]
    async fn inbound_rtp_pc_separates_voice_codec_from_dtmf_pt() {
        let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                     t=0 0\r\nm=audio 10000 RTP/AVP 0 101\r\n\
                     a=rtpmap:0 PCMU/8000\r\n\
                     a=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-16\r\n";
        let (_pc, _answer_sdp, voice, dtmf_pt) = build_inbound_rtp_pc(offer)
            .await
            .expect("PCMU + telephone-event offer should negotiate");
        assert_eq!(voice.codec_name.to_uppercase(), "PCMU",
            "voice codec must be PCMU, not telephone-event");
        assert_eq!(dtmf_pt, Some(101),
            "DTMF payload type must be captured separately for set_dtmf_sink");
    }

    /// Carrier offers a non-standard telephone-event PT (96 instead of
    /// the usual 101) → we honour whatever PT the carrier chose.
    #[tokio::test]
    async fn inbound_rtp_pc_honours_carrier_dtmf_pt() {
        let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                     t=0 0\r\nm=audio 10000 RTP/AVP 0 96\r\n\
                     a=rtpmap:0 PCMU/8000\r\n\
                     a=rtpmap:96 telephone-event/8000\r\na=fmtp:96 0-16\r\n";
        let (_pc, _answer_sdp, _voice, dtmf_pt) = build_inbound_rtp_pc(offer)
            .await
            .expect("non-standard DTMF PT should still negotiate");
        assert_eq!(dtmf_pt, Some(96),
            "DTMF PT must follow carrier's offer, not be hardcoded to 101");
    }

    /// Carrier does NOT offer telephone-event → DTMF PT is None.
    /// dispatch path will skip the `set_dtmf_sink` call and log that
    /// DTMF will only flow as in-band audio (if at all).
    #[tokio::test]
    async fn inbound_rtp_pc_no_dtmf_when_carrier_omits_it() {
        let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                     t=0 0\r\nm=audio 10000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let (_pc, _answer_sdp, _voice, dtmf_pt) = build_inbound_rtp_pc(offer)
            .await
            .expect("PCMU-only offer (no telephone-event) should still negotiate");
        assert_eq!(dtmf_pt, None,
            "no telephone-event in offer must yield dtmf_pt=None");
    }

    /// Carrier offers a codec we don't support (G.729) → dispatch should
    /// surface a clear error rather than silently fall back. Important
    /// because the previous hardcoded-PCMU path would have continued
    /// regardless and produced silent audio.
    #[tokio::test]
    async fn inbound_rtp_pc_rejects_unsupported_only_offer() {
        let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                     t=0 0\r\nm=audio 10000 RTP/AVP 18\r\na=rtpmap:18 G729/8000\r\n";
        let result = build_inbound_rtp_pc(offer).await;
        match result {
            Ok((_, _, neg, _)) => panic!(
                "expected error for G729-only offer, got negotiated={}",
                neg.codec_name
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("no voice codecs the bridge supports") || msg.to_lowercase().contains("g729"),
                    "expected codec-mismatch error message, got: {msg}"
                );
            }
        }
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
