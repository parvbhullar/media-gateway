//! Resample a telephony WAV through the production media pipeline.
//!
//! Reads a µ-law (G.711/PCMU) or 16-bit PCM WAV, decodes with the same
//! `audio_codec` decoder the gateway uses, streams each channel through
//! `VoiceResampler` in 20 ms frames (exactly like a live call leg), and
//! writes s16 PCM WAVs at the requested rates. Also writes a
//! legacy-resampler 48 kHz version for A/B listening.
//!
//! Usage:
//!   cargo run --example resample_wav -- <input.wav> [out_dir]

use audio_codec::{CodecType, create_decoder};
use rustpbx::media::resampler::VoiceResampler;

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("usage: resample_wav <input.wav> [out_dir]");
    let out_dir = args.next().unwrap_or_else(|| {
        std::path::Path::new(&input)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    });
    let stem = std::path::Path::new(&input)
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let bytes = std::fs::read(&input).expect("read input wav");
    let (fmt, channels, in_rate, data) = parse_wav(&bytes);
    println!(
        "input: fmt={fmt} ({}), {channels} ch, {in_rate} Hz, {} bytes of audio",
        if fmt == 7 { "µ-law/PCMU" } else { "PCM s16" },
        data.len()
    );

    // Decode to per-channel i16 PCM using the gateway's own codec path.
    let chans: Vec<Vec<i16>> = match fmt {
        7 => {
            // Interleaved µ-law, 1 byte/sample. Decode each channel with
            // the production PCMU decoder.
            (0..channels)
                .map(|c| {
                    let ulaw: Vec<u8> =
                        data.iter().skip(c).step_by(channels).copied().collect();
                    let mut dec = create_decoder(CodecType::PCMU);
                    dec.decode(&ulaw)
                })
                .collect()
        }
        1 => (0..channels)
            .map(|c| {
                data.chunks_exact(2)
                    .skip(c)
                    .step_by(channels)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect()
            })
            .collect(),
        other => panic!("unsupported WAV format tag {other} (need 1=PCM or 7=µ-law)"),
    };

    // Reference: the decoded input as s16 PCM at its native rate.
    write_wav(
        &format!("{out_dir}/{stem}_{in_rate}hz_decoded.wav"),
        &chans,
        in_rate,
    );

    for out_rate in [24_000u32, 48_000] {
        let out: Vec<Vec<i16>> = chans
            .iter()
            .map(|ch| stream_resample_hq(ch, in_rate as usize, out_rate as usize))
            .collect();
        write_wav(&format!("{out_dir}/{stem}_{out_rate}hz_hq.wav"), &out, out_rate);
    }

    // Legacy polyphase 48 kHz for A/B listening.
    let legacy: Vec<Vec<i16>> = chans
        .iter()
        .map(|ch| {
            let mut r = audio_codec::Resampler::new(in_rate as usize, 48_000);
            let frame = in_rate as usize / 50; // 20 ms
            let mut out = Vec::new();
            for chunk in ch.chunks(frame) {
                out.extend(r.resample(chunk));
            }
            out
        })
        .collect();
    write_wav(&format!("{out_dir}/{stem}_48000hz_legacy.wav"), &legacy, 48_000);
}

/// Stream through VoiceResampler in 20 ms frames, like a live call leg.
fn stream_resample_hq(pcm: &[i16], in_rate: usize, out_rate: usize) -> Vec<i16> {
    let mut r = VoiceResampler::new(in_rate, out_rate);
    let frame = in_rate / 50; // 20 ms
    let mut out = Vec::new();
    for chunk in pcm.chunks(frame) {
        out.extend(r.resample(chunk));
    }
    out
}

/// Minimal RIFF/WAVE parser: returns (format_tag, channels, sample_rate, data).
fn parse_wav(bytes: &[u8]) -> (u16, usize, u32, &[u8]) {
    assert_eq!(&bytes[0..4], b"RIFF", "not a RIFF file");
    assert_eq!(&bytes[8..12], b"WAVE", "not a WAVE file");
    let (mut fmt_tag, mut channels, mut rate) = (0u16, 0usize, 0u32);
    let mut data: &[u8] = &[];
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8 + size).min(bytes.len())];
        match id {
            b"fmt " => {
                fmt_tag = u16::from_le_bytes(body[0..2].try_into().unwrap());
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap()) as usize;
                rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            }
            b"data" => data = body,
            _ => {}
        }
        pos += 8 + size + (size & 1); // chunks are word-aligned
    }
    assert!(!data.is_empty(), "no data chunk found");
    (fmt_tag, channels, rate, data)
}

/// Write interleaved s16 PCM WAV from per-channel buffers.
fn write_wav(path: &str, chans: &[Vec<i16>], rate: u32) {
    let spec = hound::WavSpec {
        channels: chans.len() as u16,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).expect("create wav");
    let frames = chans.iter().map(|c| c.len()).min().unwrap_or(0);
    for i in 0..frames {
        for ch in chans {
            w.write_sample(ch[i]).expect("write sample");
        }
    }
    w.finalize().expect("finalize wav");
    println!(
        "wrote {path} ({} ch, {rate} Hz, {:.1}s)",
        chans.len(),
        frames as f64 / rate as f64
    );
}
