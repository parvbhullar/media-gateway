//! WebRTC signaling adapter trait + process-global registry.
//!
//! See plan: `/home/anuj/.claude/plans/imperative-sauteeing-cake.md` (Phase 4).
//!
//! Each adapter knows how to drive the offer/answer exchange with a specific
//! signaling dialect (HTTP+JSON in v1; future: WHIP, custom token-dance
//! protocols, etc.). The dispatcher resolves the adapter by name via the
//! trunk's `signaling` field and calls into it through this trait.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use serde_json::Value;

pub mod http_json;

/// Context handed to an adapter for a single negotiate call. Built by the
/// dispatcher from the trunk's WebRTC config.
#[derive(Clone, Debug)]
pub struct SignalingContext {
    pub endpoint_url: String,
    pub auth_header: Option<String>,
    pub timeout_ms: u64,
    /// Adapter-specific configuration blob from the trunk's `kind_config`.
    pub protocol: Option<Value>,
}

/// Outcome of a successful negotiate call.
#[derive(Debug, Clone)]
pub struct NegotiateOutcome {
    /// SDP answer returned by the remote signaling peer.
    pub answer_sdp: String,
    /// Opaque adapter-defined session handle (echoed back on close).
    pub session: SessionHandle,
}

/// Opaque adapter-defined session data, carried back to `close`.
#[derive(Debug, Clone)]
pub struct SessionHandle(pub Value);

#[derive(Debug, thiserror::Error)]
pub enum SignalingError {
    #[error("missing protocol config for adapter '{0}'")]
    MissingProtocol(String),
    #[error("invalid protocol config: {0}")]
    InvalidProtocol(String),
    #[error("signaling request failed: {0}")]
    Transport(String),
    #[error("invalid response from signaling endpoint: {0}")]
    InvalidResponse(String),
}

/// Trait implemented by each WebRTC signaling adapter.
///
/// Adapters are stateless beyond the per-trunk configuration handed in via
/// [`SignalingContext`] — one adapter instance services many trunks.
#[async_trait]
pub trait WebRtcSignalingAdapter: Send + Sync {
    /// Validate the per-trunk `protocol` blob at CRUD/load time, before any
    /// call setup. Default: no-op (adapter doesn't need protocol config).
    fn validate_protocol(&self, _protocol: Option<&Value>) -> Result<(), SignalingError> {
        Ok(())
    }

    /// Drive the offer→answer exchange. The adapter consumes `offer_sdp`,
    /// performs whatever signaling its dialect prescribes, and returns the
    /// resulting SDP answer plus an opaque session handle.
    async fn negotiate(
        &self,
        ctx: &SignalingContext,
        offer_sdp: &str,
    ) -> Result<NegotiateOutcome, SignalingError>;

    /// Tear down the session at the remote signaling peer. Default no-op
    /// because most stateless dialects (e.g. HTTP+JSON offer/answer) have no
    /// teardown step.
    async fn close(
        &self,
        _ctx: &SignalingContext,
        _session: &SessionHandle,
    ) -> Result<(), SignalingError> {
        Ok(())
    }
}

type AdapterMap = HashMap<String, Arc<dyn WebRtcSignalingAdapter>>;

static REGISTRY: OnceLock<RwLock<AdapterMap>> = OnceLock::new();

fn registry() -> &'static RwLock<AdapterMap> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register (or replace) the adapter for `name`. Idempotent — repeated
/// registration overwrites the prior entry.
pub fn register(name: impl Into<String>, adapter: Arc<dyn WebRtcSignalingAdapter>) {
    let name = name.into();
    let mut guard = registry()
        .write()
        .expect("signaling registry RwLock poisoned");
    guard.insert(name, adapter);
}

/// Look up the adapter registered for `name`, cloning the `Arc`.
pub fn lookup(name: &str) -> Option<Arc<dyn WebRtcSignalingAdapter>> {
    let guard = registry()
        .read()
        .expect("signaling registry RwLock poisoned");
    guard.get(name).cloned()
}

/// Snapshot of all currently-registered adapter names. Order unspecified.
pub fn registered() -> Vec<String> {
    let guard = registry()
        .read()
        .expect("signaling registry RwLock poisoned");
    guard.keys().cloned().collect()
}

/// One-time registration of built-in adapters (`"http_json"`). Idempotent.
/// Called from process startup in `app.rs`.
pub fn register_builtins() {
    register(
        "http_json",
        Arc::new(http_json::HttpJsonAdapter::new()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyAdapter;
    #[async_trait]
    impl WebRtcSignalingAdapter for DummyAdapter {
        async fn negotiate(
            &self,
            _ctx: &SignalingContext,
            _offer_sdp: &str,
        ) -> Result<NegotiateOutcome, SignalingError> {
            Ok(NegotiateOutcome {
                answer_sdp: "v=0\r\n".to_string(),
                session: SessionHandle(Value::Null),
            })
        }
    }

    #[test]
    fn register_and_lookup_roundtrip() {
        register("dummy_test_adapter", Arc::new(DummyAdapter));
        assert!(lookup("dummy_test_adapter").is_some());
        assert!(registered().iter().any(|n| n == "dummy_test_adapter"));
    }

    #[test]
    fn register_builtins_idempotent() {
        register_builtins();
        register_builtins();
        assert!(lookup("http_json").is_some());
    }

    #[test]
    fn register_builtins_registers_http_json() {
        register_builtins();
        assert!(
            lookup("http_json").is_some(),
            "http_json adapter must be registered by register_builtins()"
        );
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("definitely_not_registered_xyz").is_none());
    }
}
