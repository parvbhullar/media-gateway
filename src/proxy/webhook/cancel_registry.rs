//! In-memory cancel registry for in-flight webhook deliveries (WH-06).
//!
//! Per D-31: DELETE /webhooks/{id} cancels in-flight retries by triggering
//! the registered CancellationToken. Per D-34: PUT /webhooks/{id} replaces
//! the prior token (cancelling it first) so the new config takes effect on
//! the next retry attempt. Per D-33 persistence is explicitly out-of-scope:
//! the registry lives in process memory only and is reset on restart.

use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct WebhookCancelRegistry {
    inner: DashMap<String, CancellationToken>,
}

impl WebhookCancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a fresh CancellationToken for `id`. If an entry already exists
    /// (PUT-replace per D-34), the prior token is cancelled before the new
    /// one is stored so any in-flight retry loop observing it bails out.
    pub fn insert(&self, id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        if let Some((_, prior)) = self.inner.remove(id) {
            prior.cancel();
        }
        self.inner.insert(id.to_string(), token.clone());
        token
    }

    /// Cancel the stored token for `id` and remove the entry. No-op if
    /// missing. Per D-31 — DELETE handler triggers this.
    pub fn cancel(&self, id: &str) {
        if let Some((_, token)) = self.inner.remove(id) {
            token.cancel();
        }
    }

    /// Remove without cancelling. Used after a delivery completes naturally
    /// so the entry doesn't leak between events for the same webhook id.
    pub fn remove(&self, id: &str) {
        self.inner.remove(id);
    }

    /// Test/observability helper.
    pub fn contains_key(&self, id: &str) -> bool {
        self.inner.contains_key(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::task::JoinSet;

    #[test]
    fn insert_returns_active_token() {
        let registry = WebhookCancelRegistry::new();
        let token = registry.insert("wh-1");
        assert!(!token.is_cancelled(), "freshly inserted token must be active");
        assert!(registry.contains_key("wh-1"));
    }

    #[test]
    fn cancel_marks_token_cancelled_and_removes_entry() {
        let registry = WebhookCancelRegistry::new();
        let token = registry.insert("wh-1");
        registry.cancel("wh-1");
        assert!(token.is_cancelled(), "cancelled token must report is_cancelled");
        assert!(!registry.contains_key("wh-1"), "cancel must remove the entry");
    }

    #[test]
    fn cancel_missing_id_is_silent_noop() {
        let registry = WebhookCancelRegistry::new();
        registry.cancel("does-not-exist"); // must not panic
        assert!(!registry.contains_key("does-not-exist"));
    }

    #[test]
    fn put_replace_cancels_prior_and_returns_fresh_token() {
        let registry = WebhookCancelRegistry::new();
        let t1 = registry.insert("wh-1");
        let t2 = registry.insert("wh-1");
        assert!(t1.is_cancelled(), "prior token must be cancelled (D-34)");
        assert!(!t2.is_cancelled(), "replacement token must be active");
        assert!(registry.contains_key("wh-1"));
        assert_eq!(registry.inner.len(), 1, "exactly one entry per id");
    }

    #[test]
    fn remove_does_not_cancel_token() {
        let registry = WebhookCancelRegistry::new();
        let token = registry.insert("wh-1");
        registry.remove("wh-1");
        assert!(!token.is_cancelled(), "remove must not cancel (post-success cleanup)");
        assert!(!registry.contains_key("wh-1"));
    }

    #[test]
    fn contains_key_reflects_lifecycle() {
        let registry = WebhookCancelRegistry::new();
        assert!(!registry.contains_key("wh-1"));
        registry.insert("wh-1");
        assert!(registry.contains_key("wh-1"));
        registry.cancel("wh-1");
        assert!(!registry.contains_key("wh-1"));
        registry.insert("wh-2");
        registry.remove("wh-2");
        assert!(!registry.contains_key("wh-2"));
    }

    #[tokio::test]
    async fn concurrent_inserts_on_distinct_ids_all_observable() {
        let registry = Arc::new(WebhookCancelRegistry::new());
        let mut set: JoinSet<()> = JoinSet::new();
        for i in 0..100 {
            let r = Arc::clone(&registry);
            set.spawn(async move {
                let _ = r.insert(&format!("wh-{i}"));
            });
        }
        while let Some(res) = set.join_next().await {
            res.expect("task did not panic");
        }
        assert_eq!(registry.inner.len(), 100, "all 100 inserts must land");
        for i in 0..100 {
            assert!(registry.contains_key(&format!("wh-{i}")));
        }
    }
}
