//! Per-step reload helpers extracted from `handler/ami.rs` so
//! `POST /api/v1/system/reload` (SYS-02) can invoke them sequentially.
//!
//! The legacy `/ami/reload/{trunks,routes,acl,app}` endpoints remain
//! unchanged — this module only lifts their business logic into pure
//! `pub(crate) async fn` helpers returning structured outcomes, so the
//! API-v1 reload path can report per-step `{step, elapsed_ms, changed_count}`
//! instead of the Phase-1 stub that returned only a hardcoded name list.

use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use thiserror::Error;

use crate::app::AppState;
use crate::config::{Config, ProxyConfig};

#[derive(Debug, Serialize)]
pub struct ReloadStepOutcome {
    pub step: &'static str,
    pub elapsed_ms: u64,
    pub changed_count: u64,
}

#[derive(Debug, Error)]
pub enum ReloadStepError {
    #[error("reload step '{step}' failed: {source}")]
    Underlying {
        step: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("config override load failed: {0}")]
    ConfigOverride(String),
}

/// Build the optional `ProxyConfig` override identically to
/// `handler::ami::load_proxy_config_override` so the per-step helpers
/// can be called independently of the axum `Response` return shape
/// that `ami.rs` uses.
fn load_proxy_config_override(
    state: &AppState,
) -> Result<Option<Arc<ProxyConfig>>, ReloadStepError> {
    let Some(path) = state.config_path.as_ref() else {
        return Ok(None);
    };
    match Config::load(path) {
        Ok(cfg) => Ok(Some(Arc::new(cfg.proxy))),
        Err(err) => Err(ReloadStepError::ConfigOverride(err.to_string())),
    }
}

pub(crate) async fn reload_trunks_step(
    state: &AppState,
) -> Result<ReloadStepOutcome, ReloadStepError> {
    let start = Instant::now();
    let config_override = load_proxy_config_override(state)?;
    let metrics = state
        .sip_server()
        .inner
        .data_context
        .reload_trunks(true, config_override)
        .await
        .map_err(|e| ReloadStepError::Underlying {
            step: "trunks",
            source: anyhow::anyhow!(e.to_string()),
        })?;

    // Match the side-effects of the legacy `/ami/reload/trunks` handler.
    state
        .sip_server()
        .inner
        .data_context
        .reload_did_index()
        .await;
    #[cfg(feature = "console")]
    if let Some(ref console) = state.console {
        console.clear_pending_reload();
    }

    Ok(ReloadStepOutcome {
        step: "trunks",
        elapsed_ms: start.elapsed().as_millis() as u64,
        changed_count: metrics.total as u64,
    })
}

pub(crate) async fn reload_routes_step(
    state: &AppState,
) -> Result<ReloadStepOutcome, ReloadStepError> {
    let start = Instant::now();
    let config_override = load_proxy_config_override(state)?;
    let metrics = state
        .sip_server()
        .inner
        .data_context
        .reload_routes(true, config_override)
        .await
        .map_err(|e| ReloadStepError::Underlying {
            step: "routes",
            source: anyhow::anyhow!(e.to_string()),
        })?;

    #[cfg(feature = "console")]
    if let Some(ref console) = state.console {
        console.clear_pending_reload();
    }

    Ok(ReloadStepOutcome {
        step: "routes",
        elapsed_ms: start.elapsed().as_millis() as u64,
        changed_count: metrics.total as u64,
    })
}

pub(crate) async fn reload_acl_step(
    state: &AppState,
) -> Result<ReloadStepOutcome, ReloadStepError> {
    let start = Instant::now();
    let config_override = load_proxy_config_override(state)?;
    // NOTE: reload_acl_rules is synchronous in data_context — no .await.
    let metrics = state
        .sip_server()
        .inner
        .data_context
        .reload_acl_rules(true, config_override)
        .map_err(|e| ReloadStepError::Underlying {
            step: "acl",
            source: anyhow::anyhow!(e.to_string()),
        })?;

    Ok(ReloadStepOutcome {
        step: "acl",
        elapsed_ms: start.elapsed().as_millis() as u64,
        changed_count: metrics.total as u64,
    })
}
