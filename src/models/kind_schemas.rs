//! Kind-schema validator registry for the unified `trunks` table.
//!
//! See plan: `/home/anuj/.claude/plans/imperative-sauteeing-cake.md` (Phase 3).
//!
//! Each trunk `kind` (`"sip"`, `"webrtc"`, future kinds) registers a
//! `KindValidator` closure that takes a `serde_json::Value` representing a
//! `kind_config` blob and either returns `Ok(())` or a structured
//! `KindValidationError`. The CRUD layer (REST `/api/v1/gateways`, file-based
//! trunk loader) calls [`validate`] as its single validation gate so that
//! adding a new kind only requires registering a new validator — no further
//! changes to call sites.
//!
//! Future kinds (PR 3+) register their own validators at startup via
//! [`register`]; the WebRTC validator here is a stub that will delegate
//! protocol validation to a signaling-adapter registry once that module
//! lands in PR 3.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use serde_json::Value;

use crate::models::trunk::{SipTrunkConfig, WebRtcTrunkConfig};

#[derive(Debug, thiserror::Error)]
pub enum KindValidationError {
    #[error("unknown trunk kind '{0}'")]
    UnknownKind(String),
    #[error("missing kind_config for kind '{0}'")]
    MissingConfig(String),
    #[error("invalid kind_config for kind '{kind}': {message}")]
    Invalid { kind: String, message: String },
}

impl KindValidationError {
    pub fn invalid(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid {
            kind: kind.into(),
            message: message.into(),
        }
    }
}

pub type KindValidator = Arc<dyn Fn(&Value) -> Result<(), KindValidationError> + Send + Sync>;

static REGISTRY: OnceLock<RwLock<HashMap<String, KindValidator>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, KindValidator>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register (or replace) the validator for `kind`. Idempotent — calling
/// twice with the same kind simply replaces the prior entry.
pub fn register(kind: impl Into<String>, validator: KindValidator) {
    let kind = kind.into();
    let mut guard = registry()
        .write()
        .expect("kind_schemas registry RwLock poisoned");
    guard.insert(kind, validator);
}

/// Look up the validator for `kind`, returning a clone of the `Arc`.
pub fn lookup(kind: &str) -> Option<KindValidator> {
    let guard = registry()
        .read()
        .expect("kind_schemas registry RwLock poisoned");
    guard.get(kind).cloned()
}

/// Snapshot of all currently-registered kind names. Order is unspecified.
pub fn registered_kinds() -> Vec<String> {
    let guard = registry()
        .read()
        .expect("kind_schemas registry RwLock poisoned");
    guard.keys().cloned().collect()
}

/// Validate a `kind_config` blob against the registered validator for the
/// given kind. Returns [`KindValidationError::UnknownKind`] when no
/// validator is registered for `kind`.
pub fn validate(kind: &str, config: &Value) -> Result<(), KindValidationError> {
    let validator =
        lookup(kind).ok_or_else(|| KindValidationError::UnknownKind(kind.to_string()))?;
    validator(config)
}

/// One-time registration of built-in kinds (`"sip"`, `"webrtc"`). Called
/// from process-wide startup. Idempotent — safe to call multiple times.
pub fn register_builtins() {
    register(
        "sip",
        Arc::new(|v: &Value| -> Result<(), KindValidationError> {
            let cfg: SipTrunkConfig = serde_json::from_value(v.clone())
                .map_err(|e| KindValidationError::invalid("sip", e.to_string()))?;
            cfg.validate()
                .map_err(|e| KindValidationError::invalid("sip", e.to_string()))?;
            Ok(())
        }),
    );

    register(
        "webrtc",
        Arc::new(|v: &Value| -> Result<(), KindValidationError> {
            let cfg: WebRtcTrunkConfig = serde_json::from_value(v.clone())
                .map_err(|e| KindValidationError::invalid("webrtc", e.to_string()))?;
            cfg.validate()
                .map_err(|e| KindValidationError::invalid("webrtc", e.to_string()))?;
            // Delegate protocol-blob validation to the signaling adapter
            // named by the trunk's `signaling` field. The adapter registry
            // is populated at startup (`signaling::register_builtins`) and
            // is process-global; tests that exercise the `webrtc` validator
            // also call `register_builtins` so the adapter is reachable.
            let adapter = crate::proxy::bridge::signaling::lookup(&cfg.signaling).ok_or_else(
                || {
                    KindValidationError::invalid(
                        "webrtc",
                        format!("signaling adapter '{}' not registered", cfg.signaling),
                    )
                },
            )?;
            adapter
                .validate_protocol(cfg.protocol.as_ref())
                .map_err(|e| KindValidationError::invalid("webrtc", e.to_string()))?;
            Ok(())
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn register_builtins_registers_sip_and_webrtc() {
        register_builtins();
        assert!(lookup("sip").is_some(), "sip validator should be registered");
        assert!(
            lookup("webrtc").is_some(),
            "webrtc validator should be registered"
        );
        let kinds = registered_kinds();
        assert!(kinds.iter().any(|k| k == "sip"));
        assert!(kinds.iter().any(|k| k == "webrtc"));
    }

    #[test]
    fn register_builtins_is_idempotent() {
        register_builtins();
        register_builtins();
        // No panic, and both kinds still resolve.
        assert!(lookup("sip").is_some());
        assert!(lookup("webrtc").is_some());
    }

    #[test]
    fn validate_sip_accepts_valid_config() {
        register_builtins();
        let cfg = json!({
            "sip_server": "sip.example.com:5060",
            "sip_transport": "udp",
        });
        assert!(validate("sip", &cfg).is_ok());
    }

    #[test]
    fn validate_sip_rejects_invalid_field() {
        register_builtins();
        let cfg = json!({
            "sip_server": "sip.example.com",
            "sip_transport": "carrier-pigeon",
        });
        match validate("sip", &cfg) {
            Err(KindValidationError::Invalid { kind, .. }) => assert_eq!(kind, "sip"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn validate_webrtc_accepts_valid_config() {
        // The webrtc validator now delegates protocol validation to the
        // adapter named by `signaling`, so we must register adapters too.
        crate::proxy::bridge::signaling::register_builtins();
        register_builtins();
        let cfg = json!({
            "signaling": "http_json",
            "endpoint_url": "https://signal.example.com/offer",
            "protocol": {
                "request_body_template": r#"{"sdp":"{offer_sdp}","type":"offer"}"#,
                "response_answer_path": "$.sdp",
            },
        });
        assert!(validate("webrtc", &cfg).is_ok());
    }

    #[test]
    fn validate_webrtc_rejects_unknown_signaling_adapter() {
        crate::proxy::bridge::signaling::register_builtins();
        register_builtins();
        let cfg = json!({
            "signaling": "nonexistent_adapter_xyz",
            "endpoint_url": "https://signal.example.com/offer",
        });
        match validate("webrtc", &cfg) {
            Err(KindValidationError::Invalid { kind, message }) => {
                assert_eq!(kind, "webrtc");
                assert!(message.contains("nonexistent_adapter_xyz"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn validate_webrtc_rejects_missing_protocol_for_http_json() {
        crate::proxy::bridge::signaling::register_builtins();
        register_builtins();
        let cfg = json!({
            "signaling": "http_json",
            "endpoint_url": "https://signal.example.com/offer",
        });
        match validate("webrtc", &cfg) {
            Err(KindValidationError::Invalid { kind, message }) => {
                assert_eq!(kind, "webrtc");
                assert!(
                    message.contains("missing protocol") || message.contains("protocol"),
                    "expected protocol-related error, got: {message}"
                );
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn validate_webrtc_rejects_missing_signaling() {
        register_builtins();
        let cfg = json!({
            "signaling": "",
            "endpoint_url": "https://signal.example.com/offer",
        });
        match validate("webrtc", &cfg) {
            Err(KindValidationError::Invalid { kind, message }) => {
                assert_eq!(kind, "webrtc");
                assert!(
                    message.contains("signaling"),
                    "expected message to mention signaling, got: {message}"
                );
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn validate_unknown_kind() {
        register_builtins();
        let cfg = json!({});
        match validate("frobnicate", &cfg) {
            Err(KindValidationError::UnknownKind(k)) => assert_eq!(k, "frobnicate"),
            other => panic!("expected UnknownKind, got {other:?}"),
        }
    }
}
