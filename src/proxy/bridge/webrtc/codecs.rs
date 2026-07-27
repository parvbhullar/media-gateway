//! Codec reconciliation for the `kind=webrtc` SIP↔WebRTC bridge.
//!
//! The SIP leg and the WebRTC leg can settle on different audio codecs (a
//! G.711 PSTN carrier on one side, Opus on the bot side). The bridge data
//! plane only transcodes when a [`Transcoder`] is installed; otherwise it
//! forwards raw payload bytes and (in `external_bridge_mode`) lets rustrtc
//! stamp the destination's default payload type — which silently mislabels
//! G.711 as Opus and leaves the peer deaf.
//!
//! This mirrors the legacy B2BUA's `sip_session::configure_media_bridge_transcoders`:
//! compare the two negotiated codecs and install a transcoder in each
//! direction when they differ (or clear both when they match, preserving
//! zero-cost passthrough).
//!
//! The SIP codec is taken from the **authoritative** negotiated capability
//! that `build_inbound_rtp_pc` already computed — NOT by re-parsing the SIP
//! answer SDP. rustrtc's RTP-mode answer enumerates our full offered set
//! (Opus first), so the answer's first codec is *not* what the leg actually
//! negotiated (see the note in `proxy::bridge::common::build_inbound_rtp_pc`).
//! The WebRTC codec is still read from the bot's answer, which genuinely
//! selects a single codec and carries the payload type we must restamp to.

use audio_codec::CodecType;

use crate::media::bridge::{BridgeEndpoint, BridgePeer};
use crate::media::negotiate::MediaNegotiator;

/// Install (or clear) the bridge's audio transcoders based on the codec each
/// leg actually negotiated.
///
/// * `sip_codec` / `sip_pt` — the authoritative SIP-leg voice codec from
///   `build_inbound_rtp_pc`'s returned `AudioCapability` (the
///   `BridgeEndpoint::Rtp` leg).
/// * `webrtc_answer_sdp` — the bot's WebRTC answer (the `BridgeEndpoint::WebRtc`
///   leg); its selected codec + payload type are read from this SDP.
///
/// When the codecs differ, a transcoder is set in both directions; when they
/// match, both transcoders are cleared so audio passes through untouched.
pub(crate) fn configure_webrtc_bridge_transcoders(
    bridge: &BridgePeer,
    sip_codec: CodecType,
    sip_pt: u8,
    webrtc_answer_sdp: &str,
) {
    let Some(web_audio) = MediaNegotiator::extract_leg_profile(webrtc_answer_sdp).audio else {
        // Couldn't determine the WebRTC leg's codec from its answer — leave
        // the transcoders untouched (passthrough). Safer than guessing.
        tracing::debug!(
            "webrtc bridge: could not extract WebRTC audio codec from answer SDP; \
             leaving transcoders as-is"
        );
        return;
    };

    if sip_codec == web_audio.codec {
        // Same codec on both legs → no transcode needed; clear any stale
        // transcoders so audio passes through byte-for-byte.
        bridge.clear_transcoder(BridgeEndpoint::Rtp);
        bridge.clear_transcoder(BridgeEndpoint::WebRtc);
        tracing::debug!(
            codec = ?sip_codec,
            "webrtc bridge transcoder not needed; SIP and WebRTC legs share a codec"
        );
        return;
    }

    // Mismatch → transcode in both directions. `set_transcoder`'s endpoint
    // names the SOURCE leg; `target_pt` is the PT the destination leg
    // negotiated (so the re-stamped RTP matches what that peer expects).
    bridge.set_transcoder(
        BridgeEndpoint::Rtp, // caller → bot
        sip_codec,
        web_audio.codec,
        web_audio.payload_type,
    );
    bridge.set_transcoder(
        BridgeEndpoint::WebRtc, // bot → caller
        web_audio.codec,
        sip_codec,
        sip_pt,
    );
    tracing::info!(
        ?sip_codec,
        sip_pt,
        webrtc_codec = ?web_audio.codec,
        webrtc_pt = web_audio.payload_type,
        "webrtc bridge transcoder configured for SIP↔WebRTC codec mismatch"
    );
}
