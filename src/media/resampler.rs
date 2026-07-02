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
        /// Mutex only to grant `Sync` (rubato's inner trait object is
        /// `Send`-only); `resample` takes `&mut self` so access goes via
        /// `get_mut()` — no runtime locking.
        rs: std::sync::Mutex<Async<f32>>,
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
                        rs: std::sync::Mutex::new(rs),
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
                let rs = rs.get_mut().unwrap_or_else(|e| e.into_inner());
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

    /// Goertzel power at `freq` (normalized, relative comparisons only).
    fn goertzel_power(samples: &[i16], sample_rate: f64, freq: f64) -> f64 {
        let n = samples.len() as f64;
        let k = (0.5 + n * freq / sample_rate).floor();
        let w = 2.0 * std::f64::consts::PI * k / n;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in samples {
            let s0 = f64::from(x) + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (n * n)
    }

    /// Sum of image powers relative to the signal, in dBc (negative = good),
    /// for a 1 kHz tone upsampled from 8 kHz to `out_rate`.
    fn imaging_dbc(out: &[i16], out_rate: f64) -> f64 {
        let steady = &out[out.len() / 2..]; // skip warmup
        let signal = goertzel_power(steady, out_rate, 1000.0);
        // Images of a 1 kHz tone sit at n*8000 ± 1000 Hz below Nyquist.
        let images: f64 = [7000.0, 9000.0, 15000.0, 17000.0, 23000.0]
            .iter()
            .filter(|&&f| f < out_rate / 2.0)
            .map(|&f| goertzel_power(steady, out_rate, f))
            .sum();
        10.0 * (images / signal).log10()
    }

    /// The whole point of the rubato upgrade: spectral images after
    /// upsampling must sit far below the legacy 16-tap polyphase's, and
    /// below -55 dBc absolutely. Covers both AI-consumer targets (24/48 k).
    #[test]
    fn upsample_image_suppression_beats_legacy() {
        let sine: Vec<i16> = (0..16000)
            .map(|i| {
                (10000.0
                    * (2.0 * std::f64::consts::PI * 1000.0 * f64::from(i) / 8000.0)
                        .sin()) as i16
            })
            .collect();

        for out_rate in [24000usize, 48000] {
            let new_db = {
                let mut r = VoiceResampler::new(8000, out_rate);
                imaging_dbc(&r.resample(&sine), out_rate as f64)
            };
            let old_db = {
                let mut r = audio_codec::Resampler::new(8000, out_rate);
                imaging_dbc(&r.resample(&sine), out_rate as f64)
            };
            println!("8k->{out_rate}: new {new_db:.1} dBc, legacy {old_db:.1} dBc");
            assert!(new_db < -55.0, "8k->{out_rate}: images too high: {new_db:.1} dBc");
            assert!(
                new_db < old_db,
                "8k->{out_rate}: new ({new_db:.1}) must beat legacy ({old_db:.1})"
            );
        }
    }
}
