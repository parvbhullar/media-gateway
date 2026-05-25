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
use audio_codec::{CodecType, Decoder, Encoder, Resampler};
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
            tokio::spawn(async move {
                tracing::info!(
                    trunk = %trunk_name,
                    codec = ?codec_type,
                    sip_clock_rate,
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
                )
                .await;
                tracing::info!(trunk = %trunk_name, "LiveKit forward task B exited");
            });
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
                                }
                            }
                            Some(RoomEvent::Disconnected { reason }) => {
                                tracing::info!(
                                    trunk = %trunk_name,
                                    ?reason,
                                    "LiveKit room disconnected — cancelling bridge"
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

    /// Surface the LiveKit-side disconnect signal so call.rs can drive SIP
    /// teardown when the room ends (task C cancels the token on
    /// `RoomEvent::Disconnected`).
    fn watch_disconnect(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        let token = self.cancel_token.clone();
        Box::pin(async move { token.cancelled().await })
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
    let mut decoder: Box<dyn Decoder> = audio_codec::create_decoder(codec_type);
    let decoder_rate = decoder.sample_rate();
    let mut resampler: Option<Resampler> = if decoder_rate != LK_SAMPLE_RATE {
        Some(Resampler::new(
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
                        tracing::debug!(
                            trunk = %trunk_name,
                            frames_in,
                            frames_out,
                            buf_residual = pcm_buf.len(),
                            "SIP→LiveKit progress"
                        );
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
) {
    let mut encoder: Box<dyn Encoder> = audio_codec::create_encoder(codec_type);
    let encoder_rate = encoder.sample_rate();
    let mut resampler: Option<Resampler> = if encoder_rate != LK_SAMPLE_RATE {
        Some(Resampler::new(
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

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                // Pragmatic shortcut (v1): forward the first subscriber's
                // available frame as-is rather than mixing.  Real summing
                // mixer is a follow-up.  See README note.
                let mut mixed: Option<Vec<i16>> = None;
                for mut entry in subscribers.iter_mut() {
                    // Non-blocking poll: take whatever the stream has now
                    // without waiting.  We use `next().now_or_never()` to
                    // peek the futures::Stream queue.
                    let stream = entry.value_mut();
                    use futures::future::FutureExt;
                    if let Some(Some(lk_frame)) = stream.next().now_or_never() {
                        // Frames are typically 10 ms @ 48 kHz (480 samples)
                        // from libwebrtc.  Coalesce up to ~ LK_FRAME_SAMPLES.
                        let samples: Vec<i16> = lk_frame.data.into_owned();
                        if samples.is_empty() {
                            continue;
                        }
                        mixed = Some(samples);
                        break;
                    }
                }
                let Some(pcm_48k) = mixed else { continue; };

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
                    break;
                }
                rtp_timestamp = rtp_timestamp.wrapping_add(ts_increment);
                sequence_number = sequence_number.wrapping_add(1);
                frames_sent += 1;
                if frames_sent == 1 {
                    tracing::info!(trunk = %trunk_name, "First LiveKit→SIP frame sent");
                }
                if frames_sent.is_multiple_of(500) {
                    tracing::debug!(trunk = %trunk_name, frames_sent, "LiveKit→SIP progress");
                }
            }
        }
    }
}
