//! Console/HTTP Command Adapter
//!
//! Converts `CallCommandPayload` (from HTTP API) to unified `CallCommand`.

use crate::call::domain::*;
use crate::call::runtime::command_payload::CallCommandPayload;
use crate::callrecord::CallRecordHangupReason;
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
        CallCommandPayload::ApiMute { .. } => {
            Err(anyhow::anyhow!("ApiMute not yet wired; see plan 04-02"))
        }
        CallCommandPayload::ApiUnmute { .. } => {
            Err(anyhow::anyhow!("ApiUnmute not yet wired; see plan 04-02"))
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
        CallCommandPayload::ApiHangup { .. } => {
            Err(anyhow::anyhow!("ApiHangup not yet wired; see plan 04-02"))
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
