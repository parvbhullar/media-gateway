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
    // Use the SDK's default ICE-transport policy (`All`) so libwebrtc can
    // gather host + srflx + relay candidates and pick whichever path
    // actually works against LiveKit's media edge.
    //
    // History: we briefly forced `IceTransportsType::Relay` here, on the
    // theory that this host's UDP egress was blocked. Diagnostic logs
    // showed that was wrong — direct (srflx) UDP did work intermittently
    // on the same network; forcing Relay just stripped the working
    // fallbacks and made the failure mode worse. On networks where TURN
    // really is the only option, the right knob is a per-trunk
    // `force_relay` config flag (deferred — add when an operator deploys
    // somewhere it actually matters). See livekit/sip's `WithDisableTURN`
    // for the inverse case (co-located deployments that skip TURN
    // entirely).
    let (room, events) = Room::connect(server_url, jwt, RoomOptions::default())
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
