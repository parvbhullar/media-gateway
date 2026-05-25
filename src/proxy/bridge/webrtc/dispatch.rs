//! `dispatch_webrtc` — the top-level entry point for the webrtc kind.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use rustrtc::{IceServer, SdpType, sdp::SessionDescription};

use tracing::warn;

use crate::media::bridge::BridgePeer;
use crate::models::trunk;

use crate::proxy::bridge::common::{
    DispatchContext, audio_capability_for, build_inbound_rtp_pc, codec_params_from_capability,
};
use crate::proxy::bridge::signaling::{
    self, NegotiateOutcome, SessionHandle, SignalingContext, WebRtcSignalingAdapter,
};

use crate::proxy::bridge::session::{BridgeKind, DispatchOutcome};
use super::dtmf;
use super::sdp::build_outbound_webrtc_pc;
use super::media_bridge::WebRtcMediaBridge;
use super::teardown::WebRtcTeardown;

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
    _ctx: &DispatchContext,
) -> Result<DispatchOutcome> {
    let cfg = trunk.webrtc().map_err(|e| {
        crate::metrics::bridge::dispatch_outcome(BridgeKind::WebRtc, "rtp_setup_error");
        e
    })?;
    let adapter_name = cfg.signaling.clone();
    let adapter = signaling::lookup(&cfg.signaling).ok_or_else(|| {
        // Treat an unknown adapter the same as any other RTP/setup-time
        // failure for outcome accounting — we never made it to signaling.
        crate::metrics::bridge::dispatch_outcome(BridgeKind::WebRtc, "rtp_setup_error");
        anyhow!(
            "signaling adapter '{}' not registered for trunk '{}'",
            cfg.signaling,
            trunk.name
        )
    })?;

    // Helper: classify any setup-time (non-signaling) error.
    let setup_err = |e: anyhow::Error| -> anyhow::Error {
        crate::metrics::bridge::dispatch_outcome(BridgeKind::WebRtc, "rtp_setup_error");
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
        BridgeKind::WebRtc,
        &adapter_name,
        signaling_start.elapsed().as_secs_f64(),
    );
    let NegotiateOutcome {
        answer_sdp,
        session,
    } = negotiate_res.map_err(|e| {
        crate::metrics::bridge::dispatch_outcome(BridgeKind::WebRtc, "signaling_error");
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
    let webrtc_dtmf_pt = dtmf::webrtc_dtmf_pt_from_answer(&answer_desc);
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
    dtmf::install_sip_to_webrtc_dtmf(&bridge, sip_dtmf_pt, &trunk.name);
    dtmf::install_webrtc_to_sip_dtmf(&bridge, webrtc_dtmf_pt, &trunk.name);

    // Note: "success" is counted by the caller (proxy::call) once reply_with
    // also succeeds — that keeps the four `rustpbx_bridge_dispatch_total`
    // outcomes mutually exclusive per call.
    Ok(DispatchOutcome {
        sip_sdp_answer,
        bridge: Arc::new(WebRtcMediaBridge(bridge)),
        teardown: Box::new(WebRtcTeardown {
            adapter,
            session,
            ctx,
        }),
    })
}
