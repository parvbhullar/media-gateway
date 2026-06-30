//! Canonical OK / USR / SYS outcome taxonomy and Q.850 parsing for CDRs.
//!
//! Single source of truth, used by the reporter at call end (write path, via
//! [`from_messages`]) and by the carrier API / console at read time (per-code
//! [`remark`]). The OK/USR/SYS bucket is a function of the final SIP code **and**
//! the Q.850 cause — e.g. `480` is `USR` for most causes but `SYS` for cause 102
//! (`RECOVERY_ON_TIMER_EXPIRE`) — so the kind is materialized onto the CDR row
//! rather than re-derived from `status_code` alone (a `GROUP BY status_code`
//! would misclassify the timer-expiry 480s).
//!
//! Taxonomy and per-code remarks are sourced from the operational call report
//! (captured in `openspec/changes/enrich-cdr-api/design.md`), not from
//! `scripts/qa_call_records_csv.py` (which only carries the finer-grained
//! `error_reason` strings).

use serde_json::Value;

use crate::callrecord::CallRecordHangupMessage;

/// Three-bucket outcome class for a finished call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    /// Answered / completed normally (2xx).
    Ok,
    /// User behaviour — no answer, busy, caller cancelled, declined.
    Usr,
    /// System / carrier / config fault — auth, unroutable, timeout, 5xx.
    Sys,
}

impl OutcomeKind {
    /// Stable uppercase token for the CDR `outcome_kind` column, matching the
    /// operational report (`OK` / `USR` / `SYS`).
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Usr => "USR",
            Self::Sys => "SYS",
        }
    }
}

/// Q.850 causes that promote an otherwise user-behaviour code to a system fault.
/// Evidence: `480 + cause 102` (`RECOVERY_ON_TIMER_EXPIRE`) is `SYS` in the
/// report. Extend as ops classification needs (e.g. 38 `NETWORK_OUT_OF_ORDER`).
const SYS_CAUSES: &[u16] = &[102];

/// The denormalized outcome fields stored on a CDR row at call end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallOutcome {
    pub kind: OutcomeKind,
    pub q850_cause: Option<u16>,
    pub q850_text: Option<String>,
}

/// Classify a finished call into OK / USR / SYS from its final SIP status code,
/// the parsed Q.850 cause, and whether the callee alerted (`rang`).
///
/// `rang` is part of the contract (the finer-grained `error_reason` splits on
/// it) but does not change the three-bucket today: `480` is `USR` whether it
/// rang or not. A 2xx call is always `OK` and is never overridden by a cause.
pub fn classify(status_code: u16, q850_cause: Option<u16>, rang: bool) -> OutcomeKind {
    let _ = rang;
    if (200..300).contains(&status_code) {
        return OutcomeKind::Ok;
    }
    let base = match status_code {
        480 | 486 | 487 | 600 | 603 => OutcomeKind::Usr,
        401 | 403 | 404 | 407 | 408 => OutcomeKind::Sys,
        c if (500..600).contains(&c) => OutcomeKind::Sys,
        c if (600..700).contains(&c) => OutcomeKind::Usr,
        // Unknown non-2xx: surface as a system fault rather than hide it.
        _ => OutcomeKind::Sys,
    };
    match q850_cause {
        Some(c) if SYS_CAUSES.contains(&c) => OutcomeKind::Sys,
        _ => base,
    }
}

/// Extract `(cause, text)` from an RFC 3326 `Reason:` value such as
/// `Q.850;cause=17;text="user busy"`. Returns `None` when no `cause=` is present;
/// a missing/blank `text=` yields an empty string. Tolerant of surrounding
/// whitespace and an unquoted text value.
pub fn parse_q850(reason: &str) -> Option<(u16, String)> {
    let after_cause = &reason[reason.find("cause=")? + "cause=".len()..];
    let digits: String = after_cause
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let cause: u16 = digits.parse().ok()?;
    let text = reason
        .find("text=")
        .map(|i| {
            let t = reason[i + "text=".len()..].trim_start();
            let t = t.strip_prefix('"').unwrap_or(t);
            match t.find('"') {
                Some(end) => t[..end].to_string(),
                None => t.trim().to_string(),
            }
        })
        .unwrap_or_default();
    Some((cause, text))
}

/// Per-code remark text for the summary's "SIP RESPONSE CODES" table. Verbatim
/// from the operational report; `None` for codes without a curated remark.
pub fn remark(code: u16) -> Option<&'static str> {
    Some(match code {
        200 => "Call answered and completed normally.",
        403 => "Forbidden — ACL or auth blocked the call. Check trunk auth/IP ACL.",
        404 => "Not found / unroutable — no route or DID mapping for this number.",
        408 => "Request timeout — upstream didn't respond in time.",
        480 => "No 200 OK / unavailable — destination didn't pick up (no-answer).",
        486 => "Destination line was busy.",
        487 => "Caller hung up / cancelled before the call connected.",
        500 => "Server error — internal fault. Check rustpbx logs.",
        503 => "Service unavailable — upstream/carrier overloaded or down.",
        _ => return None,
    })
}

/// Shared tail: parse the selected reason and classify.
fn finish(status_code: u16, reason: Option<&str>, rang: bool) -> CallOutcome {
    let (q850_cause, q850_text) = match reason.and_then(parse_q850) {
        Some((c, t)) => (Some(c), (!t.is_empty()).then_some(t)),
        None => (None, None),
    };
    CallOutcome {
        kind: classify(status_code, q850_cause, rang),
        q850_cause,
        q850_text,
    }
}

/// Write-path entry: classify from the typed hangup messages the reporter holds.
/// Selects the message whose `code` matches the final status code, else the last
/// recorded message — the same selection the console uses.
pub fn from_messages(
    status_code: u16,
    messages: &[CallRecordHangupMessage],
    rang: bool,
) -> CallOutcome {
    let reason = messages
        .iter()
        .find(|m| m.code == status_code)
        .or_else(|| messages.last())
        .and_then(|m| m.reason.as_deref());
    finish(status_code, reason, rang)
}

/// Backfill entry: classify from the `metadata.hangup_messages` JSON array of
/// existing rows. Mirrors [`from_messages`] selection on the untyped `Value`s.
pub fn from_json_messages(status_code: i16, messages: &[Value], rang: bool) -> CallOutcome {
    let code = status_code.max(0) as u16;
    let reason = messages
        .iter()
        .find(|m| m.get("code").and_then(|c| c.as_i64()) == Some(status_code as i64))
        .or_else(|| messages.last())
        .and_then(|m| m.get("reason"))
        .and_then(|r| r.as_str());
    finish(code, reason, rang)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(code: u16, reason: &str) -> CallRecordHangupMessage {
        CallRecordHangupMessage {
            code,
            reason: Some(reason.to_string()),
            target: None,
            endpoint: None,
        }
    }

    #[test]
    fn classify_base_codes_match_report() {
        assert_eq!(classify(200, None, false), OutcomeKind::Ok);
        assert_eq!(classify(480, None, true), OutcomeKind::Usr);
        assert_eq!(classify(480, None, false), OutcomeKind::Usr);
        assert_eq!(classify(486, None, false), OutcomeKind::Usr);
        assert_eq!(classify(487, None, true), OutcomeKind::Usr);
        assert_eq!(classify(403, None, false), OutcomeKind::Sys);
        assert_eq!(classify(404, None, false), OutcomeKind::Sys);
        assert_eq!(classify(408, None, false), OutcomeKind::Sys);
        assert_eq!(classify(500, None, false), OutcomeKind::Sys);
        assert_eq!(classify(503, None, true), OutcomeKind::Sys);
    }

    #[test]
    fn classify_q850_cause_102_overrides_480_to_sys() {
        assert_eq!(classify(480, Some(102), false), OutcomeKind::Sys);
        // a non-system cause leaves 480 as USR
        assert_eq!(classify(480, Some(31), false), OutcomeKind::Usr);
        // override never demotes an answered (2xx) call
        assert_eq!(classify(200, Some(102), false), OutcomeKind::Ok);
    }

    #[test]
    fn classify_unknown_defaults() {
        assert_eq!(classify(599, None, false), OutcomeKind::Sys); // 5xx
        assert_eq!(classify(699, None, false), OutcomeKind::Usr); // 6xx
        assert_eq!(classify(450, None, false), OutcomeKind::Sys); // unknown 4xx
    }

    #[test]
    fn parse_q850_extracts_cause_and_text() {
        assert_eq!(
            parse_q850("Q.850;cause=31;text=\"NORMAL_UNSPECIFIED\""),
            Some((31, "NORMAL_UNSPECIFIED".to_string()))
        );
        assert_eq!(
            parse_q850("Q.850;cause=17;text=\"user busy\""),
            Some((17, "user busy".to_string()))
        );
        // tolerates surrounding spaces
        assert_eq!(
            parse_q850("Q.850 ;cause=102; text=\"RECOVERY_ON_TIMER_EXPIRE\" "),
            Some((102, "RECOVERY_ON_TIMER_EXPIRE".to_string()))
        );
        // cause with no text → empty text
        assert_eq!(parse_q850("Q.850;cause=16"), Some((16, String::new())));
        // no cause → None
        assert_eq!(parse_q850("no cause here"), None);
    }

    #[test]
    fn remark_known_and_unknown() {
        assert_eq!(
            remark(480),
            Some("No 200 OK / unavailable — destination didn't pick up (no-answer).")
        );
        assert_eq!(remark(999), None);
    }

    #[test]
    fn from_messages_selects_by_code_then_classifies() {
        let msgs = [
            msg(486, "Q.850;cause=17;text=\"busy\""),
            msg(480, "Q.850;cause=102;text=\"RECOVERY_ON_TIMER_EXPIRE\""),
        ];
        // final code 480 → selects the 480 message → cause 102 → SYS override
        let o = from_messages(480, &msgs, true);
        assert_eq!(o.kind, OutcomeKind::Sys);
        assert_eq!(o.q850_cause, Some(102));
        assert_eq!(o.q850_text.as_deref(), Some("RECOVERY_ON_TIMER_EXPIRE"));

        // no matching code → falls back to last message
        let o2 = from_messages(600, &msgs, false);
        assert_eq!(o2.q850_cause, Some(102));
    }

    #[test]
    fn from_messages_no_messages_is_code_only() {
        let o = from_messages(486, &[], false);
        assert_eq!(o.kind, OutcomeKind::Usr);
        assert_eq!(o.q850_cause, None);
        assert_eq!(o.q850_text, None);
    }

    #[test]
    fn from_json_messages_mirrors_typed_path() {
        let msgs = vec![serde_json::json!({
            "code": 480,
            "reason": "Q.850;cause=102;text=\"RECOVERY_ON_TIMER_EXPIRE\""
        })];
        let o = from_json_messages(480, &msgs, false);
        assert_eq!(o.kind, OutcomeKind::Sys);
        assert_eq!(o.q850_cause, Some(102));
    }
}
