//! `/api/v1/system/*` — health and reload (Phase 1, Plan 01-05).
//!
//! Two routes per CONTEXT.md SYS-01/SYS-02:
//!
//! - `GET  /api/v1/system/health`  uptime, db_ok, active_calls, version
//! - `POST /api/v1/system/reload`  synchronous reload guard + elapsed_ms
//!
//! The pure fns take `&AppState` because health and reload both span
//! multiple state components (DB + uptime + active call registry for
//! health; the reload guard AtomicBool for reload). CONTEXT.md §"Adapter
//! Pattern" explicitly authorizes `&AppState` for non-DB helpers.
//!
//! Reload is serialized via the existing `reload_requested: AtomicBool`
//! on `AppStateInner`. A second concurrent reload returns 409 Conflict.
//! A `ReloadGuard` with a `Drop` impl releases the flag on both success
//! and error paths so a panic mid-reload cannot leave the flag stuck.
//!
//! Reload executes the trunks / routes / acl steps sequentially via
//! `reload_steps::*`. The `reload/app` step (config-file reload with
//! validation + dry-run) is deferred — see `deferred-items.md`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use chrono::Utc;
use sea_orm::ConnectionTrait;
use serde::Serialize;

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::handler::api_v1::reload_steps::{self, ReloadStepOutcome};

// ---------------------------------------------------------------------------
// Wire types (CONTEXT.md locked shapes)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub uptime_secs: u64,
    pub db_ok: bool,
    pub active_calls: u64,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct ReloadResponse {
    /// Legacy Phase-1 field — list of step names that succeeded.
    /// Kept for backwards compatibility with any client that locked
    /// this shape in the stub era. Mirrors `steps[].step`.
    pub reloaded: Vec<&'static str>,

    /// New in 01-06 — per-step outcome with elapsed_ms and changed_count.
    pub steps: Vec<ReloadStepOutcome>,

    /// Total elapsed_ms across all steps.
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/system/health", get(handle_health))
        .route("/system/reload", post(handle_reload))
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

async fn handle_health(State(state): State<AppState>) -> ApiResult<Json<HealthResponse>> {
    Ok(Json(health_snapshot(&state).await))
}

pub(crate) async fn health_snapshot(state: &AppState) -> HealthResponse {
    // Uptime from the existing DateTime<Utc> field on AppStateInner.
    let uptime_secs = (Utc::now() - state.uptime).num_seconds().max(0) as u64;

    // DB probe — wrap in a 250ms timeout so a hung DB never blocks the
    // health route. Any error or timeout yields db_ok=false but the
    // endpoint still returns 200 (health is informative, not a gate).
    let db_ok = match tokio::time::timeout(
        Duration::from_millis(250),
        state
            .db()
            .execute_unprepared("SELECT 1"),
    )
    .await
    {
        Ok(Ok(_)) => true,
        _ => false,
    };

    // Active calls from the proxy registry; counted best-effort.
    // In the test harness (skip_sip_bind=true) the registry exists but is
    // empty, so count() returns 0. No panics if the count() accessor
    // returns usize — we widen to u64.
    let active_calls: u64 = state
        .sip_server()
        .inner
        .active_call_registry
        .count()
        .try_into()
        .unwrap_or(0);

    let version = env!("CARGO_PKG_VERSION").to_string();

    HealthResponse {
        uptime_secs,
        db_ok,
        active_calls,
        version,
    }
}

// ---------------------------------------------------------------------------
// Reload
// ---------------------------------------------------------------------------

/// RAII guard that releases the reload flag on drop.
///
/// Holding this struct means `reload_requested` is currently `true`. When
/// the guard is dropped — on success, error, OR panic — it sets the flag
/// back to `false` so a subsequent reload can proceed. This is critical:
/// a panic inside a reload step without the guard would leave the flag
/// stuck at `true` and every future reload would return 409 until the
/// process restarts.
struct ReloadGuard<'a>(&'a AtomicBool);

impl<'a> Drop for ReloadGuard<'a> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

async fn handle_reload(State(state): State<AppState>) -> ApiResult<Json<ReloadResponse>> {
    reload_all(&state).await.map(Json)
}

pub(crate) async fn reload_all(state: &AppState) -> ApiResult<ReloadResponse> {
    // Serialize — CAS false -> true. If it fails, another reload is in
    // flight; return 409 rather than running a second one concurrently.
    if state
        .reload_requested
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(ApiError::conflict("reload already in progress"));
    }
    let _guard = ReloadGuard(&state.reload_requested);

    let overall_start = Instant::now();
    let mut steps: Vec<ReloadStepOutcome> = Vec::with_capacity(3);

    // Trunks first — routes depend on fresh trunk ids for fk validation.
    steps.push(reload_steps::reload_trunks_step(state).await?);

    // Routes after trunks.
    steps.push(reload_steps::reload_routes_step(state).await?);

    // ACL last — independent of the other two, runs synchronously.
    steps.push(reload_steps::reload_acl_step(state).await?);

    let reloaded: Vec<&'static str> = steps.iter().map(|s| s.step).collect();

    Ok(ReloadResponse {
        reloaded,
        steps,
        elapsed_ms: overall_start.elapsed().as_millis() as u64,
    })
}
