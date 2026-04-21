//! Console/HTTP Command Adapter
//!
//! Converts `CallCommandPayload` (from HTTP API) to unified `CallCommand`.

use crate::call::domain::*;
use crate::call::runtime::command_payload::{ApiPlayOptions, CallCommandPayload, Leg, PlaySource};
use crate::callrecord::CallRecordHangupReason;
use crate::proxy::proxy_call::sip_session::SipSession;
use anyhow::Result;

/// Resolve the `leg` hint to a `LegId` suitable for the session layer.
///
/// Per D-21, Phase 4 keeps `LegId == session_id` so the SIP session-layer's
/// existing leg-selection logic remains the authority. The leg hint is
/// accepted on the wire for forward-compat and logged by the handler for
/// the response (no-op here at the adapter layer today).
fn leg_to_leg_id(_leg: Option<Leg>, session_id: &str) -> LegId {
    LegId::new(session_id)
}

/// Convert `ApiPlayOptions` (wire) to domain `PlayOptions`.
///
/// Missing options → `None` (session layer uses its own defaults).
fn api_play_options_to_domain(options: Option<ApiPlayOptions>) -> Option<PlayOptions> {
    options.map(|o| PlayOptions {
        loop_playback: o.loop_playback,
        await_completion: false,
        interrupt_on_dtmf: o.interrupt_on_dtmf,
        track_id: None,
        send_progress: false,
    })
}

/// Convert hangup reason string to CallRecordHangupReason
fn parse_hangup_reason(reason: Option<&str>) -> Option<CallRecordHangupReason> {
    reason.and_then(|r| match r.to_lowercase().as_str() {
        "by_caller" | "caller" => Some(CallRecordHangupReason::ByCaller),
        "by_callee" | "callee" => Some(CallRecordHangupReason::ByCallee),
        "by_system" | "system" => Some(CallRecordHangupReason::BySystem),
        "no_answer" => Some(CallRecordHangupReason::NoAnswer),
        "rejected" => Some(CallRecordHangupReason::Rejected),
        "canceled" => Some(CallRecordHangupReason::Canceled),
        "failed" => Some(CallRecordHangupReason::Failed),
        _ => None,
    })
}

/// Convert Console CallCommandPayload to unified CallCommand
///
/// # Arguments
/// * `payload` - The console command payload
/// * `session_id` - The session ID context
///
/// # Returns
/// * `Ok(CallCommand)` - Successfully converted command
/// * `Err` - Conversion failed
pub fn console_to_call_command(
    payload: CallCommandPayload,
    session_id: &str,
) -> Result<CallCommand> {
    match payload {
        CallCommandPayload::Hangup {
            reason,
            code,
            initiator,
        } => {
            let cdr_reason = parse_hangup_reason(reason.as_deref());
            let mut cmd = HangupCommand::local(
                initiator.as_deref().unwrap_or("console"),
                cdr_reason,
                code,
            );
            cmd = cmd.with_cascade(HangupCascade::All);
            Ok(CallCommand::Hangup(cmd))
        }

        CallCommandPayload::Accept { callee, sdp } => {
            // Accept is similar to Answer but with additional context
            // For now, we map it to Answer with the leg being the session itself
            // The callee and sdp fields are used internally by the session
            let _ = (callee, sdp); // Acknowledge but don't use for now
            Ok(CallCommand::Answer {
                leg_id: LegId::new(session_id),
            })
        }

        CallCommandPayload::Transfer { target } => Ok(CallCommand::Transfer {
            leg_id: LegId::new(session_id),
            target,
            attended: false,
        }),

        CallCommandPayload::Mute { track_id } => Ok(CallCommand::MuteTrack { track_id }),

        CallCommandPayload::Unmute { track_id } => Ok(CallCommand::UnmuteTrack { track_id }),

        // ── Phase 4 API variants — filled in by later plans ─────────────
        CallCommandPayload::BlindTransfer { target, leg } => {
            // `leg` accepted for forward-compat per D-21 (default=callee);
            // SIP session-layer picks the leg from its dialog state today.
            let _ = leg;
            Ok(CallCommand::Transfer {
                leg_id: LegId::new(session_id),
                target,
                attended: false,
            })
        }
        CallCommandPayload::AttendedTransferStart { target, leg } => {
            // `leg` accepted for forward-compat per D-21 (default=callee);
            // SIP session-layer picks the leg from its dialog state today.
            let _ = leg;
            Ok(CallCommand::Transfer {
                leg_id: LegId::new(session_id),
                target,
                attended: true,
            })
        }
        CallCommandPayload::AttendedTransferComplete { consult_leg } => {
            Ok(CallCommand::TransferComplete {
                consult_leg: LegId::new(consult_leg),
            })
        }
        CallCommandPayload::AttendedTransferCancel { consult_leg } => {
            Ok(CallCommand::TransferCancel {
                consult_leg: LegId::new(consult_leg),
            })
        }
        CallCommandPayload::ApiMute { leg } => {
            let track_id = match leg {
                Leg::Caller => SipSession::CALLER_TRACK_ID.to_string(),
                Leg::Callee => SipSession::CALLEE_TRACK_ID.to_string(),
            };
            Ok(CallCommand::MuteTrack { track_id })
        }
        CallCommandPayload::ApiUnmute { leg } => {
            let track_id = match leg {
                Leg::Caller => SipSession::CALLER_TRACK_ID.to_string(),
                Leg::Callee => SipSession::CALLEE_TRACK_ID.to_string(),
            };
            Ok(CallCommand::UnmuteTrack { track_id })
        }
        CallCommandPayload::Play {
            source,
            leg,
            options,
        } => {
            // The api_v1 /play handler rejects `PlaySource::Url` variants
            // BEFORE calling this adapter (pre-dispatch probe per RESEARCH §6).
            // We still emit a valid `CallCommand::Play{source:Url{..}}` here
            // so other call paths (e.g., console) can surface the same
            // `handle_play` rejection message from the session layer if they
            // ever call through this adapter.
            let media_source = match source {
                PlaySource::File { path } => MediaSource::File { path },
                PlaySource::Url { url } => MediaSource::Url { url },
            };
            Ok(CallCommand::Play {
                leg_id: Some(leg_to_leg_id(leg, session_id)),
                source: media_source,
                options: api_play_options_to_domain(options),
            })
        }
        CallCommandPayload::Speak { text, voice, leg } => {
            // Phase 4 never reaches this arm through the api_v1 `/speak`
            // handler (which always 400s pre-dispatch per D-13). Kept here
            // for forward-compat so a later phase that wires TTS can use
            // this adapter path unchanged.
            Ok(CallCommand::Play {
                leg_id: Some(leg_to_leg_id(leg, session_id)),
                source: MediaSource::Tts { text, voice },
                options: None,
            })
        }
        CallCommandPayload::Dtmf {
            digits,
            duration_ms,
            inter_digit_ms,
            leg,
        } => Ok(CallCommand::SendDtmf {
            leg_id: leg_to_leg_id(leg, session_id),
            digits,
            duration_ms,
            inter_digit_ms,
        }),
        CallCommandPayload::Record { .. } => {
            Err(anyhow::anyhow!("Record not yet wired; see plan 04-05"))
        }
        CallCommandPayload::ApiHangup { reason, code } => {
            let cdr_reason = parse_hangup_reason(reason.as_deref());
            let mut cmd = HangupCommand::local("api", cdr_reason, code);
            cmd = cmd.with_cascade(HangupCascade::All);
            Ok(CallCommand::Hangup(cmd))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hangup_conversion() {
        let payload = CallCommandPayload::Hangup {
            reason: Some("normal_clearing".to_string()),
            code: Some(200),
            initiator: Some("admin".to_string()),
        };
        let cmd = console_to_call_command(payload, "session-123").unwrap();
        if let CallCommand::Hangup(hangup_cmd) = cmd {
            assert_eq!(hangup_cmd.code, Some(200));
        } else {
            panic!("Expected Hangup command");
        }
    }

    #[test]
    fn test_transfer_conversion() {
        let payload = CallCommandPayload::Transfer {
            target: "sip:1001@example.com".to_string(),
        };
        let cmd = console_to_call_command(payload, "session-123").unwrap();
        if let CallCommand::Transfer {
            leg_id,
            target,
            attended,
        } = cmd
        {
            assert_eq!(leg_id.as_str(), "session-123");
            assert_eq!(target, "sip:1001@example.com");
            assert!(!attended);
        } else {
            panic!("Expected Transfer command");
        }
    }

    #[test]
    fn test_mute_conversion() {
        let payload = CallCommandPayload::Mute {
            track_id: "track-audio-1".to_string(),
        };
        let cmd = console_to_call_command(payload, "session-123").unwrap();
        if let CallCommand::MuteTrack { track_id } = cmd {
            assert_eq!(track_id, "track-audio-1");
        } else {
            panic!("Expected MuteTrack command");
        }
    }

    // ── Phase 4 Plan 04-02 — ApiMute / ApiUnmute / ApiHangup ─────────────

    #[test]
    fn test_api_mute_caller_converts_to_caller_track() {
        let payload = CallCommandPayload::ApiMute { leg: Leg::Caller };
        let cmd = console_to_call_command(payload, "sess-x").unwrap();
        if let CallCommand::MuteTrack { track_id } = cmd {
            assert_eq!(track_id, "caller-track");
        } else {
            panic!("expected MuteTrack");
        }
    }

    #[test]
    fn test_api_mute_callee_converts_to_callee_track() {
        let payload = CallCommandPayload::ApiMute { leg: Leg::Callee };
        let cmd = console_to_call_command(payload, "sess-x").unwrap();
        if let CallCommand::MuteTrack { track_id } = cmd {
            assert_eq!(track_id, "callee-track");
        } else {
            panic!("expected MuteTrack");
        }
    }

    #[test]
    fn test_api_unmute_caller_converts() {
        let payload = CallCommandPayload::ApiUnmute { leg: Leg::Caller };
        let cmd = console_to_call_command(payload, "sess-x").unwrap();
        assert!(matches!(
            cmd,
            CallCommand::UnmuteTrack { track_id } if track_id == "caller-track"
        ));
    }

    #[test]
    fn test_api_unmute_callee_converts() {
        let payload = CallCommandPayload::ApiUnmute { leg: Leg::Callee };
        let cmd = console_to_call_command(payload, "sess-x").unwrap();
        assert!(matches!(
            cmd,
            CallCommand::UnmuteTrack { track_id } if track_id == "callee-track"
        ));
    }

    #[test]
    fn test_api_hangup_converts_with_api_initiator() {
        let payload = CallCommandPayload::ApiHangup {
            reason: Some("by_caller".to_string()),
            code: Some(200),
        };
        let cmd = console_to_call_command(payload, "sess-x").unwrap();
        if let CallCommand::Hangup(hc) = cmd {
            assert_eq!(hc.code, Some(200));
            assert!(matches!(hc.cascade, HangupCascade::All));
        } else {
            panic!("expected Hangup");
        }
    }

    #[test]
    fn test_api_hangup_without_reason_or_code() {
        let payload = CallCommandPayload::ApiHangup {
            reason: None,
            code: None,
        };
        let cmd = console_to_call_command(payload, "sess-x").unwrap();
        assert!(matches!(cmd, CallCommand::Hangup(_)));
    }

    // ── Phase 4 Plan 04-03 — Blind / Attended Transfer* ──────────────────

    #[test]
    fn test_blind_transfer_conversion() {
        let payload = CallCommandPayload::BlindTransfer {
            target: "sip:1001@example.com".to_string(),
            leg: Some(Leg::Callee),
        };
        let cmd = console_to_call_command(payload, "sess-xfer").unwrap();
        if let CallCommand::Transfer {
            leg_id,
            target,
            attended,
        } = cmd
        {
            assert_eq!(leg_id.as_str(), "sess-xfer");
            assert_eq!(target, "sip:1001@example.com");
            assert!(!attended);
        } else {
            panic!("expected Transfer");
        }
    }

    #[test]
    fn test_attended_transfer_start_conversion() {
        let payload = CallCommandPayload::AttendedTransferStart {
            target: "sip:1001@example.com".to_string(),
            leg: None,
        };
        let cmd = console_to_call_command(payload, "sess-att").unwrap();
        assert!(matches!(
            cmd,
            CallCommand::Transfer {
                attended: true,
                ..
            }
        ));
    }

    #[test]
    fn test_attended_transfer_complete_conversion() {
        let payload = CallCommandPayload::AttendedTransferComplete {
            consult_leg: "consult-123".to_string(),
        };
        let cmd = console_to_call_command(payload, "sess-att").unwrap();
        if let CallCommand::TransferComplete { consult_leg } = cmd {
            assert_eq!(consult_leg.as_str(), "consult-123");
        } else {
            panic!("expected TransferComplete");
        }
    }

    #[test]
    fn test_attended_transfer_cancel_conversion() {
        let payload = CallCommandPayload::AttendedTransferCancel {
            consult_leg: "consult-xyz".to_string(),
        };
        let cmd = console_to_call_command(payload, "sess-att").unwrap();
        if let CallCommand::TransferCancel { consult_leg } = cmd {
            assert_eq!(consult_leg.as_str(), "consult-xyz");
        } else {
            panic!("expected TransferCancel");
        }
    }

    // ── Phase 4 Plan 04-04 — Play / Speak / Dtmf ─────────────────────────

    #[test]
    fn test_play_file_conversion() {
        let payload = CallCommandPayload::Play {
            source: PlaySource::File {
                path: "/tmp/hold.wav".to_string(),
            },
            leg: Some(Leg::Callee),
            options: Some(ApiPlayOptions {
                loop_playback: true,
                interrupt_on_dtmf: false,
            }),
        };
        let cmd = console_to_call_command(payload, "sess-play").unwrap();
        if let CallCommand::Play {
            leg_id,
            source,
            options,
        } = cmd
        {
            assert_eq!(leg_id.as_ref().unwrap().as_str(), "sess-play");
            assert!(matches!(source, MediaSource::File { .. }));
            let opts = options.expect("options should pass through");
            assert!(opts.loop_playback);
            assert!(!opts.interrupt_on_dtmf);
        } else {
            panic!("expected Play");
        }
    }

    #[test]
    fn test_play_url_conversion_still_produces_valid_cmd() {
        // The adapter produces `CallCommand::Play{source:Url{..}}`; the
        // api_v1 `/play` handler rejects this variant BEFORE calling the
        // adapter in Phase 4. Unit test just verifies the adapter does
        // not panic and emits the structurally-correct command.
        let payload = CallCommandPayload::Play {
            source: PlaySource::Url {
                url: "https://x/a.wav".to_string(),
            },
            leg: None,
            options: None,
        };
        let cmd = console_to_call_command(payload, "sess").unwrap();
        if let CallCommand::Play { source, options, .. } = cmd {
            assert!(matches!(source, MediaSource::Url { .. }));
            assert!(options.is_none());
        } else {
            panic!("expected Play");
        }
    }

    #[test]
    fn test_speak_conversion_produces_tts_play() {
        let payload = CallCommandPayload::Speak {
            text: "hello".to_string(),
            voice: Some("en-US".to_string()),
            leg: None,
        };
        let cmd = console_to_call_command(payload, "sess-speak").unwrap();
        if let CallCommand::Play { source, .. } = cmd {
            if let MediaSource::Tts { text, voice } = source {
                assert_eq!(text, "hello");
                assert_eq!(voice.as_deref(), Some("en-US"));
            } else {
                panic!("expected Tts media source, got {:?}", source);
            }
        } else {
            panic!("expected Play with Tts");
        }
    }

    #[test]
    fn test_dtmf_with_timing_overrides_passes_through() {
        let payload = CallCommandPayload::Dtmf {
            digits: "123".to_string(),
            duration_ms: Some(200),
            inter_digit_ms: Some(100),
            leg: Some(Leg::Callee),
        };
        let cmd = console_to_call_command(payload, "sess-dtmf").unwrap();
        if let CallCommand::SendDtmf {
            leg_id,
            digits,
            duration_ms,
            inter_digit_ms,
        } = cmd
        {
            assert_eq!(leg_id.as_str(), "sess-dtmf");
            assert_eq!(digits, "123");
            assert_eq!(duration_ms, Some(200));
            assert_eq!(inter_digit_ms, Some(100));
        } else {
            panic!("expected SendDtmf");
        }
    }

    #[test]
    fn test_dtmf_without_timing_overrides() {
        let payload = CallCommandPayload::Dtmf {
            digits: "ABCD*#".to_string(),
            duration_ms: None,
            inter_digit_ms: None,
            leg: None,
        };
        let cmd = console_to_call_command(payload, "sess").unwrap();
        if let CallCommand::SendDtmf {
            digits,
            duration_ms,
            inter_digit_ms,
            ..
        } = cmd
        {
            assert_eq!(digits, "ABCD*#");
            assert!(duration_ms.is_none());
            assert!(inter_digit_ms.is_none());
        } else {
            panic!("expected SendDtmf");
        }
    }
}
