//! Console/HTTP Command Adapter
//!
//! Converts `CallCommandPayload` (from HTTP API) to unified `CallCommand`.

use crate::call::domain::*;
use crate::callrecord::CallRecordHangupReason;
use crate::console::handlers::call_control::{CallCommandPayload, Leg, PlaySource};
use anyhow::Result;

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

/// Map the simple caller/callee `Leg` enum to a `LegId` using the session id as base.
fn leg_to_leg_id(leg: Leg, session_id: &str) -> LegId {
    match leg {
        Leg::Caller => LegId::new(format!("{}-caller", session_id)),
        Leg::Callee => LegId::new(format!("{}-callee", session_id)),
    }
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
            let mut cmd =
                HangupCommand::local(initiator.as_deref().unwrap_or("console"), cdr_reason, code);
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

        // ── Extended API variants ────────────────────────────────────────────

        CallCommandPayload::ApiHangup { reason, code } => {
            let cdr_reason = parse_hangup_reason(reason.as_deref());
            let mut cmd = HangupCommand::local("api", cdr_reason, code);
            cmd = cmd.with_cascade(HangupCascade::All);
            Ok(CallCommand::Hangup(cmd))
        }

        CallCommandPayload::ApiMute { leg } => {
            let leg_id = leg_to_leg_id(leg, session_id);
            Ok(CallCommand::MuteTrack {
                track_id: leg_id.into(),
            })
        }

        CallCommandPayload::ApiUnmute { leg } => {
            let leg_id = leg_to_leg_id(leg, session_id);
            Ok(CallCommand::UnmuteTrack {
                track_id: leg_id.into(),
            })
        }

        CallCommandPayload::BlindTransfer { target, leg } => {
            let leg_id = leg
                .map(|l| leg_to_leg_id(l, session_id))
                .unwrap_or_else(|| LegId::new(session_id));
            Ok(CallCommand::Transfer {
                leg_id,
                target,
                attended: false,
            })
        }

        CallCommandPayload::AttendedTransferStart { target, leg } => {
            let leg_id = leg
                .map(|l| leg_to_leg_id(l, session_id))
                .unwrap_or_else(|| LegId::new(session_id));
            Ok(CallCommand::Transfer {
                leg_id,
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

        CallCommandPayload::Play {
            source,
            leg,
            options,
        } => {
            let media_source = match source {
                PlaySource::File { path } => MediaSource::File { path },
                PlaySource::Url { url } => MediaSource::Url { url },
                PlaySource::Tts { text, voice } => MediaSource::Tts { text, voice },
            };
            let leg_id = leg.map(|l| leg_to_leg_id(l, session_id));
            let play_opts = options.map(|o| PlayOptions {
                loop_playback: o.loop_playback,
                interrupt_on_dtmf: o.interrupt_on_dtmf,
                ..PlayOptions::default()
            });
            Ok(CallCommand::Play {
                leg_id,
                source: media_source,
                options: play_opts,
            })
        }

        CallCommandPayload::Dtmf {
            digits,
            leg,
            duration_ms: _,
            inter_digit_ms: _,
        } => {
            let leg_id = leg
                .map(|l| leg_to_leg_id(l, session_id))
                .unwrap_or_else(|| LegId::new(session_id));
            Ok(CallCommand::SendDtmf { leg_id, digits })
        }

        CallCommandPayload::Record {
            path,
            format,
            beep,
            max_duration_secs,
            transcribe: _,
        } => {
            let resolved_path = path.unwrap_or_else(|| format!("/tmp/{}.wav", session_id));
            Ok(CallCommand::StartRecording {
                config: RecordConfig {
                    path: resolved_path,
                    max_duration_secs,
                    beep: beep.unwrap_or(false),
                    format,
                },
            })
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
}
