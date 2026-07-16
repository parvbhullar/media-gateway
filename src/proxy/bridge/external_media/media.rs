//! ExternalMediaBridge — implements `MediaBridge` for `kind="external_media"`.
//!
//! Bridges a SIP-side rustrtc RTP PeerConnection to a co-located sidecar
//! process over a localhost UDP socket carrying raw 48 kHz / mono / 16-bit
//! PCM in 20 ms datagrams (1920 bytes). The sidecar owns the far-end media
//! (e.g. joining a LiveKit room as a participant).
//!
//!   * Task A — drains incoming SIP RTP, decodes via the negotiated SIP
//!     codec, resamples to 48 kHz mono, accumulates 20 ms (960-sample)
//!     chunks, and `send`s each to the sidecar over UDP.
//!   * Task B — receives 20 ms PCM datagrams from the sidecar, resamples
//!     to the SIP clock rate, encodes to the SIP codec, and sends via a
//!     sample-track on `sip_pc`. Non-1920-byte datagrams (control, e.g.
//!     `READY`/`BYE`) are handled separately.
//!
//! Mirrors `livekit::media::LiveKitBridge` Task A/B, with the LiveKit SDK
//! replaced by the UDP socket. SIP-side codec/resample logic is identical.

use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use audio_codec::opus::OpusEncoder;
use crate::media::resampler::VoiceResampler;
use audio_codec::{CodecType, Decoder, Encoder};
use parking_lot::RwLock;
use rustrtc::PeerConnection;
use rustrtc::config::AudioCapability;
use rustrtc::media::frame::{AudioFrame as RtcAudioFrame, MediaSample};
use rustrtc::media::track::{MediaStreamTrack, sample_track};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::media::recorder::{Leg, Recorder};
use crate::proxy::bridge::session::{BridgeKind, MediaBridge};

/// Shared recorder handle (leg A = SIP caller, leg B = sidecar/agent),
/// attached by `call.rs` before `start()`.
type SharedRecorder = Arc<RwLock<Option<Recorder>>>;

const PTIME_MS: u32 = 20;

/// One 20 ms mono PCM frame at `pcm_rate` Hz, in samples (e.g. 960 @ 48 kHz,
/// 480 @ 24 kHz — the preferred AI-consumer rate).
fn pcm_frame_samples(pcm_rate: u32) -> usize {
    (pcm_rate as usize / 1000) * PTIME_MS as usize
}

/// Stereo-default Opus decoder (downmixes to mono) — Linphone sends
/// `opus/48000/2`; a mono decoder would produce silence. See livekit::media.
fn build_sip_decoder(codec: CodecType) -> Box<dyn Decoder> {
    audio_codec::create_decoder(codec)
}

/// Mono Opus encoder (valid mono via per-packet TOC).
///
/// NOTE: audio_codec 0.3.30's `OpusEncoder::new` hardcodes
/// `OPUS_APPLICATION_VOIP` (speech high-pass / band-limit) and exposes no
/// bitrate/complexity/application tuning. That VoIP profile is why the bot
/// sounds thinner than the webrtc kind, which passes the bot's native
/// full-band Opus through untouched. Making it fuller needs either an
/// audio_codec bump to 0.3.33 (adds `new_with_application`/`set_bitrate`/
/// `set_complexity`) or a custom opusic_sys encoder — see build notes.
fn build_sip_encoder(codec: CodecType) -> Box<dyn Encoder> {
    match codec {
        CodecType::Opus => Box::new(OpusEncoder::new(48_000, 1)),
        _ => audio_codec::create_encoder(codec),
    }
}

fn codec_type_for(cap: &AudioCapability) -> anyhow::Result<CodecType> {
    CodecType::try_from(cap.codec_name.as_str())
        .map_err(|e| anyhow!("unsupported SIP codec '{}': {e}", cap.codec_name))
}

/// One unit of recording work handed off the media hot path.
struct RecItem {
    leg: Leg,
    sample: MediaSample,
}

/// Drain recording items on a dedicated task so the codec work + periodic
/// disk flush inside `Recorder::write_sample` never run on the realtime
/// forwarding tasks (Task A/B) — inline recording previously caused audible
/// jitter + lock contention that thinned the recorded caller leg. Owns the
/// recorder exclusively. `codec_hint` is the SIP voice codec both legs carry.
async fn run_recorder(
    recorder: SharedRecorder,
    codec_hint: CodecType,
    dtmf_pt: Option<u8>,
    mut rx: mpsc::Receiver<RecItem>,
) {
    while let Some(item) = rx.recv().await {
        // Only leg A (the SIP caller) can carry RFC 2833 DTMF.
        let leg_dtmf = if item.leg == Leg::A { dtmf_pt } else { None };
        if let Some(r) = recorder.write().as_mut() {
            let _ = r.write_sample(item.leg, &item.sample, leg_dtmf, None, Some(codec_hint));
        }
    }
}

/// Media-plane bridge between a SIP-side RTP PC and a sidecar PCM socket.
pub struct ExternalMediaBridge {
    /// Inbound RTP-mode PC (SIP carrier side).
    pub sip_pc: PeerConnection,
    /// SIP-side negotiated voice codec (drives decoder/encoder).
    pub sip_codec: AudioCapability,
    /// SIP-side telephone-event PT (RFC 2833); logged + dropped in v1.
    pub sip_dtmf_pt: Option<u8>,
    /// UDP socket bound on 127.0.0.1, connected to the sidecar's address
    /// (learned at dispatch time from the sidecar's READY datagram).
    pub sock: Arc<UdpSocket>,
    /// PCM sample rate on the sidecar datagram pipe (from trunk config;
    /// 24 kHz preferred for AI consumers, 48 kHz default).
    pub pcm_rate: u32,
    /// Drop-aware lifecycle signal.
    pub cancel_token: CancellationToken,
    /// For logs.
    pub trunk_name: String,
    /// Disconnect cause surfaced to call.rs teardown via `watch_disconnect`.
    pub disconnect_cause:
        Arc<parking_lot::Mutex<crate::proxy::bridge::session::BridgeHangupCause>>,
    /// Shared call recorder, attached by `call.rs` before `start()`. Leg A =
    /// SIP caller, leg B = sidecar/agent. `None` when recording disabled.
    pub recorder: RwLock<Option<SharedRecorder>>,
}

impl Drop for ExternalMediaBridge {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

#[async_trait]
impl MediaBridge for ExternalMediaBridge {
    async fn start(&self) -> anyhow::Result<()> {
        let trunk_name = self.trunk_name.clone();
        let cancel = self.cancel_token.clone();

        let sip_codec_type = codec_type_for(&self.sip_codec)?;
        let sip_clock_rate = self.sip_codec.clock_rate;
        let sip_pt = self.sip_codec.payload_type;
        let sip_channels = self.sip_codec.channels.max(1);

        // Outbound SIP sender track (Task B pushes encoded audio here).
        let (sip_send, sip_send_track, _fb) =
            sample_track(rustrtc::media::frame::MediaKind::Audio, 100);
        let sip_send_params = rustrtc::RtpCodecParameters {
            payload_type: sip_pt,
            clock_rate: sip_clock_rate,
            channels: sip_channels,
        };
        if let Err(e) = self.sip_pc.add_track(sip_send_track, sip_send_params) {
            tracing::warn!(trunk = %trunk_name, error = %e,
                "external_media: failed to add outbound SIP track — sidecar→SIP will be silent");
        }

        // Recording runs on its own task fed by a bounded channel, so the
        // codec work + periodic disk flush never block the realtime media
        // tasks. `try_send` from the media tasks is non-blocking and drops on
        // backpressure — recording is best-effort, live audio never waits.
        let rec_tx: Option<mpsc::Sender<RecItem>> =
            self.recorder.read().clone().map(|recorder| {
                let (tx, rx) = mpsc::channel::<RecItem>(500);
                let dtmf_pt = self.sip_dtmf_pt;
                tokio::spawn(run_recorder(recorder, sip_codec_type, dtmf_pt, rx));
                tx
            });

        // ── Task A: SIP RTP → sidecar PCM ─────────────────────────────
        {
            let cancel = cancel.clone();
            let trunk_name = trunk_name.clone();
            let sip_pc = self.sip_pc.clone();
            let sock = self.sock.clone();
            let dtmf_pt = self.sip_dtmf_pt;
            let codec_type = sip_codec_type;
            let pcm_rate = self.pcm_rate;
            let rec_tx = rec_tx.clone();
            tokio::spawn(async move {
                tracing::info!(trunk = %trunk_name, codec = ?codec_type,
                    "external_media task A (SIP→sidecar) started");
                run_sip_to_sidecar(trunk_name.clone(), sip_pc, codec_type, dtmf_pt, sock,
                    pcm_rate, rec_tx, cancel)
                    .await;
                tracing::info!(trunk = %trunk_name, "external_media task A exited");
            });
        }

        // ── Task B: sidecar PCM → SIP RTP ─────────────────────────────
        {
            let cancel = cancel.clone();
            let trunk_name = trunk_name.clone();
            let sock = self.sock.clone();
            let codec_type = sip_codec_type;
            let pcm_rate = self.pcm_rate;
            tokio::spawn(async move {
                tracing::info!(trunk = %trunk_name, codec = ?codec_type, sip_clock_rate,
                    "external_media task B (sidecar→SIP) started");
                run_sidecar_to_sip(trunk_name.clone(), sock, codec_type, sip_clock_rate, sip_pt,
                    pcm_rate, sip_send, rec_tx, cancel)
                    .await;
                tracing::info!(trunk = %trunk_name, "external_media task B exited");
            });
        }

        Ok(())
    }

    fn kind(&self) -> BridgeKind {
        BridgeKind::ExternalMedia
    }

    fn attach_recorder(&self, recorder: SharedRecorder) {
        *self.recorder.write() = Some(recorder);
    }

    fn watch_disconnect(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = crate::proxy::bridge::session::BridgeHangupCause>
                + Send
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

/// Task A: drain SIP RTP → decode → resample to `pcm_rate` → 20 ms PCM →
/// UDP `send`.
#[allow(clippy::too_many_arguments)]
async fn run_sip_to_sidecar(
    trunk_name: String,
    sip_pc: PeerConnection,
    codec_type: CodecType,
    dtmf_pt: Option<u8>,
    sock: Arc<UdpSocket>,
    pcm_rate: u32,
    rec_tx: Option<mpsc::Sender<RecItem>>,
    cancel: CancellationToken,
) {
    let mut decoder: Box<dyn Decoder> = build_sip_decoder(codec_type);
    let decoder_rate = decoder.sample_rate();
    let mut resampler: Option<VoiceResampler> = if decoder_rate != pcm_rate {
        Some(VoiceResampler::new(decoder_rate as usize, pcm_rate as usize))
    } else {
        None
    };

    let frame_samples = pcm_frame_samples(pcm_rate);
    let frame_bytes = frame_samples * 2;
    let mut pcm_buf: Vec<i16> = Vec::with_capacity(frame_samples * 2);
    let mut frames_out: u64 = 0;

    // Wait for the inbound audio track.
    let track = loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            ev = sip_pc.recv() => match ev {
                Some(rustrtc::PeerConnectionEvent::Track(t)) => {
                    if t.kind() == rustrtc::MediaKind::Audio
                        && let Some(r) = t.receiver() {
                            break r.track();
                        }
                }
                Some(_) => continue,
                None => {
                    tracing::info!(trunk = %trunk_name, "SIP PC closed before audio track");
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
                // Record the caller leg (leg A) off the hot path — including
                // DTMF, which the recorder renders via its own DTMF path.
                // Non-blocking; drops on backpressure.
                if let Some(tx) = &rec_tx
                    && matches!(sample, MediaSample::Audio(_))
                {
                    let _ = tx.try_send(RecItem { leg: Leg::A, sample: sample.clone() });
                }
                let frame = match &sample {
                    MediaSample::Audio(f) => f,
                    MediaSample::Video(_) => continue,
                };
                // DTMF (RFC 2833) — not forwarded to the sidecar in v1.
                if let Some(dpt) = dtmf_pt
                    && frame.payload_type == Some(dpt)
                {
                    continue;
                }
                let pcm = decoder.decode(&frame.data);
                if pcm.is_empty() {
                    continue;
                }
                let pcm_out = match resampler.as_mut() {
                    Some(r) => r.resample(&pcm),
                    None => pcm,
                };
                pcm_buf.extend_from_slice(&pcm_out);

                while pcm_buf.len() >= frame_samples {
                    let chunk: Vec<i16> = pcm_buf.drain(..frame_samples).collect();
                    let mut bytes = Vec::with_capacity(frame_bytes);
                    for s in &chunk {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                    if let Err(e) = sock.send(&bytes).await {
                        tracing::debug!(trunk = %trunk_name, error = %e,
                            "sidecar UDP send failed — task A exiting");
                        return;
                    }
                    frames_out += 1;
                    if frames_out == 1 {
                        tracing::info!(trunk = %trunk_name, "First SIP→sidecar frame sent");
                    }
                }
            }
        }
    }
}

/// Buffers resampled PCM to exact `samples_per_frame`-sized chunks before
/// encoding — block codecs like Opus reject off-size buffers, and the
/// resampler's output is not guaranteed frame-exact (short first call after
/// startup trim; the polyphase fallback jitters ±1 sample). Extracted from
/// `run_sidecar_to_sip` so the accumulation/priming logic is unit-testable
/// without socket I/O or a real encoder.
struct FrameAccumulator {
    samples_per_frame: usize,
    buf: Vec<i16>,
    primed: bool,
}

impl FrameAccumulator {
    fn new(samples_per_frame: usize) -> Self {
        Self { samples_per_frame: samples_per_frame.max(1), buf: Vec::new(), primed: false }
    }

    /// Feed newly resampled PCM; returns zero or more complete, exactly
    /// `samples_per_frame`-sized frames. Any remainder is carried internally
    /// to the next call.
    fn push(&mut self, samples: &[i16]) -> Vec<Vec<i16>> {
        if !self.primed && !samples.is_empty() {
            // Absorb the resampler's one-time startup trim: left-pad the
            // first output to a frame boundary so the accumulator holds ~0
            // samples in steady state (no standing latency).
            let pad = (self.samples_per_frame - samples.len() % self.samples_per_frame)
                % self.samples_per_frame;
            self.buf.resize(pad, 0);
            self.primed = true;
        }
        self.buf.extend_from_slice(samples);
        let mut frames = Vec::new();
        while self.buf.len() >= self.samples_per_frame {
            frames.push(self.buf.drain(..self.samples_per_frame).collect());
        }
        frames
    }
}

/// Task B: receive 20 ms PCM datagrams from the sidecar → resample to SIP
/// rate → encode → send on the SIP track (recv-driven).
#[allow(clippy::too_many_arguments)]
async fn run_sidecar_to_sip(
    trunk_name: String,
    sock: Arc<UdpSocket>,
    codec_type: CodecType,
    sip_clock_rate: u32,
    sip_pt: u8,
    pcm_rate: u32,
    sip_send: rustrtc::media::track::SampleStreamSource,
    rec_tx: Option<mpsc::Sender<RecItem>>,
    cancel: CancellationToken,
) {
    let mut encoder: Box<dyn Encoder> = build_sip_encoder(codec_type);
    let encoder_rate = encoder.sample_rate();
    let mut resampler: Option<VoiceResampler> = if encoder_rate != pcm_rate {
        Some(VoiceResampler::new(pcm_rate as usize, encoder_rate as usize))
    } else {
        None
    };
    let frame_bytes = pcm_frame_samples(pcm_rate) * 2;
    // Encode only exact 20 ms frames (same contract as Transcoder::transcode).
    let samples_per_frame = pcm_frame_samples(encoder_rate);
    let mut accumulator = FrameAccumulator::new(samples_per_frame);

    let mut rtp_timestamp: u32 = rand::random();
    let mut sequence_number: u16 = rand::random();
    let ts_increment: u32 = sip_clock_rate / 1000 * PTIME_MS;
    let mut frames_sent: u64 = 0;
    let mut buf = vec![0u8; 4096];

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            recv = sock.recv(&mut buf) => {
                let n = match recv {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::debug!(trunk = %trunk_name, error = %e,
                            "sidecar UDP recv error — task B exiting");
                        break;
                    }
                };
                // Sidecar-initiated hangup (LiveKit room closed / agent left):
                // cancel the bridge so call.rs tears the SIP dialog down. The
                // disconnect_cause default (ByCallee) is the correct CDR cause.
                if &buf[..n] == b"BYE" {
                    tracing::info!(trunk = %trunk_name,
                        "sidecar sent BYE — cancelling external_media bridge");
                    cancel.cancel();
                    break;
                }
                // Ignore other control datagrams (e.g. READY) — only process
                // exact 20 ms PCM frames. A size mismatch almost always means
                // the sidecar was built for a different pcm_sample_rate than
                // this trunk is configured for — operator action required.
                if n != frame_bytes {
                    tracing::warn!(
                        received_bytes = n,
                        expected_bytes = frame_bytes,
                        pcm_rate = pcm_rate,
                        "sidecar datagram size mismatch — \
                        sidecar pcm_sample_rate likely differs from trunk config; frame dropped"
                    );
                    continue;
                }
                let pcm_in: Vec<i16> = buf[..n]
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect();
                let pcm_sip: Vec<i16> = match resampler.as_mut() {
                    Some(r) => r.resample(&pcm_in),
                    None => pcm_in,
                };
                for chunk in accumulator.push(&pcm_sip) {
                    let encoded = encoder.encode(&chunk);
                    if encoded.is_empty() {
                        // 20 ms of timeline was consumed either way — keep RTP
                        // timestamps aligned with content on encoder hiccups.
                        rtp_timestamp = rtp_timestamp.wrapping_add(ts_increment);
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
                    // Record the agent leg (leg B) off the hot path, from the
                    // same encoded frame. Non-blocking; drops on backpressure.
                    if let Some(tx) = &rec_tx {
                        let _ = tx.try_send(RecItem {
                            leg: Leg::B,
                            sample: MediaSample::Audio(frame.clone()),
                        });
                    }
                    if let Err(e) = sip_send.send_audio(frame).await {
                        tracing::debug!(trunk = %trunk_name, error = %e,
                            "SIP send_audio failed — task B exiting");
                        return;
                    }
                    rtp_timestamp = rtp_timestamp.wrapping_add(ts_increment);
                    sequence_number = sequence_number.wrapping_add(1);
                    frames_sent += 1;
                    if frames_sent == 1 {
                        tracing::info!(trunk = %trunk_name, "First sidecar→SIP frame sent");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod frame_accumulator_tests {
    use super::*;

    #[test]
    fn post_priming_partial_push_waits_for_a_full_frame() {
        // Once primed on an exact multiple, later partial pushes must
        // buffer without emitting until a full frame accumulates.
        let mut acc = FrameAccumulator::new(160);
        assert_eq!(acc.push(&[1i16; 160]).len(), 1, "exact multiple primes cleanly");
        assert!(acc.push(&[2i16; 100]).is_empty(), "100 < 160, no frame yet");
    }

    #[test]
    fn every_emitted_frame_is_exactly_samples_per_frame() {
        // Feed odd-sized chunks (simulating the resampler's ±1 jitter) and
        // assert every frame that comes out is exactly the target size —
        // this is the guarantee that keeps Opus from rejecting the buffer.
        let mut acc = FrameAccumulator::new(160);
        let mut total_frames = 0;
        for chunk_len in [161, 159, 160, 158, 162] {
            for frame in acc.push(&vec![7i16; chunk_len]) {
                assert_eq!(frame.len(), 160, "frame must be exactly samples_per_frame");
                total_frames += 1;
            }
        }
        assert!(total_frames > 0, "5 chunks of ~160 samples must yield at least one frame");
    }

    #[test]
    fn short_first_push_is_left_padded_to_a_frame_boundary() {
        // Absorbs the resampler's one-time startup trim: a short first
        // output must not vanish or stall forever waiting for more data —
        // it gets padded with leading silence up to the frame boundary.
        let mut acc = FrameAccumulator::new(160);
        let frames = acc.push(&[9i16; 100]);
        assert_eq!(frames.len(), 1, "padding must complete exactly one frame");
        let frame = &frames[0];
        assert_eq!(frame.len(), 160);
        // First 60 samples are the left-pad (silence), last 100 are real audio.
        assert!(frame[..60].iter().all(|&s| s == 0), "pad must be silence");
        assert!(frame[60..].iter().all(|&s| s == 9), "real audio must follow the pad");
    }

    #[test]
    fn priming_only_happens_once() {
        // After the first (possibly short) push, later short pushes must
        // just accumulate normally — re-padding every push would inject
        // silence gaps throughout the stream, not just at startup.
        let mut acc = FrameAccumulator::new(160);
        let first = acc.push(&[1i16; 100]); // primes: 60-sample pad + 100 = one frame
        assert_eq!(first.len(), 1);
        let second = acc.push(&[2i16; 60]); // 60 alone isn't a full frame yet
        assert!(second.is_empty(), "no re-padding — must wait for more data");
        let third = acc.push(&[2i16; 100]); // 60 + 100 = 160, completes with no padding
        assert_eq!(third.len(), 1);
        assert!(third[0].iter().all(|&s| s == 2), "no silence injected on later pushes");
    }

    #[test]
    fn remainder_carries_across_pushes_and_no_samples_are_dropped() {
        let mut acc = FrameAccumulator::new(160);
        let mut delivered = 0usize;
        for _ in 0..10 {
            for frame in acc.push(&[3i16; 100]) {
                delivered += frame.len();
            }
        }
        // 10 * 100 = 1000 input samples; with a one-time 60-sample left-pad
        // the accumulator holds 1060 samples total, emitting 6 full frames
        // (960) and carrying 100 samples forward — nothing is lost.
        assert_eq!(delivered, 960, "6 full frames delivered, remainder carried internally");
    }

    #[test]
    fn empty_push_is_a_no_op_before_priming() {
        let mut acc = FrameAccumulator::new(160);
        assert!(acc.push(&[]).is_empty());
        assert!(!acc.primed, "an empty push must not consume the priming step");
    }
}
