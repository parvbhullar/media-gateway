//! Ingress jitter stage for bridge legs: packet-domain reorder + adaptive
//! delay BEFORE decode, so transcoders never see out-of-order input.
//!
//! Thin policy wrapper around `rustrtc::media::JitterBuffer`. Frames the
//! inner buffer cannot handle (no sequence number, video) bypass instead
//! of being silently dropped.

use rustrtc::media::{JitterBuffer, MediaSample};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Per-trunk jitter policy (wire shape shared by the trunk media API).
///
/// - `{"mode":"off"}` — disable even on transcoded legs (escape hatch).
/// - `{"mode":"adaptive","min_ms":20,"max_ms":120}` — enable on ALL inbound
///   legs from this trunk, passthrough included.
/// - Absent — default behavior: enabled (defaults) only while the leg is
///   transcoding.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum JitterBufferPolicy {
    Off,
    Adaptive {
        #[serde(default = "default_min_ms")]
        min_ms: u64,
        #[serde(default = "default_max_ms")]
        max_ms: u64,
    },
}

fn default_min_ms() -> u64 {
    20
}

fn default_max_ms() -> u64 {
    120
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JitterConfig {
    pub min_ms: u64,
    pub max_ms: u64,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self { min_ms: default_min_ms(), max_ms: default_max_ms() }
    }
}

pub struct JitterStage {
    jb: JitterBuffer,
    cfg: JitterConfig,
}

impl JitterStage {
    pub fn new(cfg: JitterConfig) -> Self {
        // Capacity: 4× the max-delay window at 20 ms packets — enough for
        // bursts without unbounded memory.
        let capacity = (cfg.max_ms as usize / 20).max(2) * 4;
        Self {
            jb: JitterBuffer::new(
                Duration::from_millis(cfg.min_ms),
                Duration::from_millis(cfg.max_ms),
                capacity,
            ),
            cfg,
        }
    }

    pub fn config(&self) -> JitterConfig {
        self.cfg
    }

    /// Buffer the sample, or hand it straight back when the jitter buffer
    /// cannot manage it (video, missing sequence number).
    pub fn push_or_bypass(&mut self, sample: MediaSample) -> Option<MediaSample> {
        match &sample {
            MediaSample::Audio(f) if f.sequence_number.is_some() => {
                self.jb.push(sample);
                None
            }
            _ => Some(sample),
        }
    }

    pub fn pop(&mut self) -> Option<MediaSample> {
        self.jb.pop()
    }

    pub fn next_wait(&self) -> Option<Duration> {
        self.jb.next_pop_wait()
    }

    pub fn is_empty(&self) -> bool {
        self.jb.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use rustrtc::media::{AudioFrame, MediaSample, VideoFrame};

    fn frame(seq: u16) -> MediaSample {
        MediaSample::Audio(AudioFrame {
            sequence_number: Some(seq),
            rtp_timestamp: u32::from(seq) * 160,
            payload_type: Some(0),
            clock_rate: 8000,
            data: Bytes::from(vec![0u8; 160]),
            ..Default::default()
        })
    }

    fn seq_of(s: &MediaSample) -> u16 {
        match s {
            MediaSample::Audio(f) => f.sequence_number.unwrap(),
            MediaSample::Video(f) => f.sequence_number.unwrap(),
        }
    }

    #[test]
    fn reorders_out_of_order_audio() {
        // min_delay 0 so pops are immediate once in order.
        let mut js = JitterStage::new(JitterConfig { min_ms: 0, max_ms: 100 });
        assert!(js.push_or_bypass(frame(2)).is_none());
        assert!(js.push_or_bypass(frame(1)).is_none());
        assert!(js.push_or_bypass(frame(3)).is_none());
        let popped: Vec<u16> =
            std::iter::from_fn(|| js.pop()).map(|s| seq_of(&s)).collect();
        assert_eq!(popped, vec![1, 2, 3]);
    }

    #[test]
    fn bypasses_frames_without_sequence_number() {
        let mut js = JitterStage::new(JitterConfig::default());
        let s = MediaSample::Audio(AudioFrame {
            sequence_number: None,
            data: Bytes::from(vec![0u8; 160]),
            ..Default::default()
        });
        // Must come straight back — the inner JB would silently drop it.
        assert!(js.push_or_bypass(s).is_some());
        assert!(js.is_empty());
    }

    #[test]
    fn bypasses_video() {
        let mut js = JitterStage::new(JitterConfig::default());
        let v = MediaSample::Video(VideoFrame {
            sequence_number: Some(1),
            ..Default::default()
        });
        assert!(js.push_or_bypass(v).is_some());
        assert!(js.is_empty());
    }

    #[test]
    fn next_wait_bounded_by_min_delay() {
        let mut js = JitterStage::new(JitterConfig { min_ms: 50, max_ms: 100 });
        assert!(js.push_or_bypass(frame(1)).is_none());
        assert!(js.pop().is_none()); // min_delay not yet elapsed
        let w = js.next_wait().expect("has buffered frame");
        assert!(w <= Duration::from_millis(50));
    }

    #[test]
    fn wire_shape_round_trips() {
        let adaptive: JitterBufferPolicy =
            serde_json::from_str(r#"{"mode":"adaptive","min_ms":30,"max_ms":200}"#)
                .unwrap();
        assert_eq!(
            adaptive,
            JitterBufferPolicy::Adaptive { min_ms: 30, max_ms: 200 }
        );
        let off: JitterBufferPolicy =
            serde_json::from_str(r#"{"mode":"off"}"#).unwrap();
        assert_eq!(off, JitterBufferPolicy::Off);
        // Defaults fill in when bounds are omitted.
        let bare: JitterBufferPolicy =
            serde_json::from_str(r#"{"mode":"adaptive"}"#).unwrap();
        assert_eq!(
            bare,
            JitterBufferPolicy::Adaptive { min_ms: 20, max_ms: 120 }
        );
    }
}
