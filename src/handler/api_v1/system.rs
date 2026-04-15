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
//! Phase 1 ships a minimal `reload_all` that flips the guard, sleeps
//! briefly, and returns the elapsed_ms. Wiring it to the actual AMI
//! reload subsystems (trunks / routes / acl / app) is deferred — the
//! locked response shape is `{reloaded, elapsed_ms}` and this plan
//! honors it exactly with a no-op `reloaded` list of the 4 steps so
//! the contract is stable for later plans to fill in.

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
    pub reloaded: Vec<&'static str>,
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

    let start = Instant::now();

    // Phase 1 stub: record the 4 steps but do not hook the actual AMI
    // reload subsystems. The locked response shape is {reloaded,
    // elapsed_ms}; a later plan will replace this no-op body with real
    // calls into the trunks / routes / acl / app reload paths.
    let reloaded = vec!["trunks", "routes", "acl", "app"];

    Ok(ReloadResponse {
        reloaded,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}
