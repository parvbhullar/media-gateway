//! Thin wrapper over livekit::Room — connect + publish only. The rest of
//! the lifecycle (events, subscribe, disconnect) belongs in `media.rs`
//! and `teardown.rs`.

use anyhow::{Result, anyhow};
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::prelude::{AudioSourceOptions, RtcAudioSource};

/// What we get back after a successful Room::connect + publish.
pub struct ConnectedRoom {
    pub room: Room,
    /// Event receiver — `LiveKitBridge::start` drains this in task C
    /// (room events).
    pub events: tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    /// Audio source we feed PCM frames into (the published "sip-caller"
    /// track is built around this source).
    pub local_source: NativeAudioSource,
}

/// Connect to LiveKit at `server_url` with `jwt`, create+publish a
/// "sip-caller" LocalAudioTrack backed by a fresh NativeAudioSource @
/// 48 kHz mono.
pub async fn connect_and_publish(server_url: &str, jwt: &str) -> Result<ConnectedRoom> {
    // Force ICE transport policy to Relay for the publisher/subscriber PCs.
    //
    // Rationale (validated against Oracle-VM ↔ LiveKit Cloud diagnostics):
    // the publisher PeerConnection's *direct* connectivity checks to
    // LiveKit's `ice-lite` SFU host candidate (`143.223.91.x`) intermittently
    // — and eventually persistently — receive no STUN responses (the flaky
    // public-internet UDP leg between this VM and the SFU). Meanwhile TURN
    // allocations against LiveKit's TURN servers *always* succeed
    // (rustpbx → LiveKit-TURN is a reliable path). Forcing Relay routes the
    // media/connectivity through that TURN path: the only internet hop is
    // rustpbx → TURN (proven good), and the TURN → SFU hop is internal to
    // LiveKit's datacenter. This converts the unreliable direct-UDP-to-SFU
    // leg into the reliable relayed leg.
    //
    // Trade-off: ~30-50ms extra one-way latency and LiveKit TURN bandwidth
    // usage. Acceptable for a NAT'd cloud bridge that owns the SIP↔LiveKit
    // path. (Self-hosted LiveKit co-located with rustpbx wouldn't need this
    // — a future per-trunk `force_relay` flag could make it conditional.)
    let mut room_options = RoomOptions::default();
    room_options.rtc_config.ice_transport_type =
        livekit::webrtc::prelude::IceTransportsType::Relay;
    let (room, events) = Room::connect(server_url, jwt, room_options)
        .await
        .map_err(|e| anyhow!("livekit Room::connect failed: {e}"))?;

    // Construct the audio source the forward-task-A loop will feed.
    let local_source = NativeAudioSource::new(
        AudioSourceOptions {
            echo_cancellation: false,
            noise_suppression: false,
            auto_gain_control: false,
        },
        48_000,
        1,
        1_000,
    );

    let track = LocalAudioTrack::create_audio_track(
        "sip-caller",
        RtcAudioSource::Native(local_source.clone()),
    );

    // If publish_track fails AFTER Room::connect succeeded, we own a
    // live LiveKit room with rustpbx as a "ghost" participant. Dropping
    // `room` won't send a Disconnect message on its own — call close()
    // explicitly so the LiveKit server sheds the participant immediately
    // rather than waiting on its idle timeout.
    if let Err(e) = room
        .local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..Default::default()
            },
        )
        .await
    {
        let publish_err = anyhow!("livekit publish_track failed: {e}");
        if let Err(close_err) = room.close().await {
            tracing::warn!(
                error = %close_err,
                "post-publish-failure room.close also failed; \
                 LiveKit room may linger until its idle timeout"
            );
        }
        return Err(publish_err);
    }

    Ok(ConnectedRoom {
        room,
        events,
        local_source,
    })
}
