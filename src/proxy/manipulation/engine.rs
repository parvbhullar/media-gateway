//! `ManipulationEngine` — Phase 9 SIP-manipulation runtime (Plan 09-01 STUB).
//!
//! This file lands the public surface and the frozen wire-type contract that
//! Wave 2 (09-02 CRUD, 09-03 engine impl, 09-04 hot-path insertion) depends
//! on. Method bodies are stubs:
//!
//!   - `manipulate(...)` returns `Ok(ManipulationOutcome::Continue { trace: default() })`
//!     immediately (09-03 lands the real DB-load + condition-eval body).
//!   - `invalidate_class(class_id)` removes any `regex_cache` entries whose
//!     key starts with `{class_id}::` (cheap and correct now; 09-03 reuses
//!     this unchanged).
//!   - `cleanup_session(session_id)` removes the session entry from
//!     `var_scope` (D-15 isolation).
//!
//! Mirrors `src/proxy/translation/engine.rs` (Phase 8 analog) for direct
//! architectural symmetry: `Arc<DashMap>` cache field, `pub fn new()`,
//! cheap-to-clone via `Arc`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use regex::Regex;
use rsipstack::dialog::invitation::InviteOption;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};

use crate::call::DialDirection;

// ─── Wire types (D-05, D-06, D-11, D-23..D-25) ──────────────────────────────

/// How conditions in a rule are combined.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConditionMode {
    #[serde(rename = "and")]
    And,
    #[serde(rename = "or")]
    Or,
}

/// Comparison operator for a single condition (D-06).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConditionOp {
    Equals,
    NotEquals,
    Regex,
    NotRegex,
    StartsWith,
    Contains,
}

/// Severity for the `Log` action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LogLevel {
    #[serde(rename = "info")]
    Info,
    #[serde(rename = "warn")]
    Warn,
    #[serde(rename = "error")]
    Error,
}

/// One condition operand inside a rule (D-05).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    pub source: String,
    pub op: ConditionOp,
    pub value: String,
}

/// One action emitted when a rule fires (D-11). Tagged via `type` field for
/// JSON discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    SetHeader { name: String, value: String },
    RemoveHeader { name: String },
    SetVar { name: String, value: String },
    Log { level: LogLevel, message: String },
    Hangup { sip_code: u16, reason: String },
    Sleep { duration_ms: u32 },
}

/// One rule — embedded in `supersip_manipulations.rules` JSON column (D-05).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default = "default_condition_mode")]
    pub condition_mode: ConditionMode,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub anti_actions: Vec<Action>,
}

fn default_condition_mode() -> ConditionMode {
    ConditionMode::And
}

// ─── Engine context / outcome / trace (D-23..D-25) ──────────────────────────

/// Per-call evaluation context (D-23). Cheap to construct on the hot path.
pub struct ManipulationContext {
    pub caller_number: String,
    pub destination_number: String,
    pub trunk_name: String,
    pub direction: DialDirection,
    pub session_id: String,
}

/// Per-call audit trail (D-25). `applied_rules` lists `class::rule` names that
/// fired; `triggered_actions` is a human-readable summary of what changed.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ManipulationTrace {
    pub applied_rules: Vec<String>,
    pub triggered_actions: Vec<String>,
}

/// Outcome of `ManipulationEngine::manipulate` (D-24).
#[derive(Debug, Clone, Serialize)]
pub enum ManipulationOutcome {
    Continue {
        trace: ManipulationTrace,
    },
    Hangup {
        code: u16,
        reason: String,
        trace: ManipulationTrace,
    },
}

// ─── Engine ─────────────────────────────────────────────────────────────────

/// SIP-manipulation runtime. Cheap to clone via `Arc` — the engine itself
/// holds only two `Arc<DashMap>` caches, so cloning is a refcount bump.
pub struct ManipulationEngine {
    /// Per-rule compiled-regex cache. Key is `{class_id}::{rule_idx}` so
    /// `invalidate_class(class_id)` can drop every regex for a class in one
    /// pass. 09-03 populates this lazily on first match.
    regex_cache: Arc<DashMap<String, Arc<Regex>>>,
    /// Per-session variable scope (D-15). Outer key is `session_id`; inner
    /// map is `var_name -> value`. 09-03 writes via `SetVar`, 09-04 wires
    /// `cleanup_session` into the hangup path.
    var_scope: Arc<DashMap<String, HashMap<String, String>>>,
}

impl ManipulationEngine {
    /// Construct a fresh engine with empty caches.
    pub fn new() -> Self {
        Self {
            regex_cache: Arc::new(DashMap::new()),
            var_scope: Arc::new(DashMap::new()),
        }
    }

    /// Apply manipulation rules to `invite_option`.
    ///
    /// STUB — implementation lands in 09-03 (D-23). Returns `Continue` with
    /// an empty trace so downstream call paths see a benign default while
    /// Wave 2 lands.
    pub async fn manipulate(
        &self,
        _invite_option: &mut InviteOption,
        _ctx: ManipulationContext,
        _db: &DatabaseConnection,
    ) -> Result<ManipulationOutcome> {
        // STUB — implementation lands in 09-03 (D-23)
        Ok(ManipulationOutcome::Continue {
            trace: ManipulationTrace::default(),
        })
    }

    /// Drop every cached compiled regex associated with `class_id`.
    ///
    /// Called by the 09-02 PUT/DELETE handlers so the next `manipulate`
    /// invocation recompiles from the freshly-stored rules. Iterates the
    /// cache once and removes any key matching `{class_id}::*`.
    pub fn invalidate_class(&self, class_id: &str) {
        let prefix = format!("{}::", class_id);
        let stale: Vec<String> = self
            .regex_cache
            .iter()
            .filter_map(|kv| {
                let k = kv.key();
                if k.starts_with(&prefix) {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        for k in stale {
            self.regex_cache.remove(&k);
        }
    }

    /// Drop the per-session variable scope (D-15). 09-04 wires this into the
    /// hangup path so var_scope cannot leak across calls.
    pub fn cleanup_session(&self, session_id: &str) {
        self.var_scope.remove(session_id);
    }

    /// Test-only accessor for the regex cache size.
    #[cfg(test)]
    pub fn regex_cache_len(&self) -> usize {
        self.regex_cache.len()
    }

    /// Test-only accessor for the var_scope size.
    #[cfg(test)]
    pub fn var_scope_len(&self) -> usize {
        self.var_scope.len()
    }

    /// Test-only seed for `regex_cache` — used to verify `invalidate_class`
    /// without depending on the (still-stubbed) `manipulate` body.
    #[cfg(test)]
    pub fn seed_regex(&self, key: &str, pattern: &str) {
        self.regex_cache
            .insert(key.to_string(), Arc::new(Regex::new(pattern).unwrap()));
    }

    /// Test-only seed for `var_scope`.
    #[cfg(test)]
    pub fn seed_var(&self, session_id: &str, name: &str, value: &str) {
        let mut entry = self
            .var_scope
            .entry(session_id.to_string())
            .or_insert_with(HashMap::new);
        entry.insert(name.to_string(), value.to_string());
    }
}

impl Default for ManipulationEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rsipstack::sip::Uri;

    fn make_invite() -> InviteOption {
        InviteOption {
            caller: Uri::try_from("sip:alice@example.com").expect("caller"),
            callee: Uri::try_from("sip:bob@example.com").expect("callee"),
            contact: Uri::try_from("sip:alice@192.168.1.1:5060").expect("contact"),
            ..Default::default()
        }
    }

    #[test]
    fn new_engine_has_empty_caches() {
        let e = ManipulationEngine::new();
        assert_eq!(e.regex_cache_len(), 0);
        assert_eq!(e.var_scope_len(), 0);
    }

    #[tokio::test]
    async fn manipulate_stub_returns_continue_with_empty_trace() {
        let engine = ManipulationEngine::new();
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("open sqlite memory");
        let mut opt = make_invite();
        let ctx = ManipulationContext {
            caller_number: "alice".into(),
            destination_number: "bob".into(),
            trunk_name: "carrier-a".into(),
            direction: DialDirection::Inbound,
            session_id: "sess-1".into(),
        };
        let outcome = engine
            .manipulate(&mut opt, ctx, &db)
            .await
            .expect("stub manipulate ok");
        match outcome {
            ManipulationOutcome::Continue { trace } => {
                assert!(trace.applied_rules.is_empty());
                assert!(trace.triggered_actions.is_empty());
            }
            ManipulationOutcome::Hangup { .. } => {
                panic!("stub must return Continue, not Hangup")
            }
        }
    }

    #[test]
    fn invalidate_class_removes_only_matching_prefix() {
        let engine = ManipulationEngine::new();
        engine.seed_regex("class-a::0", r"^\d+$");
        engine.seed_regex("class-a::1", r"^foo$");
        engine.seed_regex("class-b::0", r"^bar$");
        assert_eq!(engine.regex_cache_len(), 3);

        engine.invalidate_class("class-a");
        assert_eq!(engine.regex_cache_len(), 1, "only class-b should remain");

        engine.invalidate_class("class-a"); // idempotent
        assert_eq!(engine.regex_cache_len(), 1);

        engine.invalidate_class("class-missing"); // safe on missing
        assert_eq!(engine.regex_cache_len(), 1);

        engine.invalidate_class("class-b");
        assert_eq!(engine.regex_cache_len(), 0);
    }

    #[test]
    fn cleanup_session_removes_session_only() {
        let engine = ManipulationEngine::new();
        engine.seed_var("sess-1", "x", "1");
        engine.seed_var("sess-2", "y", "2");
        assert_eq!(engine.var_scope_len(), 2);

        engine.cleanup_session("sess-1");
        assert_eq!(engine.var_scope_len(), 1);

        engine.cleanup_session("missing"); // idempotent
        assert_eq!(engine.var_scope_len(), 1);

        engine.cleanup_session("sess-2");
        assert_eq!(engine.var_scope_len(), 0);
    }

    // ─── Wire-type serde round-trips (D-05, D-06, D-11) ──────────────────

    #[test]
    fn rule_serde_round_trip_with_defaults() {
        let json = serde_json::json!({
            "name": "block-anon",
            "conditions": [
                {"source": "caller_number", "op": "equals", "value": "anonymous"}
            ],
            "condition_mode": "and",
            "actions": [
                {"type": "hangup", "sip_code": 603, "reason": "Decline"}
            ],
            "anti_actions": []
        });
        let rule: Rule = serde_json::from_value(json.clone()).expect("decode rule");
        assert_eq!(rule.name.as_deref(), Some("block-anon"));
        assert_eq!(rule.conditions.len(), 1);
        assert!(matches!(rule.conditions[0].op, ConditionOp::Equals));
        assert!(matches!(rule.condition_mode, ConditionMode::And));
        assert_eq!(rule.actions.len(), 1);
        match &rule.actions[0] {
            Action::Hangup { sip_code, reason } => {
                assert_eq!(*sip_code, 603);
                assert_eq!(reason, "Decline");
            }
            _ => panic!("expected hangup action"),
        }
        assert!(rule.anti_actions.is_empty());

        // Round-trip back to JSON.
        let back = serde_json::to_value(&rule).expect("encode rule");
        assert_eq!(back["actions"][0]["type"], "hangup");
    }

    #[test]
    fn rule_minimal_uses_defaults() {
        let json = serde_json::json!({
            "conditions": [],
            "actions": []
        });
        let rule: Rule = serde_json::from_value(json).expect("decode minimal rule");
        assert!(rule.name.is_none());
        assert!(matches!(rule.condition_mode, ConditionMode::And));
        assert!(rule.anti_actions.is_empty());
    }

    #[test]
    fn action_variants_serialize_with_snake_case_tag() {
        let actions = vec![
            Action::SetHeader {
                name: "X-Foo".into(),
                value: "bar".into(),
            },
            Action::RemoveHeader {
                name: "X-Foo".into(),
            },
            Action::SetVar {
                name: "v1".into(),
                value: "1".into(),
            },
            Action::Log {
                level: LogLevel::Warn,
                message: "hi".into(),
            },
            Action::Hangup {
                sip_code: 503,
                reason: "Busy".into(),
            },
            Action::Sleep { duration_ms: 50 },
        ];
        let v = serde_json::to_value(&actions).expect("encode");
        assert_eq!(v[0]["type"], "set_header");
        assert_eq!(v[1]["type"], "remove_header");
        assert_eq!(v[2]["type"], "set_var");
        assert_eq!(v[3]["type"], "log");
        assert_eq!(v[3]["level"], "warn");
        assert_eq!(v[4]["type"], "hangup");
        assert_eq!(v[5]["type"], "sleep");
    }

    #[test]
    fn condition_mode_or_round_trips() {
        let json = serde_json::json!({"condition_mode": "or", "conditions": [], "actions": []});
        let rule: Rule = serde_json::from_value(json).expect("decode");
        assert!(matches!(rule.condition_mode, ConditionMode::Or));
    }

    #[test]
    fn condition_op_snake_case_round_trip() {
        let json = serde_json::json!({
            "source": "destination_number",
            "op": "starts_with",
            "value": "+44"
        });
        let c: Condition = serde_json::from_value(json).expect("decode condition");
        assert!(matches!(c.op, ConditionOp::StartsWith));
        let back = serde_json::to_value(&c).expect("encode");
        assert_eq!(back["op"], "starts_with");
    }
}
