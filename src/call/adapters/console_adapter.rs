//! Console/HTTP Command Adapter
//!
//! Converts `CallCommandPayload` (from HTTP API) to unified `CallCommand`.

use crate::call::domain::*;
use crate::call::runtime::command_payload::{CallCommandPayload, Leg};
use crate::callrecord::CallRecordHangupReason;
use crate::proxy::proxy_call::sip_session::SipSession;
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
        CallCommandPayload::BlindTransfer { .. } => {
            Err(anyhow::anyhow!("BlindTransfer not yet wired; see plan 04-03"))
        }
        CallCommandPayload::AttendedTransferStart { .. } => {
            Err(anyhow::anyhow!("AttendedTransferStart not yet wired; see plan 04-03"))
        }
        CallCommandPayload::AttendedTransferComplete { .. } => {
            Err(anyhow::anyhow!("AttendedTransferComplete not yet wired; see plan 04-03"))
        }
        CallCommandPayload::AttendedTransferCancel { .. } => {
            Err(anyhow::anyhow!("AttendedTransferCancel not yet wired; see plan 04-03"))
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
        CallCommandPayload::Play { .. } => {
            Err(anyhow::anyhow!("Play not yet wired; see plan 04-04"))
        }
        CallCommandPayload::Speak { .. } => {
            Err(anyhow::anyhow!("Speak not yet wired; see plan 04-04"))
        }
        CallCommandPayload::Dtmf { .. } => {
            Err(anyhow::anyhow!("Dtmf not yet wired; see plan 04-04"))
        }
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
}
