//! `ManipulationEngine` — Phase 9 SIP-manipulation runtime (Plan 09-03 GREEN).
//!
//! Full evaluator + executor replacing the 09-01 stubs per decisions
//! D-07..D-30 in 09-CONTEXT.md.
//!
//! Architecture mirrors `src/proxy/translation/engine.rs` (Phase 8 analog):
//!   - Fresh DB read per INVITE (`Entity::find().filter(is_active=true).all(db)`)
//!   - Direction filter (inbound/outbound/both)
//!   - Priority ASC sort
//!   - Per-condition regex cache keyed `{class_id}::{rule_idx}::{cond_idx}`
//!   - Per-call variable scope in `var_scope: Arc<DashMap<session_id, HashMap>>`
//!
//! Hangup short-circuits via private `ActionFlow` enum; anti-actions fire on
//! condition false; variable interpolation (`${source}`) handled in
//! set_header.value, set_var.value, and log.message via a `OnceLock<Regex>`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use regex::Regex;
use rsipstack::dialog::invitation::InviteOption;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::call::DialDirection;
use crate::models::manipulations::{Column, Entity as Manipulation};

/// Compiled regex for `${...}` interpolation placeholders (D-19).
/// Compiled once at first use and reused for every interpolation call.
static INTERP_RE: OnceLock<Regex> = OnceLock::new();

fn interp_regex() -> &'static Regex {
    INTERP_RE.get_or_init(|| Regex::new(r"\$\{([^}]+)\}").unwrap())
}

/// Private control-flow signal from `execute_action` (D-29 hangup
/// short-circuit). Using an enum avoids returning `Result<bool>` which
/// loses the `code`/`reason` data needed for the `Hangup` outcome.
enum ActionFlow {
    Continue,
    Hangup { code: u16, reason: String },
}

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

    /// Apply manipulation rules to `invite_option` (D-23).
    ///
    /// Algorithm (D-13, D-22..D-29):
    ///   1. Fresh DB read of every active class.
    ///   2. Direction filter — keep "both" plus the matching direction.
    ///   3. Priority ASC sort (stable).
    ///   4. For each class, deserialize rules JSON. For each rule, evaluate
    ///      conditions; run actions on true, anti_actions on false.
    ///   5. Hangup short-circuits all further processing immediately.
    pub async fn manipulate(
        &self,
        invite_option: &mut InviteOption,
        ctx: ManipulationContext,
        db: &DatabaseConnection,
    ) -> Result<ManipulationOutcome> {
        let mut trace = ManipulationTrace::default();

        // 1. Fresh DB read (D-13).
        let mut classes = Manipulation::find()
            .filter(Column::IsActive.eq(true))
            .all(db)
            .await?;

        // 2. Direction filter (D-22..D-23).
        classes.retain(|c| dir_matches(&c.direction, ctx.direction));

        // 3. Priority ASC sort (D-28 / Phase 8 D-09 reuse).
        classes.sort_by_key(|c| c.priority);

        // 4. Per-class rule loop.
        for class in &classes {
            let rules: Vec<Rule> = match serde_json::from_value(class.rules.clone()) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        class_id = %class.id,
                        error = %e,
                        "failed to deserialize rules JSON for manipulation class; skipping"
                    );
                    continue;
                }
            };

            for (rule_idx, rule) in rules.iter().enumerate() {
                let matched = self.evaluate_conditions(
                    &class.id,
                    rule_idx,
                    &rule.conditions,
                    &rule.condition_mode,
                    invite_option,
                    &ctx,
                )?;

                let actions = if matched {
                    &rule.actions
                } else {
                    &rule.anti_actions
                };

                if actions.is_empty() {
                    continue;
                }

                let rule_label = format!(
                    "{}::{}",
                    class.name,
                    rule.name
                        .clone()
                        .unwrap_or_else(|| format!("rule#{}", rule_idx))
                );
                trace.applied_rules.push(rule_label.clone());

                for action in actions {
                    match self
                        .execute_action(action, invite_option, &ctx, &rule_label, &mut trace)
                        .await?
                    {
                        ActionFlow::Continue => {}
                        ActionFlow::Hangup { code, reason } => {
                            return Ok(ManipulationOutcome::Hangup {
                                code,
                                reason,
                                trace,
                            });
                        }
                    }
                }
            }
        }

        Ok(ManipulationOutcome::Continue { trace })
    }

    // ── Condition evaluator (D-07, D-08, D-10) ──────────────────────────────

    /// Evaluate all conditions in `rule` and return whether the rule fires.
    ///
    /// `And` mode: all conditions must be true.
    /// `Or` mode: any condition must be true.
    fn evaluate_conditions(
        &self,
        class_id: &str,
        rule_idx: usize,
        conditions: &[Condition],
        mode: &ConditionMode,
        invite_option: &InviteOption,
        ctx: &ManipulationContext,
    ) -> Result<bool> {
        if conditions.is_empty() {
            // Empty condition list always matches (unconditional rule).
            return Ok(true);
        }

        for (cond_idx, cond) in conditions.iter().enumerate() {
            let source_val = self.resolve_source(&cond.source, invite_option, ctx);
            let result = self.apply_op(
                class_id,
                rule_idx,
                cond_idx,
                &cond.op,
                source_val.as_deref(),
                &cond.value,
            )?;

            match mode {
                ConditionMode::And => {
                    if !result {
                        return Ok(false);
                    }
                }
                ConditionMode::Or => {
                    if result {
                        return Ok(true);
                    }
                }
            }
        }

        // And: all passed. Or: none matched.
        Ok(matches!(mode, ConditionMode::And))
    }

    /// Resolve the `source` string to its current runtime value (D-07).
    ///
    /// Returns `None` for unknown sources (defense in depth — write-time
    /// validation rejects unknowns; a corrupted row reaches this path).
    fn resolve_source(
        &self,
        source: &str,
        invite_option: &InviteOption,
        ctx: &ManipulationContext,
    ) -> Option<String> {
        if source == "caller_number" {
            return Some(ctx.caller_number.clone());
        }
        if source == "destination_number" {
            return Some(ctx.destination_number.clone());
        }
        if source == "trunk" {
            return Some(ctx.trunk_name.clone());
        }
        if let Some(header_name) = source.strip_prefix("header:") {
            return invite_option.headers.as_ref().and_then(|hs| {
                hs.iter()
                    .find(|h| h.name().eq_ignore_ascii_case(header_name))
                    .map(|h| h.value().to_string())
            });
        }
        if let Some(var_name) = source.strip_prefix("var:") {
            return self
                .var_scope
                .get(&ctx.session_id)
                .and_then(|m| m.get(var_name).cloned());
        }
        tracing::warn!(
            source = source,
            session_id = %ctx.session_id,
            "unknown condition source; treating as no-match"
        );
        None
    }

    /// Apply one condition operator (D-08).
    ///
    /// `source_val` is `None` when the source couldn't be resolved (absent
    /// header, unset var, unknown source). For `equals`/`regex`/`starts_with`/
    /// `contains` that is treated as no-match; for `not_equals`/`not_regex` a
    /// missing source counts as "not present" → true.
    fn apply_op(
        &self,
        class_id: &str,
        rule_idx: usize,
        cond_idx: usize,
        op: &ConditionOp,
        source_val: Option<&str>,
        pattern: &str,
    ) -> Result<bool> {
        match op {
            ConditionOp::Equals => Ok(source_val.map_or(false, |v| v == pattern)),
            ConditionOp::NotEquals => Ok(source_val.map_or(true, |v| v != pattern)),
            ConditionOp::StartsWith => {
                Ok(source_val.map_or(false, |v| v.starts_with(pattern)))
            }
            ConditionOp::Contains => Ok(source_val.map_or(false, |v| v.contains(pattern))),
            ConditionOp::Regex => {
                let re = self.get_or_compile_regex(class_id, rule_idx, cond_idx, pattern)?;
                Ok(source_val.map_or(false, |v| re.is_match(v)))
            }
            ConditionOp::NotRegex => {
                let re = self.get_or_compile_regex(class_id, rule_idx, cond_idx, pattern)?;
                Ok(source_val.map_or(true, |v| !re.is_match(v)))
            }
        }
    }

    /// Compile-or-fetch a regex from the per-engine cache (D-09).
    ///
    /// Cache key: `{class_id}::{rule_idx}::{cond_idx}`.
    fn get_or_compile_regex(
        &self,
        class_id: &str,
        rule_idx: usize,
        cond_idx: usize,
        pattern: &str,
    ) -> Result<Arc<Regex>> {
        let key = format!("{}::{}::{}", class_id, rule_idx, cond_idx);
        if let Some(cached) = self.regex_cache.get(&key) {
            return Ok(cached.clone());
        }
        let re = Regex::new(pattern)?;
        let arc = Arc::new(re);
        self.regex_cache.insert(key, arc.clone());
        Ok(arc)
    }

    // ── Action executor (D-12..D-18) ─────────────────────────────────────────

    /// Execute one action. Returns `ActionFlow::Hangup` to short-circuit (D-29).
    async fn execute_action(
        &self,
        action: &Action,
        invite_option: &mut InviteOption,
        ctx: &ManipulationContext,
        rule_label: &str,
        trace: &mut ManipulationTrace,
    ) -> Result<ActionFlow> {
        match action {
            Action::SetHeader { name, value } => {
                let resolved = self.interpolate(value, invite_option, ctx);
                let headers = invite_option.headers.get_or_insert_with(Vec::new);
                // Replace case-insensitively if present; append otherwise (D-12).
                let mut replaced = false;
                for h in headers.iter_mut() {
                    if h.name().eq_ignore_ascii_case(name) {
                        *h = rsipstack::sip::Header::Other(
                            name.clone(),
                            resolved.clone(),
                        );
                        replaced = true;
                        break;
                    }
                }
                if !replaced {
                    headers.push(rsipstack::sip::Header::Other(
                        name.clone(),
                        resolved.clone(),
                    ));
                }
                trace
                    .triggered_actions
                    .push(format!("set_header {}={}", name, resolved));
            }

            Action::RemoveHeader { name } => {
                // Remove ALL instances, case-insensitive (D-13).
                if let Some(headers) = invite_option.headers.as_mut() {
                    headers.retain(|h| !h.name().eq_ignore_ascii_case(name));
                }
                trace
                    .triggered_actions
                    .push(format!("remove_header {}", name));
            }

            Action::SetVar { name, value } => {
                let resolved = self.interpolate(value, invite_option, ctx);
                self.var_scope
                    .entry(ctx.session_id.clone())
                    .or_insert_with(HashMap::new)
                    .insert(name.clone(), resolved.clone());
                trace
                    .triggered_actions
                    .push(format!("set_var {}={}", name, resolved));
            }

            Action::Log { level, message } => {
                let resolved = self.interpolate(message, invite_option, ctx);
                // Extract rule name from label for structured fields.
                let parts: Vec<&str> = rule_label.splitn(2, "::").collect();
                let class_name = parts.first().copied().unwrap_or("");
                let rule_name = parts.get(1).copied().unwrap_or("");
                match level {
                    LogLevel::Info => tracing::info!(
                        event = "manipulation_log",
                        session_id = %ctx.session_id,
                        class = class_name,
                        rule = rule_name,
                        "{}",
                        resolved
                    ),
                    LogLevel::Warn => tracing::warn!(
                        event = "manipulation_log",
                        session_id = %ctx.session_id,
                        class = class_name,
                        rule = rule_name,
                        "{}",
                        resolved
                    ),
                    LogLevel::Error => tracing::error!(
                        event = "manipulation_log",
                        session_id = %ctx.session_id,
                        class = class_name,
                        rule = rule_name,
                        "{}",
                        resolved
                    ),
                }
                trace
                    .triggered_actions
                    .push(format!("log {}", resolved));
            }

            Action::Hangup { sip_code, reason } => {
                trace
                    .triggered_actions
                    .push(format!("hangup {} {}", sip_code, reason));
                return Ok(ActionFlow::Hangup {
                    code: *sip_code,
                    reason: reason.clone(),
                });
            }

            Action::Sleep { duration_ms } => {
                tokio::time::sleep(Duration::from_millis(*duration_ms as u64)).await;
                trace
                    .triggered_actions
                    .push(format!("sleep {}ms", duration_ms));
            }
        }

        Ok(ActionFlow::Continue)
    }

    // ── Variable interpolation (D-19, D-20, D-21) ────────────────────────────

    /// Substitute `${source}` placeholders in `template` (D-19).
    ///
    /// Unknown sources resolve to empty string with a warning (D-20).
    fn interpolate(
        &self,
        template: &str,
        invite_option: &InviteOption,
        ctx: &ManipulationContext,
    ) -> String {
        let re = interp_regex();
        re.replace_all(template, |caps: &regex::Captures<'_>| {
            let source = &caps[1];
            match self.resolve_source(source, invite_option, ctx) {
                Some(val) => val,
                None => {
                    tracing::warn!(
                        event = "manipulation_interp_unknown",
                        source = source,
                        session_id = %ctx.session_id,
                        "interpolation placeholder '{}' resolved to empty; unknown source",
                        source
                    );
                    String::new()
                }
            }
        })
        .into_owned()
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

    /// Test-only peek into `var_scope` for assertions.
    #[cfg(test)]
    pub fn peek_var(&self, session_id: &str, name: &str) -> Option<String> {
        self.var_scope
            .get(session_id)
            .and_then(|m| m.get(name).cloned())
    }
}

impl Default for ManipulationEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ── Direction filter helper (D-22..D-23) ─────────────────────────────────────

/// Returns true when `stored` direction column value allows the call's direction.
///
/// - "both"     → always true (D-28)
/// - "inbound"  → true only for `DialDirection::Inbound`
/// - "outbound" → true only for `DialDirection::Outbound`
/// - anything else → false + warn (defense in depth; write-time rejects unknowns)
fn dir_matches(stored: &str, ctx_dir: DialDirection) -> bool {
    match stored {
        "both" => true,
        "inbound" => matches!(ctx_dir, DialDirection::Inbound),
        "outbound" => matches!(ctx_dir, DialDirection::Outbound),
        other => {
            tracing::warn!(
                direction = other,
                "unknown direction value in manipulation class; skipping"
            );
            false
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::manipulations::{
        ActiveModel as ManipulationActiveModel, Migration as ManipulationsMigration,
    };
    use rsipstack::sip::Uri;
    use sea_orm::{ActiveModelTrait, Database, Set};
    use sea_orm_migration::prelude::MigrationTrait;
    use sea_orm_migration::MigratorTrait;

    /// Minimal in-test Migrator that runs ONLY the manipulations migration.
    struct TestMigrator;

    #[async_trait::async_trait]
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![Box::new(ManipulationsMigration)]
        }
    }

    async fn fresh_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("open sqlite memory db");
        TestMigrator::up(&db, None)
            .await
            .expect("run manipulations migration");
        db
    }

    async fn seed_class(
        db: &DatabaseConnection,
        id: &str,
        name: &str,
        direction: &str,
        priority: i32,
        rules: Vec<Rule>,
    ) {
        let now = chrono::Utc::now();
        let am = ManipulationActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            description: Set(None),
            direction: Set(direction.to_string()),
            priority: Set(priority),
            is_active: Set(true),
            rules: Set(serde_json::to_value(&rules).expect("rules to json")),
            created_at: Set(now),
            updated_at: Set(now),
        };
        am.insert(db).await.expect("insert manipulation row");
    }

    fn make_invite() -> InviteOption {
        InviteOption {
            caller: Uri::try_from("sip:alice@example.com").expect("caller"),
            callee: Uri::try_from("sip:bob@example.com").expect("callee"),
            contact: Uri::try_from("sip:alice@192.168.1.1:5060").expect("contact"),
            ..Default::default()
        }
    }

    fn make_ctx(
        caller: &str,
        dest: &str,
        trunk: &str,
        dir: DialDirection,
        session: &str,
    ) -> ManipulationContext {
        ManipulationContext {
            caller_number: caller.into(),
            destination_number: dest.into(),
            trunk_name: trunk.into(),
            direction: dir,
            session_id: session.into(),
        }
    }

    /// Case-insensitive header lookup. Returns the *first* matching header
    /// value (test helper — actual remove_header in the engine drops all).
    fn header_value(opt: &InviteOption, name: &str) -> Option<String> {
        let target = name.to_ascii_lowercase();
        opt.headers.as_ref().and_then(|hs| {
            hs.iter()
                .find(|h| h.name().eq_ignore_ascii_case(&target))
                .map(|h| h.value().to_string())
        })
    }

    /// Count of headers with the given name (case-insensitive).
    fn header_count(opt: &InviteOption, name: &str) -> usize {
        opt.headers
            .as_ref()
            .map(|hs| {
                hs.iter()
                    .filter(|h| h.name().eq_ignore_ascii_case(name))
                    .count()
            })
            .unwrap_or(0)
    }

    fn cond(source: &str, op: ConditionOp, value: &str) -> Condition {
        Condition {
            source: source.into(),
            op,
            value: value.into(),
        }
    }

    fn rule(
        name: &str,
        conditions: Vec<Condition>,
        mode: ConditionMode,
        actions: Vec<Action>,
        anti_actions: Vec<Action>,
    ) -> Rule {
        Rule {
            name: Some(name.into()),
            conditions,
            condition_mode: mode,
            actions,
            anti_actions,
        }
    }

    // ─── Stub-baseline tests (kept from 09-01) ───────────────────────────

    #[test]
    fn new_engine_has_empty_caches() {
        let e = ManipulationEngine::new();
        assert_eq!(e.regex_cache_len(), 0);
        assert_eq!(e.var_scope_len(), 0);
    }

    #[tokio::test]
    async fn manipulate_empty_db_returns_continue_with_empty_trace() {
        let engine = ManipulationEngine::new();
        let db = fresh_db().await;
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "bob", "carrier-a", DialDirection::Inbound, "sess-1");
        let outcome = engine
            .manipulate(&mut opt, ctx, &db)
            .await
            .expect("manipulate ok");
        match outcome {
            ManipulationOutcome::Continue { trace } => {
                assert!(trace.applied_rules.is_empty());
                assert!(trace.triggered_actions.is_empty());
            }
            ManipulationOutcome::Hangup { .. } => panic!("expected continue"),
        }
    }

    // ─── Evaluator: condition op × source coverage (D-07, D-08) ──────────

    #[tokio::test]
    async fn eq_caller_matches() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "eq-caller",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "+15551234")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Match".into(),
                    value: "yes".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx(
            "+15551234",
            "999",
            "trunk-a",
            DialDirection::Inbound,
            "s",
        );
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-Match").as_deref(), Some("yes"));
    }

    #[tokio::test]
    async fn eq_caller_does_not_match() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "eq-caller",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "+15551234")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Match".into(),
                    value: "yes".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("+19998888", "999", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert!(header_value(&opt, "X-Match").is_none());
    }

    #[tokio::test]
    async fn not_eq_destination() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "ne-dest",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("destination_number", ConditionOp::NotEquals, "999")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-NE".into(),
                    value: "1".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        // Destination is "888" which != "999" -> match
        let ctx = make_ctx("c", "888", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-NE").as_deref(), Some("1"));

        // Reset and test the negative case
        let mut opt2 = make_invite();
        let ctx2 = make_ctx("c", "999", "t", DialDirection::Inbound, "s2");
        engine.manipulate(&mut opt2, ctx2, &db).await.unwrap();
        assert!(header_value(&opt2, "X-NE").is_none());
    }

    #[tokio::test]
    async fn regex_caller_matches_uk_prefix() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "uk",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Regex, r"^\+44")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Country".into(),
                    value: "UK".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("+442079460123", "x", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-Country").as_deref(), Some("UK"));
    }

    #[tokio::test]
    async fn not_regex_destination() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "not-regex",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("destination_number", ConditionOp::NotRegex, r"^999$")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-NR".into(),
                    value: "ok".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("c", "111", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-NR").as_deref(), Some("ok"));

        // negative case
        let mut opt2 = make_invite();
        let ctx2 = make_ctx("c", "999", "t", DialDirection::Inbound, "s2");
        engine.manipulate(&mut opt2, ctx2, &db).await.unwrap();
        assert!(header_value(&opt2, "X-NR").is_none());
    }

    #[tokio::test]
    async fn starts_with_trunk() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "starts",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("trunk", ConditionOp::StartsWith, "us-")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Region".into(),
                    value: "US".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("c", "d", "us-carrier-1", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-Region").as_deref(), Some("US"));
    }

    #[tokio::test]
    async fn contains_header_x_country() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "ctn",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("header:X-Country", ConditionOp::Contains, "UK")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Echo".into(),
                    value: "uk-detected".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        // Pre-populate the header (different case to validate case-insensitive match)
        opt.headers = Some(vec![rsipstack::sip::Header::Other(
            "x-country".into(),
            "Greater UK Region".into(),
        )]);
        let ctx = make_ctx("c", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(
            header_value(&opt, "X-Echo").as_deref(),
            Some("uk-detected")
        );
    }

    #[tokio::test]
    async fn var_equals_after_set_var() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "var-cascade",
            "both",
            100,
            vec![
                rule(
                    "set",
                    vec![cond("caller_number", ConditionOp::Equals, "alice")],
                    ConditionMode::And,
                    vec![Action::SetVar {
                        name: "greeting".into(),
                        value: "played".into(),
                    }],
                    vec![],
                ),
                rule(
                    "check",
                    vec![cond("var:greeting", ConditionOp::Equals, "played")],
                    ConditionMode::And,
                    vec![Action::SetHeader {
                        name: "X-G".into(),
                        value: "ok".into(),
                    }],
                    vec![],
                ),
            ],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "s-var");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-G").as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn unknown_source_treated_as_no_match() {
        // Defense-in-depth: if a corrupted row had source "totally_bogus",
        // the eval-time path must NOT panic; the rule simply does not match.
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "bogus",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("totally_bogus", ConditionOp::Equals, "x")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Should-Not-Set".into(),
                    value: "no".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("c", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert!(header_value(&opt, "X-Should-Not-Set").is_none());
    }

    // ─── Executor: each action type ──────────────────────────────────────

    #[tokio::test]
    async fn set_header_appends_when_absent() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "sh-append",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-New".into(),
                    value: "v".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-New").as_deref(), Some("v"));
        assert_eq!(header_count(&opt, "X-New"), 1);
    }

    #[tokio::test]
    async fn set_header_replaces_case_insensitive_when_present() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "sh-rep",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Foo".into(),
                    value: "new".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        opt.headers = Some(vec![rsipstack::sip::Header::Other(
            "x-foo".into(),
            "old".into(),
        )]);
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-Foo").as_deref(), Some("new"));
        assert_eq!(header_count(&opt, "X-Foo"), 1);
    }

    #[tokio::test]
    async fn remove_header_removes_all_instances_case_insensitive() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "rm",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::RemoveHeader {
                    name: "X-Internal".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        opt.headers = Some(vec![
            rsipstack::sip::Header::Other("X-Internal".into(), "a".into()),
            rsipstack::sip::Header::Other("x-internal".into(), "b".into()),
            rsipstack::sip::Header::Other("X-Keep".into(), "k".into()),
        ]);
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_count(&opt, "X-Internal"), 0);
        assert_eq!(header_value(&opt, "X-Keep").as_deref(), Some("k"));
    }

    #[tokio::test]
    async fn set_var_writes_per_session() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "sv",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetVar {
                    name: "x".into(),
                    value: "1".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "sess-A");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(engine.peek_var("sess-A", "x").as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn set_var_overwrites_existing() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "sv-ow",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![
                    Action::SetVar {
                        name: "x".into(),
                        value: "1".into(),
                    },
                    Action::SetVar {
                        name: "x".into(),
                        value: "2".into(),
                    },
                ],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "sess-B");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(engine.peek_var("sess-B", "x").as_deref(), Some("2"));
    }

    #[tokio::test]
    async fn log_action_does_not_error() {
        // We don't capture tracing here (subscriber fight in parallel tests).
        // We DO assert the action runs cleanly and doesn't bork the trace.
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "lg",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::Log {
                    level: LogLevel::Info,
                    message: "Routed via ${trunk}".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "trunk-x", DialDirection::Inbound, "s");
        let outcome = engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        match outcome {
            ManipulationOutcome::Continue { trace } => {
                assert_eq!(trace.applied_rules.len(), 1);
                assert!(trace
                    .triggered_actions
                    .iter()
                    .any(|a| a.starts_with("log ")));
            }
            _ => panic!("continue expected"),
        }
    }

    #[tokio::test]
    async fn hangup_returns_outcome_hangup_with_code_reason() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "hg",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "anonymous")],
                ConditionMode::And,
                vec![Action::Hangup {
                    sip_code: 603,
                    reason: "Decline".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("anonymous", "d", "t", DialDirection::Inbound, "s");
        let outcome = engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        match outcome {
            ManipulationOutcome::Hangup { code, reason, .. } => {
                assert_eq!(code, 603);
                assert_eq!(reason, "Decline");
            }
            _ => panic!("expected hangup"),
        }
    }

    #[tokio::test]
    async fn sleep_awaits_duration() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "slp",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::Sleep { duration_ms: 50 }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "s");
        let t0 = std::time::Instant::now();
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        let elapsed = t0.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(45),
            "elapsed {:?} should be >= 45ms",
            elapsed
        );
    }

    // ─── Cascade + control flow ──────────────────────────────────────────

    #[tokio::test]
    async fn cascade_within_class_two_rules_share_var() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "cascade-in",
            "both",
            100,
            vec![
                rule(
                    "r1",
                    vec![cond("caller_number", ConditionOp::Equals, "alice")],
                    ConditionMode::And,
                    vec![Action::SetVar {
                        name: "x".into(),
                        value: "1".into(),
                    }],
                    vec![],
                ),
                rule(
                    "r2",
                    vec![cond("var:x", ConditionOp::Equals, "1")],
                    ConditionMode::And,
                    vec![Action::SetHeader {
                        name: "X-Cascade".into(),
                        value: "YES".into(),
                    }],
                    vec![],
                ),
            ],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "s-c");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-Cascade").as_deref(), Some("YES"));
    }

    #[tokio::test]
    async fn cascade_across_classes_priority_order() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "cA",
            "first",
            "both",
            10,
            vec![rule(
                "r",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-First".into(),
                    value: "A".into(),
                }],
                vec![],
            )],
        )
        .await;
        seed_class(
            &db,
            "cB",
            "second",
            "both",
            20,
            vec![rule(
                "r",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-First".into(),
                    value: "B".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-First").as_deref(), Some("B"));
        assert_eq!(header_count(&opt, "X-First"), 1);
    }

    #[tokio::test]
    async fn hangup_short_circuits_within_rule() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "hg-sc",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "anonymous")],
                ConditionMode::And,
                vec![
                    Action::Hangup {
                        sip_code: 403,
                        reason: "Forbidden".into(),
                    },
                    Action::SetHeader {
                        name: "X-After-Hangup".into(),
                        value: "foo".into(),
                    },
                ],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("anonymous", "d", "t", DialDirection::Inbound, "s");
        let out = engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert!(matches!(out, ManipulationOutcome::Hangup { .. }));
        assert!(header_value(&opt, "X-After-Hangup").is_none());
    }

    #[tokio::test]
    async fn hangup_short_circuits_across_classes() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "cA",
            "first-hg",
            "both",
            10,
            vec![rule(
                "r",
                vec![cond("caller_number", ConditionOp::Equals, "anonymous")],
                ConditionMode::And,
                vec![Action::Hangup {
                    sip_code: 403,
                    reason: "Forbidden".into(),
                }],
                vec![],
            )],
        )
        .await;
        seed_class(
            &db,
            "cB",
            "after",
            "both",
            20,
            vec![rule(
                "r",
                vec![cond("caller_number", ConditionOp::Equals, "anonymous")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-After".into(),
                    value: "foo".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("anonymous", "d", "t", DialDirection::Inbound, "s");
        let out = engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert!(matches!(out, ManipulationOutcome::Hangup { .. }));
        assert!(header_value(&opt, "X-After").is_none());
    }

    #[tokio::test]
    async fn anti_actions_fire_on_false_and() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "anti",
            "both",
            100,
            vec![rule(
                "r1",
                vec![
                    cond("caller_number", ConditionOp::Regex, r"^\+44"),
                    cond("destination_number", ConditionOp::Equals, "999"),
                ],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Country".into(),
                    value: "UK".into(),
                }],
                vec![Action::SetHeader {
                    name: "X-Country".into(),
                    value: "OTHER".into(),
                }],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        // caller is US — fails the AND → anti-actions fire
        let ctx = make_ctx("+15551234", "999", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-Country").as_deref(), Some("OTHER"));
    }

    #[tokio::test]
    async fn anti_actions_fire_on_false_or() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "anti-or",
            "both",
            100,
            vec![rule(
                "r1",
                vec![
                    cond("caller_number", ConditionOp::Equals, "alice"),
                    cond("caller_number", ConditionOp::Equals, "bob"),
                ],
                ConditionMode::Or,
                vec![Action::SetHeader {
                    name: "X-W".into(),
                    value: "actions".into(),
                }],
                vec![Action::SetHeader {
                    name: "X-W".into(),
                    value: "anti".into(),
                }],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("charlie", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-W").as_deref(), Some("anti"));
    }

    #[tokio::test]
    async fn or_mode_matches_any() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "or",
            "both",
            100,
            vec![rule(
                "r1",
                vec![
                    cond("caller_number", ConditionOp::Regex, r"^\+44"),
                    cond("caller_number", ConditionOp::Regex, r"^\+1"),
                ],
                ConditionMode::Or,
                vec![Action::SetHeader {
                    name: "X-Region".into(),
                    value: "anglo".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("+15551234", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-Region").as_deref(), Some("anglo"));
    }

    // ─── Direction filter ───────────────────────────────────────────────

    #[tokio::test]
    async fn inbound_only_class_skipped_on_outbound() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "in-only",
            "inbound",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-In".into(),
                    value: "1".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Outbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert!(header_value(&opt, "X-In").is_none());
    }

    #[tokio::test]
    async fn both_class_fires_on_either() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "either",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Both".into(),
                    value: "1".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt1 = make_invite();
        engine
            .manipulate(
                &mut opt1,
                make_ctx("alice", "d", "t", DialDirection::Inbound, "sa"),
                &db,
            )
            .await
            .unwrap();
        assert_eq!(header_value(&opt1, "X-Both").as_deref(), Some("1"));
        let mut opt2 = make_invite();
        engine
            .manipulate(
                &mut opt2,
                make_ctx("alice", "d", "t", DialDirection::Outbound, "sb"),
                &db,
            )
            .await
            .unwrap();
        assert_eq!(header_value(&opt2, "X-Both").as_deref(), Some("1"));
    }

    // ─── Variable interpolation ─────────────────────────────────────────

    #[tokio::test]
    async fn interp_caller_in_set_header_value() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "interp",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Regex, r"^\+")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Caller".into(),
                    value: "${caller_number}".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("+442079460123", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(
            header_value(&opt, "X-Caller").as_deref(),
            Some("+442079460123")
        );
    }

    #[tokio::test]
    async fn interp_var_in_set_var_value_chained() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "chain",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![
                    Action::SetVar {
                        name: "x".into(),
                        value: "${caller_number}_x".into(),
                    },
                    Action::SetVar {
                        name: "y".into(),
                        value: "${var:x}_y".into(),
                    },
                ],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "s-chain");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(
            engine.peek_var("s-chain", "y").as_deref(),
            Some("alice_x_y")
        );
    }

    #[tokio::test]
    async fn interp_unknown_source_resolves_empty() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "unk",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-Test".into(),
                    value: "${var:nonexistent}".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "t", DialDirection::Inbound, "s");
        engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        assert_eq!(header_value(&opt, "X-Test").as_deref(), Some(""));
    }

    #[tokio::test]
    async fn interp_in_log_message() {
        // Smoke test — cover the log-interpolation site without asserting
        // tracing output (separate concern). The engine MUST NOT panic on
        // log interpolation, and trace must record the action.
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "log-i",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::Log {
                    level: LogLevel::Warn,
                    message: "from ${caller_number} via ${trunk}".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let ctx = make_ctx("alice", "d", "trunk-z", DialDirection::Inbound, "s");
        let outcome = engine.manipulate(&mut opt, ctx, &db).await.unwrap();
        match outcome {
            ManipulationOutcome::Continue { trace } => {
                assert!(trace.applied_rules.iter().any(|r| r.contains("log-i")));
            }
            _ => panic!("continue expected"),
        }
    }

    // ─── Var scope isolation ─────────────────────────────────────────────

    #[tokio::test]
    async fn var_scope_isolated_across_sessions() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "iso",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetVar {
                    name: "x".into(),
                    value: "1".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();

        let mut a = make_invite();
        engine
            .manipulate(
                &mut a,
                make_ctx("alice", "d", "t", DialDirection::Inbound, "sess-A"),
                &db,
            )
            .await
            .unwrap();
        // Different session — var:x must NOT leak.
        assert_eq!(engine.peek_var("sess-A", "x").as_deref(), Some("1"));
        assert!(engine.peek_var("sess-B", "x").is_none());
    }

    #[tokio::test]
    async fn cleanup_session_drops_scope() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "c1",
            "cs",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Equals, "alice")],
                ConditionMode::And,
                vec![Action::SetVar {
                    name: "x".into(),
                    value: "1".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        engine
            .manipulate(
                &mut opt,
                make_ctx("alice", "d", "t", DialDirection::Inbound, "sess-cs"),
                &db,
            )
            .await
            .unwrap();
        assert!(engine.peek_var("sess-cs", "x").is_some());
        engine.cleanup_session("sess-cs");
        assert!(engine.peek_var("sess-cs", "x").is_none());
    }

    // ─── Cache (regex compile cache) ─────────────────────────────────────

    #[tokio::test]
    async fn regex_cached_after_first_compile() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "rc1",
            "rc",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Regex, r"^\+44")],
                ConditionMode::And,
                vec![Action::SetHeader {
                    name: "X-UK".into(),
                    value: "y".into(),
                }],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        assert_eq!(engine.regex_cache_len(), 0);
        let mut opt = make_invite();
        engine
            .manipulate(
                &mut opt,
                make_ctx("+442079460123", "d", "t", DialDirection::Inbound, "s1"),
                &db,
            )
            .await
            .unwrap();
        assert_eq!(engine.regex_cache_len(), 1);
        // Second call: cache hit, length unchanged.
        let mut opt2 = make_invite();
        engine
            .manipulate(
                &mut opt2,
                make_ctx("+442079460999", "d", "t", DialDirection::Inbound, "s2"),
                &db,
            )
            .await
            .unwrap();
        assert_eq!(engine.regex_cache_len(), 1);
    }

    #[tokio::test]
    async fn invalidate_class_drops_keys_after_real_compile() {
        let db = fresh_db().await;
        seed_class(
            &db,
            "iv1",
            "iv",
            "both",
            100,
            vec![rule(
                "r1",
                vec![cond("caller_number", ConditionOp::Regex, r"^\+44")],
                ConditionMode::And,
                vec![],
                vec![],
            )],
        )
        .await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        engine
            .manipulate(
                &mut opt,
                make_ctx("+442079460123", "d", "t", DialDirection::Inbound, "s"),
                &db,
            )
            .await
            .unwrap();
        assert_eq!(engine.regex_cache_len(), 1);
        engine.invalidate_class("iv1");
        assert_eq!(engine.regex_cache_len(), 0);
    }

    // ─── Misc edge cases ────────────────────────────────────────────────

    #[tokio::test]
    async fn empty_rule_list_yields_empty_trace() {
        let db = fresh_db().await;
        seed_class(&db, "c1", "empty", "both", 100, vec![]).await;
        let engine = ManipulationEngine::new();
        let mut opt = make_invite();
        let outcome = engine
            .manipulate(
                &mut opt,
                make_ctx("a", "b", "t", DialDirection::Inbound, "s"),
                &db,
            )
            .await
            .unwrap();
        match outcome {
            ManipulationOutcome::Continue { trace } => {
                assert!(trace.applied_rules.is_empty());
                assert!(trace.triggered_actions.is_empty());
            }
            _ => panic!("continue expected"),
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
