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

    room.local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| anyhow!("livekit publish_track failed: {e}"))?;

    Ok(ConnectedRoom {
        room,
        events,
        local_source,
    })
}
