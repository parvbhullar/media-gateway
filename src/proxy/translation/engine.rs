//! `TranslationEngine` — Phase 8 number-translation runtime (Plan 08-01 stub).
//!
//! Wave-1 scope: types, signatures, and a no-op body wired into AppState.
//! Wave-2 (08-03) replaces the `translate` body with real logic — DB query,
//! priority-ordered match, regex compile-cache (per-rule by id), and
//! `update_uri_user` rewrite reusing `src/proxy/routing/matcher.rs`.
//!
//! Shape locked by CONTEXT.md decisions:
//!   - D-12: engine surface — `translate(&mut InviteOption, direction, db)`
//!   - D-13: returns a `TranslationTrace` for downstream observability
//!   - D-20: per-rule compiled-regex cache, invalidated on PUT/DELETE
//!   - D-22: `direction` argument is the runtime `DialDirection` enum;
//!     mapping to `RoutingDirection` happens INSIDE the engine (08-03).

use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use regex::Regex;
use rsipstack::dialog::invitation::InviteOption;
use sea_orm::DatabaseConnection;

use crate::call::DialDirection;

/// Number-translation runtime. Cheap to clone via `Arc` — the engine itself
/// holds only an `Arc<DashMap>` cache, so cloning is a refcount bump.
///
/// Wave-1 stub: `translate` is a no-op returning an empty trace, and
/// `invalidate` simply removes a cached compiled regex. Real bodies land in
/// 08-03.
pub struct TranslationEngine {
    /// Per-rule compiled-regex cache (D-20). Key is the rule id (UUID v4
    /// string). 08-02 PUT/DELETE handlers call `invalidate(id)` so the next
    /// `translate` call recompiles from the new pattern.
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
    /// Wave-1 stub: returns an empty trace and mutates nothing. The real
    /// implementation in 08-03 will:
    ///   1. Query `supersip_translations` ordered by priority ASC
    ///   2. Filter by direction (matching variant OR "both")
    ///   3. For each matching rule, compile or fetch regex from cache
    ///   4. Apply caller / destination rewrites via the matcher helpers
    ///   5. Record each applied rule in the trace
    pub async fn translate(
        &self,
        invite_option: &mut InviteOption,
        direction: DialDirection,
        db: &DatabaseConnection,
    ) -> Result<TranslationTrace> {
        // TODO(08-03): real implementation. Currently a no-op so the call
        // site (08-04) and CRUD handlers (08-02) can compile against the
        // engine surface.
        let _ = (invite_option, direction, db);
        Ok(TranslationTrace::default())
    }

    /// Drop a single rule's compiled regex from the cache (D-20). Called by
    /// the 08-02 PUT/DELETE handlers so the next `translate` invocation
    /// recompiles from the freshly-stored pattern.
    pub fn invalidate(&self, rule_id: &str) {
        self.cache.remove(rule_id);
    }

    /// Test-only accessor for the cache size — used by 08-03 tests.
    #[cfg(test)]
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }
}

impl Default for TranslationEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-call audit trail of which translation rules fired (D-13).
#[derive(Debug, Default)]
pub struct TranslationTrace {
    pub applied_rules: Vec<AppliedRule>,
}

/// One entry of the trace — emitted by 08-03 when a rule actually mutates a
/// field. `before`/`after` carry the user-portion of the SIP URI before and
/// after rewrite (matcher contract — see `src/proxy/routing/matcher.rs`).
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

    #[test]
    fn new_engine_has_empty_cache() {
        let engine = TranslationEngine::new();
        assert_eq!(engine.cache_len(), 0);
    }

    #[test]
    fn invalidate_removes_only_target_id() {
        let engine = TranslationEngine::new();
        // Seed two cached regexes by hand.
        engine
            .cache
            .insert("a".to_string(), Arc::new(Regex::new(r"^a$").unwrap()));
        engine
            .cache
            .insert("b".to_string(), Arc::new(Regex::new(r"^b$").unwrap()));
        assert_eq!(engine.cache_len(), 2);

        engine.invalidate("a");
        assert_eq!(engine.cache_len(), 1);
        assert!(engine.cache.contains_key("b"));

        // Invalidating a missing id is a no-op.
        engine.invalidate("missing");
        assert_eq!(engine.cache_len(), 1);
    }

    #[test]
    fn trace_default_is_empty() {
        let trace = TranslationTrace::default();
        assert!(trace.applied_rules.is_empty());
    }
}
