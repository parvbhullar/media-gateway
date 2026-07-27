//! Outbound WebRTC PeerConnection construction (offerer role).

use anyhow::Result;
use rustrtc::{
    IceServer, MediaKind, PeerConnection, RtcConfiguration, TransceiverDirection, TransportMode,
    config::{AudioCapability, MediaCapabilities, SdpCompatibilityMode, VideoCapability},
};
use serde_json::Value;

use crate::proxy::bridge::common::{audio_capability_for, resolve_ice_servers};

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
