//! High-quality streaming PCM resampler for the media hot paths.
//!
//! Wraps rubato's async sinc resampler behind the exact same call shape as
//! `audio_codec::Resampler` (`resample(&[i16]) -> Vec<i16>`, mono) so it is
//! a drop-in replacement at every call site. Input is accumulated to fixed
//! 10 ms chunks internally; for the integer ratios used in telephony
//! (8k/16k/24k/48k) output is deterministic (exactly ratio × chunk per
//! chunk).
//!
//! ponytail: fixed ratio, no drift correction — both ends of every resample
//! here are RTP-clocked by this process. If long-call buffer creep ever
//! appears, wire `set_resample_ratio_relative` (rubato supports it).

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler as _, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

pub struct VoiceResampler {
    backend: Backend,
}

enum Backend {
    Passthrough,
    Rubato {
        rs: Async<f32>,
        chunk: usize,          // input frames per rubato call (10 ms)
        in_accum: Vec<f32>,
        out_scratch: Vec<f32>, // output_frames_max, reused every call
    },
    /// Legacy polyphase, kept as a never-crash fallback if rubato
    /// construction fails (invalid params — should never happen).
    Fallback(audio_codec::Resampler),
}

impl VoiceResampler {
    pub fn new(input_rate: usize, output_rate: usize) -> Self {
        if input_rate == output_rate {
            return Self { backend: Backend::Passthrough };
        }
        // Voice-tuned sinc profile (see design doc): lean real-time settings.
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            oversampling_factor: 64,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
        };
        // 10 ms of input per chunk divides every telephony ptime (10/20/30/40 ms)
        // so steady-state packet flows add zero accumulation latency.
        let chunk = (input_rate / 100).max(1);
        match Async::<f32>::new_sinc(
            output_rate as f64 / input_rate as f64,
            1.01, // headroom so drift correction can be enabled later without re-alloc
            &params,
            chunk,
            1,
            FixedAsync::Input,
        ) {
            Ok(rs) => {
                let out_cap = rs.output_frames_max();
                Self {
                    backend: Backend::Rubato {
                        rs,
                        chunk,
                        in_accum: Vec::with_capacity(chunk * 4),
                        out_scratch: vec![0.0; out_cap],
                    },
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, input_rate, output_rate,
                    "rubato resampler construction failed; using legacy polyphase");
                Self {
                    backend: Backend::Fallback(audio_codec::Resampler::new(
                        input_rate,
                        output_rate,
                    )),
                }
            }
        }
    }

    pub fn resample(&mut self, input: &[i16]) -> Vec<i16> {
        match &mut self.backend {
            Backend::Passthrough => input.to_vec(),
            Backend::Fallback(r) => r.resample(input),
            Backend::Rubato { rs, chunk, in_accum, out_scratch } => {
                in_accum.extend(input.iter().map(|&s| f32::from(s) / 32768.0));
                let per_chunk_out = out_scratch.len();
                let mut out: Vec<i16> =
                    Vec::with_capacity((in_accum.len() / *chunk) * per_chunk_out);
                let mut consumed = 0usize;
                while in_accum.len() - consumed >= *chunk {
                    let in_adapter = InterleavedSlice::new(
                        &in_accum[consumed..consumed + *chunk],
                        1,
                        *chunk,
                    )
                    .expect("mono input adapter");
                    let cap = out_scratch.len();
                    let mut out_adapter =
                        InterleavedSlice::new_mut(&mut out_scratch[..], 1, cap)
                            .expect("mono output adapter");
                    match rs.process_into_buffer(&in_adapter, &mut out_adapter, None) {
                        Ok((_read, written)) => {
                            out.extend(out_scratch[..written].iter().map(|&f| {
                                (f * 32768.0).clamp(i16::MIN as f32, i16::MAX as f32)
                                    as i16
                            }));
                        }
                        Err(e) => {
                            // Buffer-size bug would be caught by tests; never
                            // kill the call over one chunk.
                            tracing::warn!(error = %e, "rubato process error; chunk dropped");
                        }
                    }
                    consumed += *chunk;
                }
                in_accum.drain(..consumed);
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_is_passthrough() {
        let mut r = VoiceResampler::new(8000, 8000);
        let input = vec![100i16; 160];
        assert_eq!(r.resample(&input), input);
    }

    /// The fixed-chunk rubato backend's output contract, unlike the legacy
    /// polyphase (960±1 forever): the FIRST call is short by a small
    /// constant startup trim (rubato swallows the interpolator's leading
    /// transient instead of emitting padding silence), and every call
    /// after that is EXACTLY ratio×input for integer ratios.
    ///
    /// Helper: run `calls` fixed-size inputs, assert steady-state exactness
    /// and return the observed first-call trim (in output samples).
    fn assert_steady_exact(
        in_rate: usize,
        out_rate: usize,
        in_frames: usize,
        expect_out: usize,
        calls: usize,
    ) -> usize {
        let mut r = VoiceResampler::new(in_rate, out_rate);
        let lens: Vec<usize> = (0..calls)
            .map(|_| r.resample(&vec![1000i16; in_frames]).len())
            .collect();
        for (i, &l) in lens.iter().enumerate().skip(1) {
            assert_eq!(l, expect_out, "call {i} not frame-exact: {lens:?}");
        }
        assert!(lens[0] <= expect_out, "first call over-emitted: {lens:?}");
        expect_out - lens[0]
    }

    #[test]
    fn upsample_8k_to_48k_is_frame_exact() {
        let trim = assert_steady_exact(8000, 48000, 160, 960, 50);
        assert!(trim < 48, "startup trim should be ≪ 1 ms: {trim}");
    }

    /// 24 kHz is the primary AI-consumer target rate.
    #[test]
    fn upsample_8k_to_24k_is_frame_exact() {
        let trim = assert_steady_exact(8000, 24000, 160, 480, 50);
        assert!(trim < 24, "startup trim should be ≪ 1 ms: {trim}");
    }

    #[test]
    fn downsample_48k_to_8k_is_frame_exact() {
        let trim = assert_steady_exact(48000, 8000, 960, 160, 50);
        assert!(trim < 8, "startup trim should be ≪ 1 ms: {trim}");
    }

    /// 48k↔24k conversions occur when a 48 kHz consumer feeds a 24 kHz one
    /// (or vice versa) — both directions must be frame-exact.
    #[test]
    fn cross_rate_24k_48k_is_frame_exact() {
        assert_steady_exact(24000, 48000, 480, 960, 50);
        assert_steady_exact(48000, 24000, 960, 480, 50);
    }

    /// Odd-sized input accumulates: 10 ms chunks internally, remainder carried.
    #[test]
    fn non_chunk_multiple_input_conserves_samples() {
        let mut r = VoiceResampler::new(8000, 16000);
        let mut total_in = 0usize;
        let mut total_out = 0usize;
        for n in [37usize, 80, 123, 160, 240, 7] {
            total_in += n;
            total_out += r.resample(&vec![0i16; n]).len();
        }
        // Everything except the sub-10 ms input remainder must have been
        // emitted, minus the one-time startup trim (< 1 ms of output).
        let remainder = total_in % 80; // 80 = 10 ms @ 8 kHz input chunk
        let expected = (total_in - remainder) * 2;
        assert!(total_out <= expected, "over-emitted: {total_out} > {expected}");
        assert!(
            expected - total_out < 16,
            "lost more than the startup trim: {total_out} vs {expected}"
        );
    }

    #[test]
    fn empty_input_empty_output() {
        let mut r = VoiceResampler::new(8000, 48000);
        assert!(r.resample(&[]).is_empty());
    }
}
