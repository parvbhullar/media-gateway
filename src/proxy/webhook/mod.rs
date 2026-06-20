//! Phase 7 — webhook pipeline runtime surface (WH-01..WH-06).
//!
//! Plan 07-01 ships the type aliases + module gateway only. Bodies land in:
//!   - `signer.rs`           → 07-03 (HMAC-SHA256 Stripe-style)
//!   - `cancel_registry.rs`  → 07-03 (DashMap of in-flight tokens)
//!   - `processor.rs`        → 07-04 (DB read + retry + disk fallback)
//!
//! `WebhookEvent` shape is locked by 07-CONTEXT.md D-07 (Stripe-style
//! envelope: event_id, event, timestamp, data). The broadcast channel is
//! constructed at server boot in `src/proxy/server.rs` with capacity
//! 1024 (D-11; mirrors the locator_webhook precedent).

pub mod cancel_registry;
pub mod processor;
pub mod signer;

pub use cancel_registry::WebhookCancelRegistry;
pub use processor::{
    deliver_test_event, run_webhook_processor, run_webhook_redelivery_worker,
};

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Stripe-style envelope (D-07). `data` carries the per-event payload
/// (CallRecord JSON, recording metadata, etc. — locked per D-07).
#[derive(Clone, Debug, Serialize)]
pub struct WebhookEvent {
    pub event_id: String,
    pub event: String,
    pub timestamp: i64,
    pub data: serde_json::Value,
}

/// Broadcast sender plumbed into AppState. Emit sites never import the
/// webhook module's internals — they just call `state.webhook_sender()
/// .send(event)`.
pub type WebhookEventSender = tokio::sync::broadcast::Sender<WebhookEvent>;

/// Build a fresh `evt_<uuid-v4>` identifier (D-07). Use only for events with no
/// call/session correlation (e.g. `webhook.test`); call-lifecycle events use
/// [`derive_event_id`] so they are idempotent across redelivery.
pub fn new_event_id() -> String {
    format!("evt_{}", uuid::Uuid::new_v4())
}

/// Derive a **stable, idempotent** `evt_<sha256-hex>` id from a correlation id
/// and the event name (task 2.2).
///
/// Unlike [`new_event_id`], the same `(correlation_id, event)` pair always
/// yields the same id, so a redelivered or re-emitted event carries the id its
/// first emission produced — letting the downstream receiver (unpod) dedup
/// reliably on `event_id`. `correlation_id` is the `CallRecord.call_id` for
/// `call.completed` and the session id for the session-scoped events
/// (`call.started`, `call.failed`, `recording.completed`, `transcribe.requested`).
/// The event name is folded into the hash so the several events sharing one
/// call/session each get a distinct (but per-event stable) id.
pub fn derive_event_id(correlation_id: &str, event: &str) -> String {
    let mut h = Sha256::new();
    h.update(correlation_id.as_bytes());
    h.update(b"|");
    h.update(event.as_bytes());
    let hex: String = h.finalize().iter().map(|b| format!("{:02x}", b)).collect();
    format!("evt_{hex}")
}

/// Current Unix timestamp in seconds (D-07 `timestamp` field).
pub fn current_unix_timestamp() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_event_id_has_evt_prefix() {
        let id = new_event_id();
        assert!(id.starts_with("evt_"), "expected evt_ prefix, got {id}");
        let stripped = id.trim_start_matches("evt_");
        uuid::Uuid::parse_str(stripped).expect("UUID v4 after evt_ prefix");
    }

    #[test]
    fn current_unix_timestamp_is_positive() {
        assert!(current_unix_timestamp() > 1_700_000_000);
    }

    #[test]
    fn derive_event_id_is_stable_for_same_inputs() {
        // The whole point: a redelivery reproduces the same id (dedup key).
        let a = derive_event_id("call-123", "call.completed");
        let b = derive_event_id("call-123", "call.completed");
        assert_eq!(a, b);
        assert!(a.starts_with("evt_"), "expected evt_ prefix, got {a}");
    }

    #[test]
    fn derive_event_id_differs_by_event_name() {
        // Same call/session, different lifecycle event → distinct ids.
        assert_ne!(
            derive_event_id("sess-1", "call.started"),
            derive_event_id("sess-1", "call.completed"),
        );
    }

    #[test]
    fn derive_event_id_differs_by_correlation_id() {
        assert_ne!(
            derive_event_id("call-a", "call.completed"),
            derive_event_id("call-b", "call.completed"),
        );
    }

    #[test]
    fn derive_event_id_is_evt_prefixed_64_hex() {
        let id = derive_event_id("x", "call.completed");
        let stripped = id.trim_start_matches("evt_");
        assert_eq!(stripped.len(), 64, "sha256 hex is 64 chars");
        assert!(stripped.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
