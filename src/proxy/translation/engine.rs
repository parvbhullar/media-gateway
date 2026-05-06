//! `TranslationEngine` — Phase 8 number-translation runtime (Plan 08-03 GREEN).
//!
//! Real implementation per CONTEXT.md decisions:
//!   - D-08, D-09: priority-ASC cascade; each rule sees the OUTPUT of earlier rules.
//!   - D-05..D-07: per-field independence — null pattern skipped; non-matching pattern skipped.
//!   - D-13: fresh DB read per INVITE (`Translations::find().filter(is_active=true)`).
//!   - D-15: URI mutation reuses `update_uri_user` from `src/proxy/routing/matcher.rs`.
//!   - D-18, D-19: `regex::Regex` (linear-time, no catastrophic backtracking) with
//!     native Rust `$1`/`${name}` capture syntax via `regex.replace_all`.
//!   - D-20: per-rule compiled-regex cache; `invalidate(rule_id)` drops a single entry.
//!   - D-22..D-24: direction filter — inbound-only rule does NOT fire on outbound INVITE.

use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use regex::Regex;
use rsipstack::dialog::invitation::InviteOption;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};

use crate::call::DialDirection;
use crate::models::routing::RoutingDirection;
use crate::models::translations::{Column as TranslationColumn, Entity as Translations};
use crate::proxy::routing::matcher::update_uri_user;

/// Number-translation runtime. Cheap to clone via `Arc` — the engine itself
/// holds only an `Arc<DashMap>` cache, so cloning is a refcount bump.
pub struct TranslationEngine {
    /// Per-rule compiled-regex cache (D-20). Key is the rule id (UUID v4
    /// string). 08-02 PUT/DELETE handlers call `invalidate(id)` so the next
    /// `translate` call recompiles from the new pattern.
    ///
    /// Two distinct sub-keys are used (`{rule_id}::caller` and
    /// `{rule_id}::destination`) so a rule with both pattern fields gets one
    /// compiled `Regex` per field. `invalidate(rule_id)` removes both entries.
    cache: Arc<DashMap<String, Arc<Regex>>>,
}

impl TranslationEngine {
    /// Construct a fresh engine with an empty cache.
    pub fn new() -> Self {
        Self {
            cache: Arc::new(DashMap::new()),
        }
    }

    /// Apply translation rules to `invite_option` according to `direction`.
    ///
    /// Algorithm (D-08, D-09, D-13, D-22..D-24):
    ///   1. Fresh DB read of every active rule, ordered by `priority` ASC.
    ///   2. For each rule whose `direction` matches the call's `DialDirection`
    ///      (or whose `direction` is `both`), apply caller and destination
    ///      rewrites independently (D-05..D-07).
    ///   3. The next rule sees the already-mutated caller / destination
    ///      (cascade — D-08).
    ///   4. Each successful rewrite is recorded in the returned trace.
    pub async fn translate(
        &self,
        invite_option: &mut InviteOption,
        direction: DialDirection,
        db: &DatabaseConnection,
    ) -> Result<TranslationTrace> {
        let mut trace = TranslationTrace::default();

        // D-13: fresh DB read per INVITE; only is_active=true rows; sorted
        // ascending by priority so the iteration order IS the cascade order.
        let rules = Translations::find()
            .filter(TranslationColumn::IsActive.eq(true))
            .order_by_asc(TranslationColumn::Priority)
            .all(db)
            .await?;

        for rule in rules.iter() {
            // D-22..D-24 direction filter. `direction_enum()` returns `None`
            // for the "both" sentinel, in which case the rule fires on either
            // direction.
            let rule_dir = rule.direction_enum();
            if !direction_matches(rule_dir, direction) {
                continue;
            }

            // D-05..D-07: caller field is rewritten only when (a) the rule
            // has a caller_pattern, (b) it compiles, (c) it matches the
            // current caller.user(), and (d) the replacement actually changes
            // the value.
            if let (Some(pat), Some(repl)) =
                (rule.caller_pattern.as_deref(), rule.caller_replacement.as_deref())
            {
                let cache_key = format!("{}::caller", rule.id);
                if let Some(re) = self.get_or_compile(&cache_key, pat) {
                    let current = invite_option.caller.user().unwrap_or_default().to_string();
                    if re.is_match(&current) {
                        let new_val = re.replace_all(&current, repl).into_owned();
                        if new_val != current {
                            invite_option.caller =
                                update_uri_user(&invite_option.caller, &new_val)?;
                            trace.applied_rules.push(AppliedRule {
                                rule_id: rule.id.clone(),
                                rule_name: rule.name.clone(),
                                field: "caller".into(),
                                before: current,
                                after: new_val,
                            });
                        }
                    }
                }
            }

            // Mirror for destination — same per-field-independence semantics.
            if let (Some(pat), Some(repl)) = (
                rule.destination_pattern.as_deref(),
                rule.destination_replacement.as_deref(),
            ) {
                let cache_key = format!("{}::destination", rule.id);
                if let Some(re) = self.get_or_compile(&cache_key, pat) {
                    let current = invite_option.callee.user().unwrap_or_default().to_string();
                    if re.is_match(&current) {
                        let new_val = re.replace_all(&current, repl).into_owned();
                        if new_val != current {
                            invite_option.callee =
                                update_uri_user(&invite_option.callee, &new_val)?;
                            trace.applied_rules.push(AppliedRule {
                                rule_id: rule.id.clone(),
                                rule_name: rule.name.clone(),
                                field: "destination".into(),
                                before: current,
                                after: new_val,
                            });
                        }
                    }
                }
            }
        }

        Ok(trace)
    }

    /// Drop every cached compiled regex associated with `rule_id` (D-20).
    /// Called by the 08-02 PUT/DELETE handlers so the next `translate`
    /// invocation recompiles from the freshly-stored pattern.
    pub fn invalidate(&self, rule_id: &str) {
        // Remove the bare id (08-01 stub key) for backwards compatibility
        // with previously seeded caches, plus the per-field sub-keys used by
        // the real implementation.
        self.cache.remove(rule_id);
        self.cache.remove(&format!("{}::caller", rule_id));
        self.cache.remove(&format!("{}::destination", rule_id));
    }

    /// Test-only accessor for the cache size — used by 08-03 tests.
    #[cfg(test)]
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Cache-or-compile a regex for `cache_key`. On compile failure the rule
    /// is skipped with a warning log (defense in depth — write-time validation
    /// in 08-02 prevents this in practice; see D-19, T-08-03-03).
    fn get_or_compile(&self, cache_key: &str, pattern: &str) -> Option<Arc<Regex>> {
        if let Some(r) = self.cache.get(cache_key) {
            return Some(r.clone());
        }
        match Regex::new(pattern) {
            Ok(re) => {
                let arc = Arc::new(re);
                self.cache.insert(cache_key.to_string(), arc.clone());
                Some(arc)
            }
            Err(e) => {
                tracing::warn!(
                    cache_key,
                    pattern,
                    error = %e,
                    "translation rule pattern failed to compile; skipping"
                );
                None
            }
        }
    }
}

impl Default for TranslationEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Direction filter (D-22..D-24). `rule_dir == None` means the rule's
/// `direction` column is `"both"` — fires on any `DialDirection`.
fn direction_matches(rule_dir: Option<RoutingDirection>, call: DialDirection) -> bool {
    match (rule_dir, call) {
        (None, _) => true,
        (Some(RoutingDirection::Inbound), DialDirection::Inbound) => true,
        (Some(RoutingDirection::Outbound), DialDirection::Outbound) => true,
        _ => false,
    }
}

/// Per-call audit trail of which translation rules fired (D-13).
#[derive(Debug, Default)]
pub struct TranslationTrace {
    pub applied_rules: Vec<AppliedRule>,
}

/// One entry of the trace — emitted when a rule actually mutates a field.
/// `before`/`after` carry the user-portion of the SIP URI before and after
/// rewrite (matcher contract — see `src/proxy/routing/matcher.rs`).
#[derive(Debug)]
pub struct AppliedRule {
    pub rule_id: String,
    pub rule_name: String,
    /// "caller" | "destination"
    pub field: String,
    pub before: String,
    pub after: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::translations::{ActiveModel, Migration as TranslationsMigration};
    use rsipstack::sip::Uri;
    use sea_orm::{ActiveModelTrait, Database, Set};
    use sea_orm_migration::prelude::*;
    use sea_orm_migration::MigratorTrait;

    /// Minimal in-test Migrator that runs ONLY the translations migration.
    struct TestMigrator;

    #[async_trait::async_trait]
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![Box::new(TranslationsMigration)]
        }
    }

    async fn fresh_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("open sqlite memory db");
        TestMigrator::up(&db, None)
            .await
            .expect("run translations migration");
        db
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_translation(
        db: &DatabaseConnection,
        id: &str,
        name: &str,
        caller_pattern: Option<&str>,
        caller_replacement: Option<&str>,
        destination_pattern: Option<&str>,
        destination_replacement: Option<&str>,
        direction: &str,
        priority: i32,
        is_active: bool,
    ) -> String {
        let now = chrono::Utc::now();
        let am = ActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            description: Set(None),
            caller_pattern: Set(caller_pattern.map(|s| s.to_string())),
            destination_pattern: Set(destination_pattern.map(|s| s.to_string())),
            caller_replacement: Set(caller_replacement.map(|s| s.to_string())),
            destination_replacement: Set(destination_replacement.map(|s| s.to_string())),
            direction: Set(direction.to_string()),
            priority: Set(priority),
            is_active: Set(is_active),
            created_at: Set(now),
            updated_at: Set(now),
            account_id: Set("root".to_string()),
        };
        let inserted = am.insert(db).await.expect("insert translation row");
        inserted.id
    }

    fn make_invite(caller_user: &str, callee_user: &str) -> InviteOption {
        let caller_str = format!("sip:{}@example.com", caller_user);
        let callee_str = format!("sip:{}@example.com", callee_user);
        let contact_str = format!("sip:{}@192.168.1.1:5060", caller_user);
        InviteOption {
            caller: Uri::try_from(caller_str.as_str()).expect("caller uri"),
            callee: Uri::try_from(callee_str.as_str()).expect("callee uri"),
            contact: Uri::try_from(contact_str.as_str()).expect("contact uri"),
            ..Default::default()
        }
    }

    fn caller_user(opt: &InviteOption) -> String {
        opt.caller.user().unwrap_or_default().to_string()
    }

    fn callee_user(opt: &InviteOption) -> String {
        opt.callee.user().unwrap_or_default().to_string()
    }

    // D-29 #1: UK normalize on inbound INVITE.
    #[tokio::test]
    async fn uk_normalize_inbound_caller() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-uk",
            "uk-normalize",
            Some(r"^0(\d+)$"),
            Some("+44$1"),
            None,
            None,
            "inbound",
            100,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "callee");
        let trace = engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        assert_eq!(caller_user(&opt), "+442079460123");
        assert_eq!(trace.applied_rules.len(), 1);
        assert_eq!(trace.applied_rules[0].field, "caller");
        assert_eq!(trace.applied_rules[0].before, "02079460123");
        assert_eq!(trace.applied_rules[0].after, "+442079460123");
    }

    // D-29 #2: US normalize on inbound INVITE.
    #[tokio::test]
    async fn us_normalize_inbound_caller() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-us",
            "us-normalize",
            Some(r"^([2-9]\d{9})$"),
            Some("+1$1"),
            None,
            None,
            "inbound",
            100,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("4155551234", "callee");
        let trace = engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        assert_eq!(caller_user(&opt), "+14155551234");
        assert_eq!(trace.applied_rules.len(), 1);
    }

    // D-29 #3 / TRN-05: inbound rule does NOT fire on outbound INVITE.
    #[tokio::test]
    async fn direction_filter_skips_inbound_rule_on_outbound_call() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-uk",
            "uk-normalize",
            Some(r"^0(\d+)$"),
            Some("+44$1"),
            None,
            None,
            "inbound",
            100,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "callee");
        let trace = engine
            .translate(&mut opt, DialDirection::Outbound, &db)
            .await
            .expect("translate");
        // Pattern matches but rule is direction-skipped.
        assert_eq!(caller_user(&opt), "02079460123");
        assert!(trace.applied_rules.is_empty());
    }

    // Both-direction rule fires on outbound INVITE.
    #[tokio::test]
    async fn both_direction_rule_fires_on_outbound() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-both",
            "always",
            Some(r"^0(\d+)$"),
            Some("+44$1"),
            None,
            None,
            "both",
            100,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "callee");
        engine
            .translate(&mut opt, DialDirection::Outbound, &db)
            .await
            .expect("translate");
        assert_eq!(caller_user(&opt), "+442079460123");
    }

    // D-29 #4 / D-08: cascade in priority order — rule 2 sees rule 1's output.
    #[tokio::test]
    async fn cascade_applies_in_priority_order() {
        let db = fresh_db().await;
        // priority 10: strip leading zero
        seed_translation(
            &db,
            "trn-strip",
            "strip-zero",
            Some(r"^0(\d+)$"),
            Some("$1"),
            None,
            None,
            "inbound",
            10,
            true,
        )
        .await;
        // priority 20: prepend +44 (now sees a non-zero-leading number)
        seed_translation(
            &db,
            "trn-prepend",
            "prepend-44",
            Some(r"^(\d+)$"),
            Some("+44$1"),
            None,
            None,
            "inbound",
            20,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "callee");
        let trace = engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        assert_eq!(caller_user(&opt), "+442079460123");
        assert_eq!(trace.applied_rules.len(), 2);
        // Order matches priority ASC: strip first, then prepend.
        assert_eq!(trace.applied_rules[0].rule_id, "trn-strip");
        assert_eq!(trace.applied_rules[0].before, "02079460123");
        assert_eq!(trace.applied_rules[0].after, "2079460123");
        assert_eq!(trace.applied_rules[1].rule_id, "trn-prepend");
        assert_eq!(trace.applied_rules[1].before, "2079460123");
        assert_eq!(trace.applied_rules[1].after, "+442079460123");
    }

    // D-29 #5 / D-05..D-07: per-field independence — caller-only rule leaves
    // destination untouched.
    #[tokio::test]
    async fn per_field_independence_caller_only_leaves_destination_untouched() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-caller-only",
            "caller-only",
            Some(r"^0(\d+)$"),
            Some("+44$1"),
            None,
            None,
            "both",
            100,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "0123456");
        engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        assert_eq!(caller_user(&opt), "+442079460123");
        // Destination URI bytes unchanged — pattern was None for this field.
        assert_eq!(callee_user(&opt), "0123456");
    }

    // Mirror: destination-only rule leaves caller untouched.
    #[tokio::test]
    async fn per_field_independence_destination_only_leaves_caller_untouched() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-dest-only",
            "dest-only",
            None,
            None,
            Some(r"^9(\d+)$"),
            Some("$1"),
            "both",
            100,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("alice", "9123456");
        engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        assert_eq!(caller_user(&opt), "alice");
        assert_eq!(callee_user(&opt), "123456");
    }

    // Pattern set but doesn't match the field → no rewrite, no trace entry.
    #[tokio::test]
    async fn non_matching_pattern_leaves_field_alone() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-no-match",
            "no-match",
            Some(r"^XYZ(\d+)$"),
            Some("matched-$1"),
            None,
            None,
            "inbound",
            100,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "callee");
        let trace = engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        assert_eq!(caller_user(&opt), "02079460123");
        assert!(trace.applied_rules.is_empty());
    }

    // Inactive rule is not applied even when pattern matches.
    #[tokio::test]
    async fn inactive_rule_not_applied() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-inactive",
            "inactive",
            Some(r"^0(\d+)$"),
            Some("+44$1"),
            None,
            None,
            "inbound",
            100,
            false, // is_active=false
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "callee");
        let trace = engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        assert_eq!(caller_user(&opt), "02079460123");
        assert!(trace.applied_rules.is_empty());
    }

    // Cache populated on first match; second call is a cache hit.
    #[tokio::test]
    async fn cache_populated_on_first_call() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-cached",
            "cached",
            Some(r"^0(\d+)$"),
            Some("+44$1"),
            None,
            None,
            "inbound",
            100,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        assert_eq!(engine.cache_len(), 0);

        let mut opt1 = make_invite("02079460123", "callee");
        engine
            .translate(&mut opt1, DialDirection::Inbound, &db)
            .await
            .expect("translate-1");
        assert_eq!(engine.cache_len(), 1, "cache should hold caller regex");
        assert!(engine
            .cache
            .contains_key(&"trn-cached::caller".to_string()));

        // Second invocation: cache hit — size stays 1.
        let mut opt2 = make_invite("02079460456", "callee");
        engine
            .translate(&mut opt2, DialDirection::Inbound, &db)
            .await
            .expect("translate-2");
        assert_eq!(engine.cache_len(), 1, "no recompile on second call");
        assert_eq!(caller_user(&opt2), "+442079460456");
    }

    // invalidate(rule_id) removes both per-field cache entries.
    #[tokio::test]
    async fn invalidate_removes_from_cache() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-inv",
            "inv",
            Some(r"^0(\d+)$"),
            Some("+44$1"),
            Some(r"^9(\d+)$"),
            Some("$1"),
            "inbound",
            100,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "9123456");
        engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        // Both per-field regexes cached.
        assert_eq!(engine.cache_len(), 2);

        engine.invalidate("trn-inv");
        assert_eq!(engine.cache_len(), 0);
        // Idempotent on missing id.
        engine.invalidate("missing");
        assert_eq!(engine.cache_len(), 0);
    }

    // Invalid pattern in DB → skipped with warn (defense in depth — D-19,
    // T-08-03-03). Engine does not panic; valid sibling rules still apply.
    #[tokio::test]
    async fn invalid_pattern_in_db_is_skipped() {
        let db = fresh_db().await;
        // Bad regex (unclosed group) — write-time validation in 08-02
        // normally rejects this; we bypass it by inserting directly.
        seed_translation(
            &db,
            "trn-bad",
            "bad",
            Some(r"^0(\d+$"),
            Some("+44$1"),
            None,
            None,
            "inbound",
            10,
            true,
        )
        .await;
        // A valid sibling rule with higher priority (= later) — must still fire.
        seed_translation(
            &db,
            "trn-good",
            "good",
            Some(r"^0(\d+)$"),
            Some("+44$1"),
            None,
            None,
            "inbound",
            20,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "callee");
        let trace = engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate must not error on bad regex");
        // Only the good rule applied.
        assert_eq!(caller_user(&opt), "+442079460123");
        assert_eq!(trace.applied_rules.len(), 1);
        assert_eq!(trace.applied_rules[0].rule_id, "trn-good");
    }

    // Trace records every applied rule in cascade order.
    #[tokio::test]
    async fn trace_records_each_applied_rule_in_cascade_order() {
        let db = fresh_db().await;
        seed_translation(
            &db,
            "trn-strip",
            "strip-zero",
            Some(r"^0(\d+)$"),
            Some("$1"),
            None,
            None,
            "inbound",
            10,
            true,
        )
        .await;
        seed_translation(
            &db,
            "trn-prepend",
            "prepend-44",
            Some(r"^(\d+)$"),
            Some("+44$1"),
            None,
            None,
            "inbound",
            20,
            true,
        )
        .await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "callee");
        let trace = engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        assert_eq!(trace.applied_rules.len(), 2);
        assert_eq!(trace.applied_rules[0].rule_name, "strip-zero");
        assert_eq!(trace.applied_rules[1].rule_name, "prepend-44");
        assert_eq!(trace.applied_rules[0].field, "caller");
        assert_eq!(trace.applied_rules[1].field, "caller");
    }

    // Empty rule list → no-op trace.
    #[tokio::test]
    async fn empty_rule_list_is_noop() {
        let db = fresh_db().await;
        let engine = TranslationEngine::new();
        let mut opt = make_invite("02079460123", "callee");
        let trace = engine
            .translate(&mut opt, DialDirection::Inbound, &db)
            .await
            .expect("translate");
        assert_eq!(caller_user(&opt), "02079460123");
        assert!(trace.applied_rules.is_empty());
    }

    #[test]
    fn new_engine_has_empty_cache() {
        let engine = TranslationEngine::new();
        assert_eq!(engine.cache_len(), 0);
    }

    #[test]
    fn trace_default_is_empty() {
        let trace = TranslationTrace::default();
        assert!(trace.applied_rules.is_empty());
    }
}
