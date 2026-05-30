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

use crate::media::bridge::{BridgeEndpoint, BridgePeer};
use crate::media::negotiate::MediaNegotiator;

/// Install (or clear) the bridge's audio transcoders based on the codecs
/// each leg actually negotiated, read from the two SDP answers:
///
/// * `sip_answer_sdp`  — rustpbx's SIP 200-OK answer to the carrier (the
///   `BridgeEndpoint::Rtp` leg).
/// * `webrtc_answer_sdp` — the bot's WebRTC answer (the `BridgeEndpoint::WebRtc`
///   leg).
///
/// When the codecs differ, a transcoder is set in both directions; when they
/// match, both transcoders are cleared so audio passes through untouched.
pub(crate) fn configure_webrtc_bridge_transcoders(
    bridge: &BridgePeer,
    sip_answer_sdp: &str,
    webrtc_answer_sdp: &str,
) {
    let sip = MediaNegotiator::extract_leg_profile(sip_answer_sdp);
    let web = MediaNegotiator::extract_leg_profile(webrtc_answer_sdp);

    let (Some(sip_audio), Some(web_audio)) = (sip.audio, web.audio) else {
        // Couldn't determine one leg's codec from its SDP — leave the
        // transcoders untouched (passthrough). Safer than guessing.
        tracing::debug!(
            "webrtc bridge: could not extract audio codec from one leg's SDP; \
             leaving transcoders as-is"
        );
        return;
    };

    if sip_audio.codec == web_audio.codec {
        // Same codec on both legs → no transcode needed; clear any stale
        // transcoders so audio passes through byte-for-byte.
        bridge.clear_transcoder(BridgeEndpoint::Rtp);
        bridge.clear_transcoder(BridgeEndpoint::WebRtc);
        tracing::debug!(
            codec = ?sip_audio.codec,
            "webrtc bridge transcoder not needed; SIP and WebRTC legs share a codec"
        );
        return;
    }

    // Mismatch → transcode in both directions. `set_transcoder`'s endpoint
    // names the SOURCE leg; `target_pt` is the PT the destination leg
    // negotiated (so the re-stamped RTP matches what that peer expects).
    bridge.set_transcoder(
        BridgeEndpoint::Rtp, // caller → bot
        sip_audio.codec,
        web_audio.codec,
        web_audio.payload_type,
    );
    bridge.set_transcoder(
        BridgeEndpoint::WebRtc, // bot → caller
        web_audio.codec,
        sip_audio.codec,
        sip_audio.payload_type,
    );
    tracing::info!(
        sip_codec = ?sip_audio.codec,
        sip_pt = sip_audio.payload_type,
        webrtc_codec = ?web_audio.codec,
        webrtc_pt = web_audio.payload_type,
        "webrtc bridge transcoder configured for SIP↔WebRTC codec mismatch"
    );
}
