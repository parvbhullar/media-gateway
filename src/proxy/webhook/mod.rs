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
pub use processor::{deliver_test_event, run_webhook_processor};

use serde::Serialize;

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

/// Build a fresh `evt_<uuid-v4>` identifier (D-07).
pub fn new_event_id() -> String {
    format!("evt_{}", uuid::Uuid::new_v4())
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
}
