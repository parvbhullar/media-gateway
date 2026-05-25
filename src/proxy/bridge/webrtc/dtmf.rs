//! DTMF sink wiring for the SIP↔WebRTC bridge.

use std::sync::Arc;

use rustrtc::sdp::SessionDescription;

use crate::media::bridge::{BridgeEndpoint, BridgePeer};

/// Extract the WebRTC-side telephone-event payload type from a parsed
/// SDP answer (the bot's offer of telephone-event in its answer SDP).
/// Returns None if the answer did not include telephone-event.
pub(super) fn webrtc_dtmf_pt_from_answer(answer_desc: &SessionDescription) -> Option<u8> {
    answer_desc
        .audio_sections()
        .flat_map(|sec| sec.to_audio_capabilities())
        .find(|c| c.codec_name.eq_ignore_ascii_case("telephone-event"))
        .map(|c| c.payload_type)
}

/// Install the SIP→WebRTC direction DTMF sink: bridge data plane will
/// pass telephone-event packets verbatim from the SIP RTP leg to the
/// WebRTC peer instead of feeding them through the audio transcoder.
/// Handler is log-only.
pub(super) fn install_sip_to_webrtc_dtmf(
    bridge: &Arc<BridgePeer>,
    sip_dtmf_pt: Option<u8>,
    trunk_name: &str,
) {
    if let Some(pt) = sip_dtmf_pt {
        let trunk_name_for_log = trunk_name.to_string();
        bridge.set_dtmf_sink(
            BridgeEndpoint::Rtp,
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
            trunk = %trunk_name,
            dtmf_pt = pt,
            "DTMF pass-through enabled (RFC 2833) for SIP → WebRTC direction"
        );
    } else {
        tracing::debug!(
            trunk = %trunk_name,
            "carrier did not offer telephone-event; SIP → WebRTC DTMF pass-through \
             disabled (DTMF tones would be conveyed as in-band audio only)"
        );
    }
}

/// Symmetric helper for WebRTC→SIP direction. PT is taken from the bot's
/// answer SDP via `webrtc_dtmf_pt_from_answer`.
pub(super) fn install_webrtc_to_sip_dtmf(
    bridge: &Arc<BridgePeer>,
    webrtc_dtmf_pt: Option<u8>,
    trunk_name: &str,
) {
    if let Some(pt) = webrtc_dtmf_pt {
        let trunk_name_for_log = trunk_name.to_string();
        bridge.set_dtmf_sink(
            BridgeEndpoint::WebRtc,
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
            trunk = %trunk_name,
            dtmf_pt = pt,
            "DTMF pass-through enabled (RFC 2833) for WebRTC → SIP direction"
        );
    } else {
        tracing::debug!(
            trunk = %trunk_name,
            "bot answer omitted telephone-event; WebRTC → SIP DTMF pass-through disabled"
        );
    }
}
