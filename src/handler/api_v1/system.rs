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

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use chrono::Utc;
use sea_orm::{ConnectionTrait, EntityTrait};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::handler::api_v1::reload_steps::{self, ReloadStepOutcome};
use crate::models::system_config::Entity as SystemConfigEntity;
use crate::version::{SHORT_VERSION, VERSION_INFO};

// ---------------------------------------------------------------------------
// SAFE_PROXY_FIELDS — Phase 11 D-02/D-03
// ---------------------------------------------------------------------------
//
// Explicit allowlist of `ProxyConfig` fields exposed via
// `GET /api/v1/system/config`. Sensitive fields (DB URLs, JWT secrets,
// SSL private keys, webhook signing keys, locator/user-backend credentials)
// MUST NEVER appear here. New `ProxyConfig` fields default to NOT exposed;
// add explicitly only after security review.
//
// Note: `ssl_private_key` / `ssl_certificate` are intentionally OMITTED —
// the cert paths can leak operator filesystem layout and the private key
// is obviously sensitive.
const SAFE_PROXY_FIELDS: &[&str] = &[
    "addr",
    "udp_port",
    "tcp_port",
    "tls_port",
    "ws_port",
    "useragent",
    "callid_suffix",
    "realms",
    "codecs",
    "media_proxy",
    "modules",
    "max_concurrency",
    "registrar_expires",
    "ensure_user",
    "session_timer",
    "session_expires",
    "enable_latching",
    "nat_fix",
    "passthrough_failure",
    "topology_hiding",
    "security_flush_interval_secs",
    "generated_dir",
    "t1_timer",
    "t1x64_timer",
    "sip_flow_max_items",
];

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
pub struct InfoResponse {
    pub version: String,
    pub build: BuildInfo,
    pub full_version_string: String,
}

#[derive(Debug, Serialize)]
pub struct BuildInfo {
    pub time: String,
    pub git_commit: String,
    pub git_branch: String,
    pub git_dirty: bool,
}

#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub proxy: Value,
    pub runtime: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
pub struct ClusterResponse {
    pub mode: &'static str,
    pub nodes: Vec<ClusterNode>,
    pub note: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ClusterNode {
    pub id: &'static str,
    pub role: &'static str,
    pub healthy: bool,
}

impl ClusterResponse {
    pub fn single_node() -> Self {
        Self {
            mode: "single_node",
            nodes: vec![ClusterNode {
                id: "primary",
                role: "primary",
                healthy: true,
            }],
            note: "Multi-node clustering is intentionally unsupported in v2.0. See ROADMAP.",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub calls: CallStats,
    pub proxy: ProxyStats,
    pub gateways: GatewayStats,
    pub security: SecurityStats,
}

#[derive(Debug, Serialize)]
pub struct CallStats {
    pub active: u64,
    pub total_24h: u64,
    pub failed_24h: u64,
}

#[derive(Debug, Serialize)]
pub struct ProxyStats {
    pub uptime_secs: u64,
    pub active_dialogs: u64,
    pub registrations: u64,
}

#[derive(Debug, Serialize)]
pub struct GatewayStats {
    pub up: u64,
    pub down: u64,
    pub total: u64,
}

#[derive(Debug, Serialize)]
pub struct SecurityStats {
    pub blocks_total: u64,
    pub flood_rejected_24h: u64,
    pub auth_failures_24h: u64,
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
        .route("/system/info", get(handle_info))
        .route("/system/config", get(handle_config))
        .route("/system/stats", get(handle_stats))
        .route("/system/cluster", get(handle_cluster))
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

// ---------------------------------------------------------------------------
// /system/info — SYS-03 (Phase 11 D-01)
// ---------------------------------------------------------------------------

async fn handle_info(State(_state): State<AppState>) -> ApiResult<Json<InfoResponse>> {
    Ok(Json(info_snapshot()))
}

pub(crate) fn info_snapshot() -> InfoResponse {
    let git_dirty_str = env!("GIT_DIRTY");
    let git_dirty = git_dirty_str.eq_ignore_ascii_case("dirty");
    InfoResponse {
        version: SHORT_VERSION.to_string(),
        build: BuildInfo {
            time: env!("BUILD_TIME_FMT").to_string(),
            git_commit: env!("GIT_COMMIT_HASH").to_string(),
            git_branch: env!("GIT_BRANCH").to_string(),
            git_dirty,
        },
        full_version_string: VERSION_INFO.to_string(),
    }
}

// ---------------------------------------------------------------------------
// /system/config — SYS-04 (Phase 11 D-02/D-03/D-04/D-05)
// ---------------------------------------------------------------------------

async fn handle_config(State(state): State<AppState>) -> ApiResult<Json<ConfigResponse>> {
    let proxy = project_proxy_config(&state.config().proxy);

    let rows = SystemConfigEntity::find()
        .all(state.db())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let runtime: BTreeMap<String, Value> = rows
        .into_iter()
        .map(|r| {
            let parsed = serde_json::from_str::<Value>(&r.value)
                .unwrap_or_else(|_| Value::String(r.value.clone()));
            (r.key, parsed)
        })
        .collect();

    Ok(Json(ConfigResponse { proxy, runtime }))
}

/// Per-field projection over `ProxyConfig` using the `SAFE_PROXY_FIELDS`
/// allowlist. Sensitive fields never appear in the output. Optional fields
/// that are `None` are omitted (no nulls emitted).
///
/// Implementation: serialize the entire ProxyConfig to a JSON object, then
/// filter to allowlisted keys only. This is safer than a free-form
/// projection because adding a new ProxyConfig field defaults to NOT
/// exposed (it does not appear in SAFE_PROXY_FIELDS).
fn project_proxy_config(proxy: &crate::config::ProxyConfig) -> Value {
    let full = match serde_json::to_value(proxy) {
        Ok(v) => v,
        Err(_) => return Value::Object(Map::new()),
    };
    let mut out = Map::new();
    if let Value::Object(map) = full {
        for &field in SAFE_PROXY_FIELDS {
            if let Some(v) = map.get(field) {
                if !v.is_null() {
                    out.insert(field.to_string(), v.clone());
                }
            }
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// /system/cluster — SYS-06 (Phase 11 D-09/D-10): hardcoded single-node
// ---------------------------------------------------------------------------

async fn handle_cluster(State(_state): State<AppState>) -> ApiResult<Json<ClusterResponse>> {
    Ok(Json(ClusterResponse::single_node()))
}

// ---------------------------------------------------------------------------
// /system/stats — SYS-05 (Phase 11 D-06/D-07/D-08)
// ---------------------------------------------------------------------------
//
// Curated subset grouped by calls/proxy/gateways/security. Pulls from the
// existing AppState atomic counters and SipServer accessors. Stats not
// readily available from existing state return 0 + TODO(phase-12) per
// plan D-08 — we do NOT add new counters in Phase 11.

async fn handle_stats(State(state): State<AppState>) -> ApiResult<Json<StatsResponse>> {
    let active = state
        .sip_server()
        .inner
        .active_call_registry
        .count()
        .try_into()
        .unwrap_or(0u64);

    // Phase 11 D-08: 24h-window values are not readily available from the
    // current registry — return cumulative atomic counters and tag with
    // TODO. Phase 12 may introduce a rolling-window decoration.
    // TODO(phase-12): replace with rolling-24h counters.
    let total_24h = state.total_calls.load(Ordering::Relaxed);
    let failed_24h = state.total_failed_calls.load(Ordering::Relaxed);

    let uptime_secs = (Utc::now() - state.uptime).num_seconds().max(0) as u64;

    let active_dialogs: u64 = state
        .sip_server()
        .inner
        .dialog_layer
        .len()
        .try_into()
        .unwrap_or(0u64);

    // TODO(phase-12): expose registrar count via a stable accessor.
    let registrations: u64 = 0;

    // TODO(phase-12): wire GatewayHealthMonitor snapshot once a stable
    // (up,down,total) accessor is in place.
    let gateways = GatewayStats {
        up: 0,
        down: 0,
        total: 0,
    };

    // TODO(phase-12): aggregate SecurityState block/flood/auth-failure
    // counters into rolling-24h totals once Phase 12 wiring lands.
    let security = SecurityStats {
        blocks_total: 0,
        flood_rejected_24h: 0,
        auth_failures_24h: 0,
    };

    Ok(Json(StatsResponse {
        calls: CallStats {
            active,
            total_24h,
            failed_24h,
        },
        proxy: ProxyStats {
            uptime_secs,
            active_dialogs,
            registrations,
        },
        gateways,
        security,
    }))
}
