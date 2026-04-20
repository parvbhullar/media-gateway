//! Unified command payload — source of truth for HTTP-facing commands.
//!
//! Phase 4, Plan 04-01. Relocated from `src/console/handlers/call_control.rs`
//! per D-10. Extended with 8 new API variants per D-11 that plans 04-02..04-05
//! consume. `Leg`, `PlaySource`, and `ApiPlayOptions` are defined here so the
//! whole phase-4 wire surface lives in one module.
//!
//! ## Compat
//!
//! `src/console/handlers/call_control.rs` re-exports `CallCommandPayload` from
//! this module so callers that originally imported
//! `crate::console::handlers::call_control::CallCommandPayload` keep compiling
//! without churn. Prefer importing from `crate::call::runtime` going forward.

use serde::{Deserialize, Serialize};

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CallCommandPayload {
    // ── Legacy console variants (preserve wire compat with console) ────────
    Hangup {
        reason: Option<String>,
        code: Option<u16>,
        initiator: Option<String>,
    },
    #[serde(alias = "accept")]
    Accept {
        callee: Option<String>,
        sdp: Option<String>,
    },
    /// Console's single-arg blind transfer (no leg targeting). Preserved.
    Transfer {
        target: String,
    },
    /// Console's raw `track_id` mute. Preserved for internal console use.
    Mute {
        track_id: String,
    },
    /// Console's raw `track_id` unmute. Preserved for internal console use.
    Unmute {
        track_id: String,
    },

    // ── New api_v1 variants (consumed by plans 04-02..04-05) ───────────────
    /// Explicit blind transfer with optional leg targeting (default = callee).
    BlindTransfer {
        target: String,
        #[serde(default)]
        leg: Option<Leg>,
    },
    /// Start an attended transfer; returns the consult leg id in the response.
    AttendedTransferStart {
        target: String,
        #[serde(default)]
        leg: Option<Leg>,
    },
    AttendedTransferComplete {
        consult_leg: String,
    },
    AttendedTransferCancel {
        consult_leg: String,
    },
    /// api_v1 mute: leg-based; handler resolves `leg → track_id` using
    /// `SipSession::{CALLER,CALLEE}_TRACK_ID`.
    ApiMute {
        leg: Leg,
    },
    ApiUnmute {
        leg: Leg,
    },
    Play {
        source: PlaySource,
        #[serde(default)]
        leg: Option<Leg>,
        #[serde(default)]
        options: Option<ApiPlayOptions>,
    },
    Speak {
        text: String,
        #[serde(default)]
        voice: Option<String>,
        #[serde(default)]
        leg: Option<Leg>,
    },
    Dtmf {
        digits: String,
        #[serde(default)]
        duration_ms: Option<u32>,
        #[serde(default)]
        inter_digit_ms: Option<u32>,
        #[serde(default)]
        leg: Option<Leg>,
    },
    Record {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        format: Option<String>,
        #[serde(default)]
        beep: Option<bool>,
        #[serde(default)]
        max_duration_secs: Option<u32>,
        #[serde(default)]
        transcribe: Option<bool>,
    },
    /// api_v1 hangup variant — no `initiator` field (defaults to "api" in adapter).
    ApiHangup {
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        code: Option<u16>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Leg {
    Caller,
    Callee,
}

/// Subset of `MediaSource` accepted by `/play` — file and URL only.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PlaySource {
    File { path: String },
    Url { url: String },
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ApiPlayOptions {
    #[serde(rename = "loop", default)]
    pub loop_playback: bool,
    #[serde(default)]
    pub interrupt_on_dtmf: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hangup_legacy_deserializes() {
        let json = r#"{"action":"hangup","reason":"by_caller","code":200}"#;
        let p: CallCommandPayload = serde_json::from_str(json).unwrap();
        assert!(matches!(p, CallCommandPayload::Hangup { .. }));
    }

    #[test]
    fn blind_transfer_with_leg_deserializes() {
        let json = r#"{"action":"blind_transfer","target":"sip:1001@x.com","leg":"callee"}"#;
        let p: CallCommandPayload = serde_json::from_str(json).unwrap();
        if let CallCommandPayload::BlindTransfer { target, leg } = p {
            assert_eq!(target, "sip:1001@x.com");
            assert_eq!(leg, Some(Leg::Callee));
        } else {
            panic!("expected BlindTransfer");
        }
    }

    #[test]
    fn play_file_source_deserializes() {
        let json = r#"{"action":"play","source":{"type":"file","path":"/tmp/x.wav"}}"#;
        let p: CallCommandPayload = serde_json::from_str(json).unwrap();
        assert!(matches!(p, CallCommandPayload::Play { .. }));
    }

    #[test]
    fn play_url_source_deserializes() {
        let json = r#"{"action":"play","source":{"type":"url","url":"https://x/a.wav"}}"#;
        let p: CallCommandPayload = serde_json::from_str(json).unwrap();
        assert!(matches!(p, CallCommandPayload::Play { .. }));
    }

    #[test]
    fn leg_roundtrips_lowercase() {
        assert_eq!(serde_json::to_string(&Leg::Caller).unwrap(), r#""caller""#);
        assert_eq!(serde_json::to_string(&Leg::Callee).unwrap(), r#""callee""#);
    }
}
