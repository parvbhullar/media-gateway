//! Webhook response schema (Decision API).
//!
//! `WebhookDecisionRaw` is the deserialized wire shape exactly as the
//! controller emits it. `Decision` is the validated, strongly-typed enum
//! the dispatcher consumes.
//!
//! See spec: docs/superpowers/specs/2026-05-25-livekit-trunk-design.md.

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tracing::warn;

const MAX_OVERRIDE_LEN: usize = 256;
const MAX_METADATA_BYTES: usize = 16 * 1024;
const MAX_WAIT_MS: u64 = 10_000;
const DEFAULT_REJECT_CODE: u16 = 486;

#[derive(Debug, Deserialize)]
pub struct WebhookDecisionRaw {
    pub decision: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub identity: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub wait_ms: Option<u64>,
    #[serde(default)]
    pub reject_code: Option<u16>,
    #[serde(default)]
    pub reject_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Accept {
        /// Room name override, if supplied. None = use templated default.
        room: Option<String>,
        /// Identity override.
        identity: Option<String>,
        /// Metadata override, serialized as a JSON string. None = use
        /// templated default (or none).
        metadata: Option<String>,
        /// Already clamped to [0, MAX_WAIT_MS].
        wait_ms: u64,
    },
    Reject {
        /// Already validated to be in [400, 699]; default 486 if absent.
        code: u16,
        /// Truncated to MAX_OVERRIDE_LEN.
        reason: Option<String>,
    },
}

#[derive(Debug, Error)]
pub enum DecisionError {
    #[error("webhook: missing/invalid decision (must be 'accept' or 'reject')")]
    InvalidDecision,
    #[error("webhook: invalid room override (empty or > {0} chars)")]
    InvalidRoom(usize),
    #[error("webhook: invalid identity override (empty or > {0} chars)")]
    InvalidIdentity(usize),
    #[error("webhook: invalid metadata ({0})")]
    InvalidMetadata(String),
}

impl WebhookDecisionRaw {
    pub fn validate(self) -> Result<Decision, DecisionError> {
        match self.decision.as_str() {
            "accept" => {
                if let Some(r) = &self.room {
                    if r.is_empty() || r.len() > MAX_OVERRIDE_LEN {
                        return Err(DecisionError::InvalidRoom(MAX_OVERRIDE_LEN));
                    }
                }
                if let Some(i) = &self.identity {
                    if i.is_empty() || i.len() > MAX_OVERRIDE_LEN {
                        return Err(DecisionError::InvalidIdentity(MAX_OVERRIDE_LEN));
                    }
                }
                // metadata: accept either a JSON object/array/etc. (serialize
                // to canonical string) or a string (validate it parses as JSON
                // and pass through unchanged). Either way, the produced
                // `String` is what we hand to livekit_api::AccessToken::with_metadata.
                let metadata = match self.metadata {
                    None => None,
                    Some(Value::String(s)) => {
                        if s.len() > MAX_METADATA_BYTES {
                            return Err(DecisionError::InvalidMetadata(format!(
                                "> {MAX_METADATA_BYTES} bytes"
                            )));
                        }
                        serde_json::from_str::<Value>(&s).map_err(|e| {
                            DecisionError::InvalidMetadata(format!("not JSON: {e}"))
                        })?;
                        Some(s)
                    }
                    Some(v) => {
                        let s = serde_json::to_string(&v)
                            .map_err(|e| DecisionError::InvalidMetadata(e.to_string()))?;
                        if s.len() > MAX_METADATA_BYTES {
                            return Err(DecisionError::InvalidMetadata(format!(
                                "> {MAX_METADATA_BYTES} bytes"
                            )));
                        }
                        Some(s)
                    }
                };
                let wait_ms = match self.wait_ms {
                    None => 0,
                    Some(v) if v <= MAX_WAIT_MS => v,
                    Some(v) => {
                        warn!(
                            wait_ms = v,
                            max = MAX_WAIT_MS,
                            "webhook wait_ms out of range, clamping"
                        );
                        MAX_WAIT_MS
                    }
                };
                Ok(Decision::Accept {
                    room: self.room,
                    identity: self.identity,
                    metadata,
                    wait_ms,
                })
            }
            "reject" => {
                let code = match self.reject_code {
                    None => DEFAULT_REJECT_CODE,
                    Some(c) if (400..=699).contains(&c) => c,
                    Some(c) => {
                        warn!(
                            reject_code = c,
                            "webhook reject_code out of [400,699], using default 486"
                        );
                        DEFAULT_REJECT_CODE
                    }
                };
                let reason = self.reject_reason.map(|mut r| {
                    if r.len() > MAX_OVERRIDE_LEN {
                        r.truncate(MAX_OVERRIDE_LEN);
                    }
                    r
                });
                Ok(Decision::Reject { code, reason })
            }
            _ => Err(DecisionError::InvalidDecision),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: serde_json::Value) -> WebhookDecisionRaw {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn accept_with_no_overrides_parses() {
        let raw = parse(json!({"decision": "accept"}));
        let d = raw.validate().unwrap();
        match d {
            Decision::Accept { room, identity, metadata, wait_ms } => {
                assert!(room.is_none());
                assert!(identity.is_none());
                assert!(metadata.is_none());
                assert_eq!(wait_ms, 0);
            }
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[test]
    fn accept_with_room_and_identity_overrides_parses() {
        let raw = parse(json!({
            "decision": "accept",
            "room":     "agent-pool-7",
            "identity": "caller-xyz",
        }));
        let d = raw.validate().unwrap();
        match d {
            Decision::Accept { room, identity, .. } => {
                assert_eq!(room.as_deref(), Some("agent-pool-7"));
                assert_eq!(identity.as_deref(), Some("caller-xyz"));
            }
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[test]
    fn accept_with_metadata_object_serializes_to_json_string() {
        let raw = parse(json!({
            "decision": "accept",
            "metadata": {"tenant": "acme", "skill": "en"},
        }));
        let d = raw.validate().unwrap();
        match d {
            Decision::Accept { metadata, .. } => {
                let s = metadata.expect("metadata present");
                let back: serde_json::Value = serde_json::from_str(&s).unwrap();
                assert_eq!(back["tenant"], "acme");
                assert_eq!(back["skill"], "en");
            }
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[test]
    fn accept_with_metadata_json_string_passes_through() {
        let raw = parse(json!({
            "decision": "accept",
            "metadata": "{\"tenant\":\"acme\"}",
        }));
        let d = raw.validate().unwrap();
        match d {
            Decision::Accept { metadata, .. } => {
                assert_eq!(metadata.as_deref(), Some("{\"tenant\":\"acme\"}"));
            }
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[test]
    fn accept_metadata_string_must_be_valid_json() {
        let raw = parse(json!({
            "decision": "accept",
            "metadata": "not-json",
        }));
        let err = raw.validate().unwrap_err();
        match err {
            DecisionError::InvalidMetadata(msg) => assert!(msg.contains("not JSON")),
            other => panic!("expected InvalidMetadata, got {other:?}"),
        }
    }

    #[test]
    fn accept_rejects_empty_room() {
        let raw = parse(json!({"decision": "accept", "room": ""}));
        match raw.validate().unwrap_err() {
            DecisionError::InvalidRoom(_) => {}
            other => panic!("expected InvalidRoom, got {other:?}"),
        }
    }

    #[test]
    fn accept_rejects_too_long_room() {
        let long = "x".repeat(257);
        let raw = parse(json!({"decision": "accept", "room": long}));
        match raw.validate().unwrap_err() {
            DecisionError::InvalidRoom(257) | DecisionError::InvalidRoom(256) => {}
            other => panic!("expected InvalidRoom, got {other:?}"),
        }
    }

    #[test]
    fn accept_rejects_too_long_identity() {
        let long = "x".repeat(257);
        let raw = parse(json!({"decision": "accept", "identity": long}));
        match raw.validate().unwrap_err() {
            DecisionError::InvalidIdentity(_) => {}
            other => panic!("expected InvalidIdentity, got {other:?}"),
        }
    }

    #[test]
    fn accept_rejects_too_large_metadata_object() {
        // 16 KiB + 1 of JSON-object content
        let huge = "x".repeat(16 * 1024 + 1);
        let raw = parse(json!({"decision": "accept", "metadata": {"k": huge}}));
        match raw.validate().unwrap_err() {
            DecisionError::InvalidMetadata(msg) => assert!(msg.contains("bytes")),
            other => panic!("expected InvalidMetadata, got {other:?}"),
        }
    }

    #[test]
    fn accept_clamps_wait_ms_above_max() {
        let raw = parse(json!({"decision": "accept", "wait_ms": 999_999}));
        match raw.validate().unwrap() {
            Decision::Accept { wait_ms, .. } => assert_eq!(wait_ms, 10_000),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn accept_passes_wait_ms_in_range() {
        let raw = parse(json!({"decision": "accept", "wait_ms": 1_500}));
        match raw.validate().unwrap() {
            Decision::Accept { wait_ms, .. } => assert_eq!(wait_ms, 1_500),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn reject_with_no_code_defaults_to_486() {
        let raw = parse(json!({"decision": "reject"}));
        match raw.validate().unwrap() {
            Decision::Reject { code, reason } => {
                assert_eq!(code, 486);
                assert!(reason.is_none());
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn reject_with_valid_code_used() {
        let raw = parse(json!({"decision": "reject", "reject_code": 503}));
        match raw.validate().unwrap() {
            Decision::Reject { code, .. } => assert_eq!(code, 503),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn reject_with_out_of_range_code_falls_back_to_486() {
        let raw = parse(json!({"decision": "reject", "reject_code": 200}));
        match raw.validate().unwrap() {
            Decision::Reject { code, .. } => assert_eq!(code, 486),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn reject_truncates_reason() {
        let long = "y".repeat(500);
        let raw = parse(json!({"decision": "reject", "reject_reason": long}));
        match raw.validate().unwrap() {
            Decision::Reject { reason, .. } => {
                assert_eq!(reason.as_deref().map(str::len), Some(256));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn invalid_decision_field_errors() {
        let raw = parse(json!({"decision": "bogus"}));
        match raw.validate().unwrap_err() {
            DecisionError::InvalidDecision => {}
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let raw = parse(json!({"decision": "accept", "future_field": "ignored"}));
        let d = raw.validate().unwrap();
        matches!(d, Decision::Accept { .. });
    }
}
