//! LiveKitBridge — implements `MediaBridge` for the livekit kind.
//!
//! Forward tasks now wire actual audio between the SIP-side RTP
//! PeerConnection and the LiveKit Room:
//!
//!   * Task A — drains incoming RTP audio from `sip_pc`, decodes via the
//!     negotiated SIP codec, resamples to 48 kHz mono, accumulates into
//!     20 ms LiveKit frames, and pushes via `local_source.capture_frame`.
//!   * Task B — every 20 ms, pulls the next available frame from each
//!     subscribed remote audio stream, picks the loudest (or only) one,
//!     resamples to the SIP clock-rate, encodes back to the SIP codec, and
//!     sends via a sample-track attached to `sip_pc`. v1 forwards the
//!     first-non-empty subscriber's frame as-is instead of summing — see
//!     the "Pragmatic shortcuts" comment in task B.
//!   * Task C — drains room events; on `TrackSubscribed` it instantiates a
//!     `NativeAudioStream` and stores it in `subscribers`; on
//!     `ParticipantDisconnected` it removes the entry; on `Disconnected`
//!     it cancels the token (causing tasks A & B to exit).
//!
//! Errors in encode/decode/resample are logged at warn but do not crash —
//! RTP is lossy, dropping one frame is preferable to ending the call.

use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use crate::media::resampler::VoiceResampler;
use audio_codec::{CodecType, Decoder, Encoder};
use audio_codec::opus::OpusEncoder;

/// Construct the SIP-side audio decoder. We use the audio_codec crate's
/// default factory (stereo Opus, 48 kHz) because Linphone and most carrier
/// softphones encode Opus as stereo per `opus/48000/2` in their offer, and
/// a mono decoder produces SILENCE when fed those payloads. The stereo
/// decoder internally downmixes L+R to mono on output, so the rest of the
/// bridge (which deals in mono) sees the same shape either way.
fn build_sip_decoder(codec: CodecType) -> Box<dyn Decoder> {
    audio_codec::create_decoder(codec)
}

/// Construct the SIP-side audio encoder. Mono Opus produces valid mono
/// packets that Linphone (and any RFC-7587-compliant peer) decodes
/// correctly via the per-packet TOC byte. Keeping this mono avoids the
/// stereo encoder's duplicate-mono-to-stereo path (which doubles the
/// encode cost for no quality benefit, since LiveKit feeds us mono).
fn build_sip_encoder(codec: CodecType) -> Box<dyn Encoder> {
    match codec {
        CodecType::Opus => Box::new(OpusEncoder::new(48_000, 1)),
        _ => audio_codec::create_encoder(codec),
    }
}
use dashmap::DashMap;
use futures::StreamExt;
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame as LkAudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use rustrtc::PeerConnection;
use rustrtc::config::AudioCapability;
use rustrtc::media::frame::{AudioFrame as RtcAudioFrame, MediaSample};
use rustrtc::media::track::{MediaStreamTrack, sample_track};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::proxy::bridge::session::{BridgeKind, MediaBridge};

/// 20 ms ptime — matches both LiveKit's recommended chunk size and the
/// standard SIP ptime.
const PTIME_MS: u32 = 20;
const LK_SAMPLE_RATE: u32 = 48_000;
/// 960 samples at 48 kHz = 20 ms mono.
const LK_FRAME_SAMPLES: usize = (LK_SAMPLE_RATE as usize / 1000) * PTIME_MS as usize;

/// Media-plane bridge between a SIP-side rustrtc PeerConnection (RTP mode)
/// and a LiveKit Room. Wraps the publisher source + subscribed streams +
/// a cancellation token; dropping the last Arc cancels the token and the
/// forward tasks exit.
pub struct LiveKitBridge {
    /// Inbound RTP-mode PC (SIP carrier side).
    pub sip_pc: PeerConnection,
    /// What codec the SIP-side negotiation settled on. Drives the
    /// LiveKit→SIP encoder and the SIP→LiveKit decoder.
    pub sip_codec: AudioCapability,
    /// SIP-side telephone-event PT, if the carrier offered it. v1 logs and
    /// drops DTMF packets; no DTMF→LiveKit forward.
    pub sip_dtmf_pt: Option<u8>,
    /// Audio source the SIP→LiveKit forward task feeds.
    pub local_source: NativeAudioSource,
    /// Room handle (shared with teardown via Arc — `livekit::Room` is not
    /// `Clone`).
    pub _room: Arc<Room>,
    /// Subscribed remote audio streams indexed by participant identity.
    /// Task C populates/removes; task B pulls 20 ms frames every tick.
    pub subscribers: Arc<DashMap<String, NativeAudioStream>>,
    /// Mutex-wrapped to allow `start()` to `.take()` the receiver into
    /// the task C closure.
    pub events_rx: Mutex<Option<mpsc::UnboundedReceiver<RoomEvent>>>,
    /// Drop-aware lifecycle signal.
    pub cancel_token: CancellationToken,
    /// For logs.
    pub trunk_name: String,
    /// If set, task D (no-bot watchdog) cancels the bridge after this many
    /// milliseconds when `subscribers` is still empty — closes the silent-
    /// empty-room gap that agent_dispatch / webhook alone can't guarantee.
    pub bot_join_timeout_ms: Option<u64>,
    /// If set, task B emits a low-amplitude sine tone at this frequency on
    /// the SIP-RTP side whenever `subscribers` is empty (silence otherwise).
    pub hold_tone_hz: Option<u16>,
    /// Side-channel for the disconnect cause. Whichever internal task
    /// trips `cancel_token` first writes its cause here; the
    /// `watch_disconnect()` future then surfaces it to `call.rs`'s
    /// teardown so the CDR reflects the real reason. Defaults to
    /// `ByCallee` (room disconnected by LiveKit server / network),
    /// overridden by the watchdog to `BotJoinTimeout`.
    pub disconnect_cause:
        Arc<parking_lot::Mutex<crate::proxy::bridge::session::BridgeHangupCause>>,
}

impl Drop for LiveKitBridge {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

/// Translate the rustrtc `AudioCapability` to the `audio_codec::CodecType`
/// the decoder/encoder factory understands. Falls back to PCMU if the
/// codec name is not recognised — better to flow garbage audio than to
/// silently drop the call.
fn codec_type_for(cap: &AudioCapability) -> anyhow::Result<CodecType> {
    CodecType::try_from(cap.codec_name.as_str())
        .map_err(|e| anyhow!("unsupported SIP codec '{}': {e}", cap.codec_name))
}

/// Map a LiveKit `DisconnectReason` to a short Prometheus-friendly label
/// for the `rustpbx_livekit_room_disconnects_total{cause}` counter.
/// Closes L9 — preserves the granularity of the SDK's reason in metrics
/// instead of collapsing everything to "server".
fn disconnect_reason_label(reason: i32) -> &'static str {
    // `livekit::RoomEvent::Disconnected.reason` is a `DisconnectReason`
    // enum represented as i32 by prost. We match the variant codes
    // directly rather than depending on the imported enum names.
    match reason {
        0 => "unknown",
        1 => "client_initiated",
        2 => "duplicate_identity",
        3 => "server_shutdown",
        4 => "participant_removed",
        5 => "room_deleted",
        6 => "state_mismatch",
        7 => "join_failure",
        8 => "migration",
        9 => "signal_close",
        10 => "room_closed",
        11 => "user_unavailable",
        12 => "user_rejected",
        13 => "sip_trunk_failure",
        14 => "connection_timeout",
        _ => "other",
    }
}

/// Spawn the no-bot watchdog. Called once at start-up by `start()`, and
/// re-armed by Task C whenever `subscribers` transitions back to empty
/// (closes L7 — bot drops mid-call shouldn't leave a silent room).
///
/// `label` is logged so operators can distinguish initial-arm firings
/// from re-arm firings in the trunk's logs.
fn spawn_no_bot_watchdog(
    timeout_ms: u64,
    label: &'static str,
    cancel: CancellationToken,
    subscribers: Arc<DashMap<String, NativeAudioStream>>,
    trunk_name: String,
    disconnect_cause: Arc<parking_lot::Mutex<crate::proxy::bridge::session::BridgeHangupCause>>,
) {
    tokio::spawn(async move {
        tracing::info!(
            trunk = %trunk_name,
            timeout_ms,
            label,
            "LiveKit no-bot watchdog armed"
        );
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!(trunk = %trunk_name, label,
                    "LiveKit watchdog: cancelled before timeout");
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
                if subscribers.is_empty() {
                    // Race protection (M4): yield once so any in-flight
                    // TrackSubscribed event from Task C has a chance to
                    // land before we fire.
                    tokio::task::yield_now().await;
                    if subscribers.is_empty() {
                        tracing::warn!(
                            trunk = %trunk_name,
                            timeout_ms,
                            label,
                            "LiveKit bot did not join within timeout — aborting call"
                        );
                        *disconnect_cause.lock() =
                            crate::proxy::bridge::session::BridgeHangupCause::BotJoinTimeout;
                        crate::metrics::bridge::livekit_room_disconnect_inc("watchdog");
                        cancel.cancel();
                        return;
                    }
                }
                tracing::info!(
                    trunk = %trunk_name,
                    subscriber_count = subscribers.len(),
                    label,
                    "LiveKit bot present before watchdog fired"
                );
            }
        }
    });
}

#[async_trait]
impl MediaBridge for LiveKitBridge {
    async fn start(&self) -> anyhow::Result<()> {
        let trunk_name = self.trunk_name.clone();
        let cancel = self.cancel_token.clone();

        // Resolve the SIP-side codec up front. If the negotiation produced
        // something we can't transcode we refuse to spawn the forward
        // tasks — the call will time out and tear down naturally.
        let sip_codec_type = codec_type_for(&self.sip_codec)?;
        let sip_clock_rate = self.sip_codec.clock_rate;
        let sip_pt = self.sip_codec.payload_type;
        let sip_channels = self.sip_codec.channels.max(1);

        // ── Outbound SIP sender: attach a sample-track to sip_pc so task B
        // ── can push encoded audio back to the carrier.  Done here (rather
        // ── than at PC build time) so the SDP answer is already in place
        // ── and the codec params reflect the negotiated PT/clock.
        let (sip_send, sip_send_track, _sip_send_feedback) =
            sample_track(rustrtc::media::frame::MediaKind::Audio, 100);
        let sip_send_params = rustrtc::RtpCodecParameters {
            payload_type: sip_pt,
            clock_rate: sip_clock_rate,
            channels: sip_channels,
        };
        if let Err(e) = self.sip_pc.add_track(sip_send_track, sip_send_params) {
            // Not fatal for task A (caller→bot still works); log and
            // proceed.  Task B will see `sip_send` but its sends will
            // simply not reach the wire if the PC rejected the track.
            tracing::warn!(
                trunk = %trunk_name,
                error = %e,
                "LiveKit bridge: failed to add outbound sample-track to sip_pc — \
                 LiveKit→SIP direction will be silent"
            );
        }

        // ── Task A: SIP RTP → LiveKit publish ─────────────────────────
        {
            let cancel = cancel.clone();
            let trunk_name = trunk_name.clone();
            let sip_pc_clone = self.sip_pc.clone();
            let local_source = self.local_source.clone();
            let dtmf_pt = self.sip_dtmf_pt;
            let codec_type = sip_codec_type;
            tokio::spawn(async move {
                tracing::info!(
                    trunk = %trunk_name,
                    codec = ?codec_type,
                    "LiveKit forward task A (SIP→LiveKit) started"
                );
                run_sip_to_livekit(
                    trunk_name.clone(),
                    sip_pc_clone,
                    codec_type,
                    dtmf_pt,
                    local_source,
                    cancel.clone(),
                )
                .await;
                tracing::info!(trunk = %trunk_name, "LiveKit forward task A exited");
            });
        }

        // ── Task B: LiveKit subscribed audio → SIP RTP ────────────────
        {
            let cancel = cancel.clone();
            let trunk_name = trunk_name.clone();
            let subscribers = self.subscribers.clone();
            let codec_type = sip_codec_type;
            let hold_tone_hz = self.hold_tone_hz;
            tokio::spawn(async move {
                tracing::info!(
                    trunk = %trunk_name,
                    codec = ?codec_type,
                    sip_clock_rate,
                    hold_tone_hz = ?hold_tone_hz,
                    "LiveKit forward task B (LiveKit→SIP) started"
                );
                run_livekit_to_sip(
                    trunk_name.clone(),
                    subscribers,
                    codec_type,
                    sip_clock_rate,
                    sip_pt,
                    sip_send,
                    cancel.clone(),
                    hold_tone_hz,
                )
                .await;
                tracing::info!(trunk = %trunk_name, "LiveKit forward task B exited");
            });
        }

        // ── Task D: no-bot-join watchdog (optional, opt-in) ───────────
        // Initial arming at start-up: closes the silent-empty-room gap
        // at call setup. Task C re-arms a fresh watchdog every time the
        // subscriber set transitions back to empty (i.e. all bots left
        // mid-call) — closes L7.
        if let Some(timeout_ms) = self.bot_join_timeout_ms {
            spawn_no_bot_watchdog(
                timeout_ms,
                "initial",
                cancel.clone(),
                self.subscribers.clone(),
                trunk_name.clone(),
                self.disconnect_cause.clone(),
            );
        }

        // ── Task C: Room events → subscriber map + disconnect ─────────
        {
            let mut rx = self
                .events_rx
                .lock()
                .await
                .take()
                .ok_or_else(|| anyhow!("LiveKitBridge::start called twice"))?;
            let cancel = cancel.clone();
            let trunk_name = trunk_name.clone();
            let subscribers = self.subscribers.clone();
            // Plumb so the re-arm path in ParticipantDisconnected can
            // call `spawn_no_bot_watchdog(...)` again when subscribers
            // drops back to empty (L7).
            let bot_join_timeout_ms = self.bot_join_timeout_ms;
            let disconnect_cause_for_c = self.disconnect_cause.clone();
            tokio::spawn(async move {
                tracing::info!(trunk = %trunk_name, "LiveKit forward task C (room events) started");
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        ev = rx.recv() => match ev {
                            None => {
                                tracing::info!(
                                    trunk = %trunk_name,
                                    "LiveKit room events channel closed"
                                );
                                cancel.cancel();
                                break;
                            }
                            Some(RoomEvent::TrackSubscribed { track, participant, .. }) => {
                                if let RemoteTrack::Audio(audio_track) = track {
                                    let identity = participant.identity().to_string();
                                    let stream = NativeAudioStream::new(
                                        audio_track.rtc_track(),
                                        LK_SAMPLE_RATE as i32,
                                        1,
                                    );
                                    tracing::info!(
                                        trunk = %trunk_name,
                                        participant = %identity,
                                        "LiveKit subscribed to remote audio track"
                                    );
                                    subscribers.insert(identity, stream);
                                } else {
                                    tracing::debug!(
                                        trunk = %trunk_name,
                                        "LiveKit subscribed to non-audio track — ignored"
                                    );
                                }
                            }
                            Some(RoomEvent::ParticipantDisconnected(p)) => {
                                let identity = p.identity().to_string();
                                if subscribers.remove(&identity).is_some() {
                                    tracing::info!(
                                        trunk = %trunk_name,
                                        participant = %identity,
                                        "LiveKit participant left — dropped audio subscriber"
                                    );
                                    // L7: if that drop took us from
                                    // "had-a-bot" back to "empty room"
                                    // mid-call, re-arm the watchdog.
                                    // Without this a bot that joins,
                                    // talks, then crashes leaves a
                                    // silent room until SIP BYE / RTP
                                    // timeout.
                                    if subscribers.is_empty()
                                        && !cancel.is_cancelled()
                                        && let Some(timeout) = bot_join_timeout_ms
                                    {
                                        spawn_no_bot_watchdog(
                                            timeout,
                                            "rearm_after_drop",
                                            cancel.clone(),
                                            subscribers.clone(),
                                            trunk_name.clone(),
                                            disconnect_cause_for_c.clone(),
                                        );
                                    }
                                }
                            }
                            Some(RoomEvent::Disconnected { reason }) => {
                                // L9: surface the reason in metrics and
                                // logs instead of collapsing to a generic
                                // "server" disconnect. Cause stays as the
                                // default `ByCallee` (LiveKit-side ended
                                // the session) — granular CDR cause split
                                // is a future enum-expansion change.
                                let reason_label = disconnect_reason_label(reason as i32);
                                tracing::warn!(
                                    trunk = %trunk_name,
                                    reason = reason_label,
                                    reason_code = reason as i32,
                                    "LiveKit room disconnected — cancelling bridge"
                                );
                                crate::metrics::bridge::livekit_room_disconnect_inc(
                                    reason_label,
                                );
                                cancel.cancel();
                                break;
                            }
                            Some(_) => {}
                        }
                    }
                }
                tracing::info!(trunk = %trunk_name, "LiveKit forward task C exited");
            });
        }

        Ok(())
    }

    fn kind(&self) -> BridgeKind {
        BridgeKind::LiveKit
    }

    /// Cancel the forwarder tasks on SIP-side teardown so the disconnect
    /// watcher releases its Arc, rather than relying on the async
    /// `RoomEvent::Disconnected` from `room.close()` firing in time. Idempotent
    /// with the room-close path.
    fn shutdown(&self) {
        self.cancel_token.cancel();
    }

    /// Surface the LiveKit-side disconnect signal so call.rs can drive SIP
    /// teardown when the room ends (task C cancels the token on
    /// `RoomEvent::Disconnected`, task D on the no-bot watchdog).
    /// The output carries the specific cause whichever task tripped the
    /// cancel set in `disconnect_cause`.
    fn watch_disconnect(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                Output = crate::proxy::bridge::session::BridgeHangupCause,
            > + Send
                + '_,
        >,
    > {
        let token = self.cancel_token.clone();
        let cause = self.disconnect_cause.clone();
        Box::pin(async move {
            token.cancelled().await;
            *cause.lock()
        })
    }
}

/// Drain RTP audio from the SIP PC, decode + resample to 48 kHz mono PCM,
/// batch into 20 ms chunks, and push each chunk to the LiveKit publisher
/// source.
async fn run_sip_to_livekit(
    trunk_name: String,
    sip_pc: PeerConnection,
    codec_type: CodecType,
    dtmf_pt: Option<u8>,
    local_source: NativeAudioSource,
    cancel: CancellationToken,
) {
    let mut decoder: Box<dyn Decoder> = build_sip_decoder(codec_type);
    let decoder_rate = decoder.sample_rate();
    let mut resampler: Option<VoiceResampler> = if decoder_rate != LK_SAMPLE_RATE {
        Some(VoiceResampler::new(
            decoder_rate as usize,
            LK_SAMPLE_RATE as usize,
        ))
    } else {
        None
    };

    // Accumulator for 20 ms-aligned LK frames.  Decoder output is variable
    // (one RTP packet may decode to one ptime; for Opus this is already
    // 20 ms @ 48 kHz; for PCMU 20 ms @ 8 kHz → 480 @ 48 kHz after
    // resample, so half a LK frame).  We coalesce until we have ≥ 960.
    let mut pcm_buf: Vec<i16> = Vec::with_capacity(LK_FRAME_SAMPLES * 2);
    let mut frames_in: u64 = 0;
    let mut frames_out: u64 = 0;
    // Rolling audio-level stats, reset at each progress log.
    let mut peak_abs: i32 = 0;
    let mut sum_sq: f64 = 0.0;
    let mut sample_count: u64 = 0;

    // The track event arrives once when the carrier's RTP first hits us.
    // From then on we drive everything off `track.recv()`.
    let track = loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            ev = sip_pc.recv() => match ev {
                Some(rustrtc::PeerConnectionEvent::Track(transceiver)) => {
                    if transceiver.kind() == rustrtc::MediaKind::Audio
                        && let Some(receiver) = transceiver.receiver() {
                            break receiver.track();
                        }
                }
                Some(_) => continue,
                None => {
                    tracing::info!(trunk = %trunk_name, "SIP PC closed before audio track event");
                    return;
                }
            }
        }
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            sample_result = track.recv() => {
                let sample = match sample_result {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!(trunk = %trunk_name, error = %e,
                            "SIP track recv error — task A exiting");
                        break;
                    }
                };
                let frame = match sample {
                    MediaSample::Audio(f) => f,
                    MediaSample::Video(_) => continue,
                };
                frames_in += 1;
                // DTMF (RFC 4733) — skip silently in v1.
                if let Some(dpt) = dtmf_pt
                    && frame.payload_type == Some(dpt)
                {
                    tracing::trace!(trunk = %trunk_name, "SIP DTMF event dropped (not forwarded to LiveKit in v1)");
                    continue;
                }

                let pcm = decoder.decode(&frame.data);
                if pcm.is_empty() {
                    continue;
                }
                let pcm_48k = match resampler.as_mut() {
                    Some(r) => r.resample(&pcm),
                    None => pcm,
                };
                pcm_buf.extend_from_slice(&pcm_48k);

                // Emit complete 20 ms LK frames.
                while pcm_buf.len() >= LK_FRAME_SAMPLES {
                    let chunk: Vec<i16> = pcm_buf.drain(..LK_FRAME_SAMPLES).collect();
                    // Audio-level stats over the chunk: peak abs amplitude
                    // and accumulated sum-of-squares for RMS. Reset at each
                    // progress log so the numbers reflect ~10 s of audio.
                    for &s in &chunk {
                        let a = (s as i32).unsigned_abs() as i32;
                        if a > peak_abs {
                            peak_abs = a;
                        }
                        sum_sq += (s as f64) * (s as f64);
                    }
                    sample_count += chunk.len() as u64;
                    let lk_frame = LkAudioFrame {
                        data: chunk.into(),
                        sample_rate: LK_SAMPLE_RATE,
                        num_channels: 1,
                        samples_per_channel: LK_FRAME_SAMPLES as u32,
                    };
                    if let Err(e) = local_source.capture_frame(&lk_frame).await {
                        // capture_frame fails when the source is closed —
                        // exit cleanly rather than spin.
                        tracing::warn!(trunk = %trunk_name, error = %e,
                            "LiveKit local_source.capture_frame failed — task A exiting");
                        return;
                    }
                    frames_out += 1;
                    if frames_out == 1 {
                        tracing::info!(trunk = %trunk_name, "First SIP→LiveKit frame published");
                    }
                    if frames_out.is_multiple_of(500) {
                        let rms = if sample_count > 0 {
                            (sum_sq / sample_count as f64).sqrt() as i32
                        } else { 0 };
                        tracing::info!(
                            trunk = %trunk_name,
                            frames_in,
                            frames_out,
                            buf_residual = pcm_buf.len(),
                            peak = peak_abs,
                            rms,
                            "SIP→LiveKit progress"
                        );
                        peak_abs = 0;
                        sum_sq = 0.0;
                        sample_count = 0;
                    }
                }
            }
        }
    }
}

/// Every 20 ms, pull one frame from each subscribed remote audio stream,
/// pick a representative (v1: the first non-empty one — see comment),
/// resample down to the SIP codec rate, encode, and push to the SIP send
/// channel.
#[allow(clippy::too_many_arguments)]
async fn run_livekit_to_sip(
    trunk_name: String,
    subscribers: Arc<DashMap<String, NativeAudioStream>>,
    codec_type: CodecType,
    sip_clock_rate: u32,
    sip_pt: u8,
    sip_send: rustrtc::media::track::SampleStreamSource,
    cancel: CancellationToken,
    hold_tone_hz: Option<u16>,
) {
    let mut encoder: Box<dyn Encoder> = build_sip_encoder(codec_type);
    let encoder_rate = encoder.sample_rate();
    let mut resampler: Option<VoiceResampler> = if encoder_rate != LK_SAMPLE_RATE {
        Some(VoiceResampler::new(
            LK_SAMPLE_RATE as usize,
            encoder_rate as usize,
        ))
    } else {
        None
    };

    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(PTIME_MS as u64));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Skip the immediate first tick — wait one ptime so we don't burst.
    ticker.tick().await;

    let mut rtp_timestamp: u32 = rand::random();
    let mut sequence_number: u16 = rand::random();
    // RTP timestamp increment per ptime, in the SIP codec's clock domain.
    let ts_increment: u32 = sip_clock_rate / 1000 * PTIME_MS;
    let mut frames_sent: u64 = 0;
    // PCM accumulator @ 48 kHz mono. libwebrtc typically delivers 10 ms
    // frames (480 samples); we coalesce to exactly 20 ms (960) per RTP
    // packet so the receiver's clock-rate / ptime / sequence stay
    // perfectly aligned. Without this, a tick that sees only 10 ms of
    // buffered audio used to encode 10 ms but still advance RTP
    // timestamp by 20 ms — caller hears "speech, gap, speech, gap"
    // (= choppy bot voice).
    let mut pcm_accum: Vec<i16> = Vec::with_capacity(LK_FRAME_SAMPLES * 4);
    // Audio-level stats over the last progress window.
    let mut peak_abs: i32 = 0;
    let mut sum_sq: f64 = 0.0;
    let mut sample_count: u64 = 0;
    // Sine-wave phase, carried across ticks so the hold tone is
    // continuous (no audible clicks at frame boundaries).
    let mut hold_phase: f64 = 0.0;
    const HOLD_AMP: f64 = 1500.0; // quiet — ~ -25 dBFS

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                // L10: saturating-add PCM mixer. Non-blocking poll each
                // subscriber for any frames currently available, sum
                // them sample-by-sample into a single mix buffer with
                // i32 saturation. libwebrtc typically emits 10 ms
                // frames (480 samples @ 48 kHz); our tick is 20 ms so
                // a steady stream gives ~2 frames per tick. If frames
                // differ in length across subscribers we sum up to the
                // longer length (shorter ones contribute zero in the
                // tail).
                let mut mixed: Option<Vec<i16>> = None;
                let mut contributors: usize = 0;
                for mut entry in subscribers.iter_mut() {
                    let stream = entry.value_mut();
                    use futures::future::FutureExt;
                    // Drain everything immediately available — typically
                    // 1-2 frames @ 10 ms each.
                    while let Some(Some(lk_frame)) = stream.next().now_or_never() {
                        let samples: Vec<i16> = lk_frame.data.into_owned();
                        if samples.is_empty() {
                            continue;
                        }
                        match mixed.as_mut() {
                            None => {
                                mixed = Some(samples);
                                contributors = 1;
                            }
                            Some(mix) => {
                                contributors += 1;
                                if samples.len() > mix.len() {
                                    mix.resize(samples.len(), 0);
                                }
                                for (m, s) in mix.iter_mut().zip(samples.iter()) {
                                    *m = (*m as i32 + *s as i32)
                                        .clamp(i16::MIN as i32, i16::MAX as i32)
                                        as i16;
                                }
                            }
                        }
                    }
                }
                let _ = contributors; // surfaced for debug tooling

                match mixed {
                    Some(p) => {
                        // Reset hold phase so the next silent gap starts at zero
                        // (avoids a click if the bot drops out and rejoins).
                        hold_phase = 0.0;
                        pcm_accum.extend_from_slice(&p);
                    }
                    None => match hold_tone_hz {
                        // No subscriber audio this tick. If a hold tone is
                        // configured, top up the accumulator with one ptime
                        // worth of tone so the caller hears a continuous
                        // ringback. Otherwise let the accumulator drain
                        // naturally — caller hears silence.
                        Some(hz) if hz > 0 => {
                            let step = std::f64::consts::TAU * (hz as f64)
                                / (LK_SAMPLE_RATE as f64);
                            pcm_accum.reserve(LK_FRAME_SAMPLES);
                            for _ in 0..LK_FRAME_SAMPLES {
                                pcm_accum.push((hold_phase.sin() * HOLD_AMP) as i16);
                                hold_phase += step;
                                if hold_phase >= std::f64::consts::TAU {
                                    hold_phase -= std::f64::consts::TAU;
                                }
                            }
                        }
                        _ => {}
                    },
                }

                // Drain in fixed 20 ms (LK_FRAME_SAMPLES) chunks so the RTP
                // timestamp / sequence increments line up with what we
                // actually encoded. If multiple chunks are ready (libwebrtc
                // burst), we emit multiple RTP packets back-to-back; if
                // fewer than 20 ms is available, we wait for the next tick.
                while pcm_accum.len() >= LK_FRAME_SAMPLES {
                    let pcm_48k: Vec<i16> = pcm_accum.drain(..LK_FRAME_SAMPLES).collect();
                    for &s in &pcm_48k {
                        let a = (s as i32).unsigned_abs() as i32;
                        if a > peak_abs {
                            peak_abs = a;
                        }
                        sum_sq += (s as f64) * (s as f64);
                    }
                    sample_count += pcm_48k.len() as u64;
                    let pcm_sip_rate: Vec<i16> = match resampler.as_mut() {
                        Some(r) => r.resample(&pcm_48k),
                        None => pcm_48k,
                    };
                    if pcm_sip_rate.is_empty() {
                        continue;
                    }
                    let encoded = encoder.encode(&pcm_sip_rate);
                    if encoded.is_empty() {
                        continue;
                    }

                    let frame = RtcAudioFrame {
                        rtp_timestamp,
                        clock_rate: sip_clock_rate,
                        data: encoded.into(),
                        sequence_number: Some(sequence_number),
                        payload_type: Some(sip_pt),
                        marker: false,
                        header_extension: None,
                        source_addr: None,
                        raw_packet: None,
                    };
                    if let Err(e) = sip_send.send_audio(frame).await {
                        tracing::debug!(trunk = %trunk_name, error = %e,
                            "SIP send_audio failed — task B exiting");
                        return;
                    }
                    rtp_timestamp = rtp_timestamp.wrapping_add(ts_increment);
                    sequence_number = sequence_number.wrapping_add(1);
                    frames_sent += 1;
                    if frames_sent == 1 {
                        tracing::info!(trunk = %trunk_name, "First LiveKit→SIP frame sent");
                    }
                    if frames_sent.is_multiple_of(500) {
                        let rms = if sample_count > 0 {
                            (sum_sq / sample_count as f64).sqrt() as i32
                        } else { 0 };
                        tracing::info!(trunk = %trunk_name, frames_sent,
                            buf_residual = pcm_accum.len(),
                            peak = peak_abs, rms,
                            "LiveKit→SIP progress");
                        peak_abs = 0;
                        sum_sq = 0.0;
                        sample_count = 0;
                    }
                }
            }
        }
    }
}
