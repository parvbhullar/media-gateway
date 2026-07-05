use crate::media::resampler::VoiceResampler;
use audio_codec::{CodecType, Decoder, Encoder, create_decoder, create_encoder};
use rand::RngExt;
use rustrtc::media::AudioFrame;

#[derive(Clone, Copy)]
struct TimestampDomain {
    first_input_timestamp: u32,
    first_output_timestamp: u32,
}

pub struct RtpTiming {
    domain: Option<TimestampDomain>,
    first_input_sequence: Option<u16>,
    first_output_timestamp: u32,
    first_output_sequence: u16,
}

impl Default for RtpTiming {
    fn default() -> Self {
        let mut rng = rand::rng();
        Self {
            domain: None,
            first_input_sequence: None,
            first_output_timestamp: rng.random(),
            first_output_sequence: rng.random(),
        }
    }
}

impl RtpTiming {
    pub fn rewrite(
        &mut self,
        frame: &mut AudioFrame,
        source_clock_rate: u32,
        target_clock_rate: u32,
        target_payload_type: u8,
    ) {
        let (rtp_timestamp, output_sequence) =
            self.new_timestamp(frame, source_clock_rate, target_clock_rate);
        frame.rtp_timestamp = rtp_timestamp;
        frame.sequence_number = Some(output_sequence);
        frame.payload_type = Some(target_payload_type);
        frame.clock_rate = target_clock_rate;
    }

    fn new_timestamp(
        &mut self,
        frame: &AudioFrame,
        source_clock_rate: u32,
        target_clock_rate: u32,
    ) -> (u32, u16) {
        if self.first_input_sequence.is_none() {
            self.first_input_sequence = frame.sequence_number;
        }

        let domain = self.domain.get_or_insert(TimestampDomain {
            first_input_timestamp: frame.rtp_timestamp,
            first_output_timestamp: self.first_output_timestamp,
        });

        let input_ts_delta = frame
            .rtp_timestamp
            .wrapping_sub(domain.first_input_timestamp);
        let output_ts_delta =
            (input_ts_delta as u64 * target_clock_rate as u64 / source_clock_rate as u64) as u32;
        let output_timestamp = domain.first_output_timestamp.wrapping_add(output_ts_delta);

        let first_input_seq = self.first_input_sequence.unwrap_or_default();
        let input_seq = frame.sequence_number.unwrap_or_default();
        let seq_delta = input_seq.wrapping_sub(first_input_seq);
        let output_sequence = self.first_output_sequence.wrapping_add(seq_delta);

        (output_timestamp, output_sequence)
    }
}

/// Rewrite the duration field inside a telephone-event (RFC 4733) payload.
/// Duration is bytes [2..4] in network byte order, expressed in RTP clock ticks.
pub fn rewrite_dtmf_duration(data: &[u8], source_rate: u32, target_rate: u32) -> bytes::Bytes {
    if data.len() < 4 || source_rate == target_rate {
        return bytes::Bytes::copy_from_slice(data);
    }
    let mut buf = data.to_vec();
    let duration = u16::from_be_bytes([buf[2], buf[3]]);
    let scaled = (duration as u32 * target_rate / source_rate) as u16;
    buf[2..4].copy_from_slice(&scaled.to_be_bytes());
    bytes::Bytes::from(buf)
}

pub struct Transcoder {
    decoder: Box<dyn Decoder>,
    encoder: Box<dyn Encoder>,
    source: CodecType,
    target: CodecType,
    /// The actual negotiated PT for the target codec (from SDP answer, not codec default)
    target_pt: u8,
    resampler: Option<VoiceResampler>,
    /// Resampled-PCM accumulator. The resampler does not emit an exact sample
    /// count per call (8k→48k yields 960 or 961 depending on its fractional
    /// position accumulator), but block codecs like Opus require an EXACT
    /// frame size (e.g. 960 samples = 20 ms @ 48 kHz). Encoding a 961-sample
    /// buffer makes `opus_encode` fail → empty payload → the receiver's
    /// decoder is EOF-poisoned and goes deaf. So we buffer decoded+resampled
    /// PCM here and only ever encode exact `samples_per_frame` chunks.
    pcm_accum: Vec<audio_codec::Sample>,
    /// Exact samples per encoded frame at the target rate = 20 ms (rate/50).
    samples_per_frame: usize,
}

impl Transcoder {
    pub fn new(source: CodecType, target: CodecType, target_pt: u8) -> Self {
        let decoder = create_decoder(source);
        let encoder = create_encoder(target);

        let source_sample_rate = decoder.sample_rate();
        let target_sample_rate = encoder.sample_rate();
        let resampler = if source_sample_rate != target_sample_rate {
            tracing::info!(
                ?source,
                ?target,
                source_sample_rate,
                target_sample_rate,
                "transcoder: VoiceResampler active"
            );
            Some(VoiceResampler::new(
                source_sample_rate as usize,
                target_sample_rate as usize,
            ))
        } else {
            tracing::info!(
                ?source,
                ?target,
                sample_rate = source_sample_rate,
                "transcoder: same-rate transcode, no resampler"
            );
            None
        };
        // 20 ms of mono PCM at the encoder's sample rate — the canonical
        // packet size and a valid Opus frame size (960 @ 48k, 160 @ 8k).
        let samples_per_frame = (target_sample_rate / 50).max(1) as usize;

        Self {
            decoder,
            encoder,
            source,
            target,
            target_pt,
            resampler,
            pcm_accum: Vec::new(),
            samples_per_frame,
        }
    }

    pub fn source_clock_rate(&self) -> u32 {
        self.source.clock_rate()
    }

    pub fn target_clock_rate(&self) -> u32 {
        self.target.clock_rate()
    }

    pub fn target_pt(&self) -> u8 {
        self.target_pt
    }

    /// Decode → resample → re-encode, emitting only complete, exactly-sized
    /// frames. Returns 0..N `AudioFrame`s: 0 when there isn't yet a full
    /// frame's worth of PCM buffered (the remainder is carried to the next
    /// call), or >1 when an input packet carries more than one frame of audio
    /// (e.g. a 30 ms-ptime carrier). Never emits an empty payload.
    pub fn transcode(&mut self, frame: &AudioFrame) -> Vec<AudioFrame> {
        let mut pcmbuf = self.decoder.decode(&frame.data);
        if let Some(resampler) = &mut self.resampler {
            pcmbuf = resampler.resample(&pcmbuf);
        }
        self.pcm_accum.extend_from_slice(&pcmbuf);

        // Each emitted frame advances the (source-domain) RTP timestamp by one
        // packet so that, on the rare multi-frame emit, downstream timestamp
        // rewriting doesn't see duplicate timestamps. For the common 1:1 case
        // index 0 leaves `frame.rtp_timestamp` unchanged.
        let input_ts_step = (self.source.clock_rate() / 50).max(1);

        let mut out = Vec::new();
        let mut idx: u32 = 0;
        while self.pcm_accum.len() >= self.samples_per_frame {
            let chunk: Vec<audio_codec::Sample> =
                self.pcm_accum.drain(..self.samples_per_frame).collect();
            let encoded_data = self.encoder.encode(&chunk);
            // A valid-size chunk should always encode; guard anyway so a codec
            // hiccup never forwards an empty (EOF-poisoning) payload.
            if encoded_data.is_empty() {
                continue;
            }
            out.push(AudioFrame {
                rtp_timestamp: frame.rtp_timestamp.wrapping_add(idx * input_ts_step),
                clock_rate: self.target.clock_rate(),
                data: encoded_data.into(),
                sequence_number: frame.sequence_number,
                payload_type: Some(self.target_pt),
                marker: frame.marker,
                header_extension: None,
                raw_packet: None,
                source_addr: frame.source_addr,
            });
            idx += 1;
        }
        out
    }
}

#[cfg(test)]
mod transcoder_tests {
    use super::*;
    use audio_codec::Resampler;

    /// Documents the ROOT CAUSE: the 8k→48k resampler does NOT emit an exact
    /// 960 samples per 20 ms input — it varies (≈960±1) because of its
    /// fractional position accumulator. This is why the transcoder must
    /// buffer to exact Opus frame sizes rather than encode each resample
    /// output directly.
    #[test]
    fn resampler_8k_to_48k_is_not_frame_exact() {
        let mut r = Resampler::new(8000, 48000);
        let input = vec![100i16; 160];
        let counts: Vec<usize> = (0..30).map(|_| r.resample(&input).len()).collect();
        assert!(
            counts.iter().any(|&n| n != 960),
            "expected at least one non-960 output (root cause); got {counts:?}"
        );
        assert!(
            counts.iter().all(|&n| (955..=965).contains(&n)),
            "outputs should hover around 960; got {counts:?}"
        );
    }

    fn pcma_frame(i: u16) -> AudioFrame {
        AudioFrame {
            rtp_timestamp: (i as u32) * 160,
            clock_rate: 8000,
            data: vec![0xD5u8; 160].into(), // PCMA silence, 160 samples = 20ms @ 8k
            sequence_number: Some(i),
            payload_type: Some(8),
            marker: false,
            ..Default::default()
        }
    }

    /// REGRESSION (the "deaf bot" bug): the PCMA→Opus transcoder must NEVER
    /// emit an empty Opus payload — not even on the first frame, where the
    /// resampler warms up to 961 samples. An empty payload EOF-poisons
    /// aiortc's decoder for the rest of the call.
    #[test]
    fn pcma_to_opus_never_emits_empty_frame() {
        let mut tx = Transcoder::new(CodecType::PCMA, CodecType::Opus, 111);
        let mut total = 0usize;
        for i in 0..100 {
            for f in tx.transcode(&pcma_frame(i)) {
                assert!(!f.data.is_empty(), "frame {i} produced an empty Opus payload");
                assert_eq!(f.payload_type, Some(111));
                assert_eq!(f.clock_rate, 48000);
                total += 1;
            }
        }
        // ~1 output frame per 20 ms input (buffering carries the ≤1-sample
        // remainder); allow a small slack for warmup.
        assert!(
            (98..=100).contains(&total),
            "expected ≈1 frame per input, got {total}"
        );
    }

    /// Matching same-rate codecs (PCMU→PCMA, both 8k, no resampler) still pass
    /// through cleanly: exactly one 160-sample frame per input, never empty.
    #[test]
    fn pcma_to_pcmu_one_to_one_no_empty() {
        let mut tx = Transcoder::new(CodecType::PCMA, CodecType::PCMU, 0);
        for i in 0..10 {
            let out = tx.transcode(&pcma_frame(i));
            assert_eq!(out.len(), 1, "same-rate transcode should be 1:1");
            assert!(!out[0].data.is_empty());
            assert_eq!(out[0].payload_type, Some(0));
        }
    }
}
