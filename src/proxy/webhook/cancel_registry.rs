//! In-memory cancel registry for in-flight webhook deliveries (WH-06).
//!
//! Phase 7 Plan 07-01 ships the struct + signatures. Bodies land in 07-03
//! (D-31..D-34): DELETE /webhooks/{id} cancels the matching token; PUT
//! replaces it (cancels the prior in-flight retries because config has
//! changed). Persistence across restarts is explicitly out-of-scope (D-33).

use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct WebhookCancelRegistry {
    #[allow(dead_code)] // wired up in 07-03
    inner: DashMap<String, CancellationToken>,
}

impl WebhookCancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) a CancellationToken for the given webhook id
    /// and return it. Replacing automatically cancels the prior token
    /// (D-34) — body in 07-03.
    pub fn insert(&self, _id: &str) -> CancellationToken {
        unimplemented!("Phase 7 Plan 07-03 lands the body")
    }

    /// Cancel the token registered for this webhook id (no-op if absent).
    /// Body in 07-03.
    pub fn cancel(&self, _id: &str) {
        unimplemented!("Phase 7 Plan 07-03 lands the body")
    }

    /// Drop the entry for this webhook id without cancelling. Used after
    /// a delivery completes successfully. Body in 07-03.
    pub fn remove(&self, _id: &str) {
        unimplemented!("Phase 7 Plan 07-03 lands the body")
    }
}
