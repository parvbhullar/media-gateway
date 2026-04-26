//! Phase 5 Plan 05-04 Task 2 — codec normalization + intersection.
//!
//! Bridges RFC 3551 wire-form codec names (uppercase) and SDP RTP payload
//! type numbers to the lowercase canonical storage form used by the trunk
//! `media_config.codecs` list.
//!
//! D-19: storage form is lowercase; SDP wire form may be either uppercase
//!       canonical name (e.g. "PCMU") or static payload-type numeric (e.g. "0").
//! D-20: empty trunk codec list = filter disabled (allow-all). The matcher
//!       must skip codec gating when `hints.allow_codecs` is None or empty.

use std::collections::HashSet;

/// Normalize one codec token to its lowercase canonical name. Returns
/// `None` for tokens not in the recognised RFC 3551 audio static set or
/// the dynamic codecs we surface (opus, telephone-event).
///
/// Inputs accepted:
///   - lowercase canonical: "pcmu", "pcma", "g722", "g729", "opus", "telephone-event"
///   - uppercase wire form: "PCMU", "PCMA", "G722", "G729", "OPUS", "TELEPHONE-EVENT"
///   - RFC 3551 static payload-type numbers: "0", "8", "9", "18", "101"
pub fn normalize_codec(s: &str) -> Option<&'static str> {
    let trimmed = s.trim();
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "pcmu" | "0" => Some("pcmu"),
        "pcma" | "8" => Some("pcma"),
        "g722" | "9" => Some("g722"),
        "g729" | "18" => Some("g729"),
        "opus" => Some("opus"),
        "telephone-event" | "101" => Some("telephone-event"),
        _ => None,
    }
}

/// Compute the trunk-allowed subset of caller-offered codecs, preserving
/// caller order. Inputs may be mixed-case or RTP payload-type numbers;
/// `normalize_codec` resolves both. Trunk list is treated as lowercase
/// storage form (Phase 3 D-09); we lowercase defensively.
///
/// Caller codecs that don't normalize to a known token are dropped from
/// the result (they cannot match the canonical trunk list).
pub fn intersect_codecs(caller: &[String], trunk: &[String]) -> Vec<String> {
    let mut allowed: HashSet<String> = HashSet::new();
    for t in trunk {
        let norm = normalize_codec(t)
            .map(|s| s.to_string())
            .unwrap_or_else(|| t.trim().to_ascii_lowercase());
        if !norm.is_empty() {
            allowed.insert(norm);
        }
    }

    let mut out = Vec::new();
    for c in caller {
        if let Some(norm) = normalize_codec(c) {
            if allowed.contains(norm) {
                out.push(norm.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalize_lowercase_passthrough() {
        assert_eq!(normalize_codec("pcmu"), Some("pcmu"));
    }

    #[test]
    fn normalize_uppercase_to_lower() {
        assert_eq!(normalize_codec("PCMU"), Some("pcmu"));
    }

    #[test]
    fn normalize_rtp_payload_type() {
        assert_eq!(normalize_codec("0"), Some("pcmu"));
        assert_eq!(normalize_codec("8"), Some("pcma"));
        assert_eq!(normalize_codec("9"), Some("g722"));
        assert_eq!(normalize_codec("18"), Some("g729"));
    }

    #[test]
    fn normalize_unknown_returns_none() {
        assert_eq!(normalize_codec("zzz"), None);
    }

    #[test]
    fn intersect_basic() {
        let r = intersect_codecs(&s(&["pcmu", "opus"]), &s(&["opus", "g729"]));
        assert_eq!(r, vec!["opus".to_string()]);
    }

    #[test]
    fn intersect_empty_caller() {
        let r = intersect_codecs(&Vec::new(), &s(&["pcmu"]));
        assert!(r.is_empty());
    }

    #[test]
    fn intersect_caller_uppercase_normalizes() {
        let r = intersect_codecs(&s(&["PCMU"]), &s(&["pcmu"]));
        assert_eq!(r, vec!["pcmu".to_string()]);
    }

    #[test]
    fn intersect_caller_payload_type_normalizes() {
        let r = intersect_codecs(&s(&["0", "18"]), &s(&["pcmu", "g729"]));
        assert_eq!(r, vec!["pcmu".to_string(), "g729".to_string()]);
    }

    #[test]
    fn intersect_disjoint_returns_empty() {
        let r = intersect_codecs(&s(&["g722"]), &s(&["pcmu", "pcma"]));
        assert!(r.is_empty());
    }
}
