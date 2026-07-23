//! Gateway OPTIONS health monitor (Plan 1).
//!
//! Background task periodically probes every active outbound SIP trunk by
//! sending a SIP OPTIONS request, then transitions `sip_trunk.status`
//! between `healthy`/`offline` based on consecutive success/failure
//! thresholds. The persistent state machine is `HealthTally`; the actual
//! probe lives in `probe_trunk`; the orchestrator is `GatewayHealthMonitor`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
};
use tokio_util::sync::CancellationToken;

use crate::models::trunk::{
    self as trunk_model, Column as TrunkColumn, Entity as TrunkEntity, Model as TrunkModel,
    SipTransport, TrunkStatus,
};

/// Configurable thresholds for the consecutive-result state machine.
#[derive(Debug, Clone, Copy)]
pub struct HealthThresholds {
    pub failure: i32,
    pub recovery: i32,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            failure: 3,
            recovery: 2,
        }
    }
}

/// Persistent counters and the current observed status for a single trunk.
#[derive(Debug, Clone)]
pub struct HealthTally {
    pub status: TrunkStatus,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
}

/// Result of feeding a probe outcome into the tally — a hint to the caller
/// about whether to write the new status back to the database.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Transition {
    NoChange,
    To(TrunkStatus),
}

impl HealthTally {
    pub fn new(status: TrunkStatus) -> Self {
        Self {
            status,
            consecutive_failures: 0,
            consecutive_successes: 0,
        }
    }

    pub fn record_success(&mut self, th: &HealthThresholds) -> Transition {
        self.consecutive_failures = 0;
        self.consecutive_successes += 1;
        if matches!(
            self.status,
            TrunkStatus::Offline | TrunkStatus::Warning | TrunkStatus::Standby
        ) && self.consecutive_successes >= th.recovery
        {
            self.status = TrunkStatus::Healthy;
            self.consecutive_successes = 0;
            return Transition::To(TrunkStatus::Healthy);
        }
        Transition::NoChange
    }

    pub fn record_failure(&mut self, th: &HealthThresholds) -> Transition {
        self.consecutive_successes = 0;
        self.consecutive_failures += 1;
        if matches!(
            self.status,
            TrunkStatus::Healthy | TrunkStatus::Warning | TrunkStatus::Standby
        ) && self.consecutive_failures >= th.failure
        {
            self.status = TrunkStatus::Offline;
            self.consecutive_failures = 0;
            return Transition::To(TrunkStatus::Offline);
        }
        Transition::NoChange
    }
}

/// Outcome of a single OPTIONS probe.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub ok: bool,
    pub latency_ms: u64,
    pub detail: String,
}

/// Send a SIP OPTIONS request to the trunk's `sip_server` (or
/// `outbound_proxy` if set) and return whether a final response came back
/// within `timeout`.
///
/// Policy: *any* final response (2xx or 4xx/5xx/6xx) from the trunk is
/// treated as `ok=true` — the box answered, which is all an OPTIONS ping
/// needs to establish. Many carriers reject anonymous OPTIONS with 4xx
/// (401/403/404/405) but are still perfectly healthy for outbound calls.
/// This matches Kamailio/FreeSWITCH "dispatcher" conventions. A clean 2xx
/// is reported as-is in `detail`; a non-2xx is reported as
/// `"reachable (<code>)"`. Timeouts, transport failures, and bad URIs are
/// the only `ok=false` cases.
/// Probe destination for a SIP trunk — the same field preference as INVITE
/// trunk routing (`convert_sip_trunk` in proxy/data.rs: `sip_server` first,
/// `outbound_proxy` as fallback), so the probe exercises — and keeps warm —
/// the pooled connection calls actually use.
pub(crate) fn probe_destination(
    sip_cfg: &crate::models::trunk::SipTrunkConfig,
) -> Option<String> {
    sip_cfg
        .sip_server
        .as_deref()
        .or(sip_cfg.outbound_proxy.as_deref())
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

pub async fn probe_trunk(
    endpoint: &rsipstack::transaction::endpoint::EndpointInnerRef,
    trunk: &TrunkModel,
    timeout: Duration,
) -> ProbeOutcome {
    use rsipstack::sip::{Method, SipMessage, StatusCode, StatusCodeKind};
    use rsipstack::transaction::{
        key::{TransactionKey, TransactionRole},
        make_tag,
        transaction::Transaction,
    };

    let start = std::time::Instant::now();
    let elapsed_ms = |t: std::time::Instant| t.elapsed().as_millis() as u64;

    // 0. Decode the SIP-specific kind config. The caller filters by
    //    kind="sip" before invoking us, but be defensive: a malformed
    //    kind_config shouldn't crash the probe loop.
    let sip_cfg = match trunk.sip() {
        Ok(cfg) => cfg,
        Err(e) => {
            return ProbeOutcome {
                ok: false,
                latency_ms: 0,
                detail: format!("invalid kind_config: {}", e),
            };
        }
    };

    // 1. Resolve destination (see `probe_destination` for the field
    //    preference rationale).
    let dest = match probe_destination(&sip_cfg) {
        Some(d) => d,
        None => {
            return ProbeOutcome {
                ok: false,
                latency_ms: 0,
                detail: "no sip_server configured".into(),
            };
        }
    };

    // 2. Parse/normalise URI (reuses the registrar helper).
    let server_uri = match crate::proxy::trunk_registrar::parse_server_uri(&dest) {
        Ok(u) => u,
        Err(e) => {
            return ProbeOutcome {
                ok: false,
                latency_ms: elapsed_ms(start),
                detail: format!("bad uri '{}': {}", dest, e),
            };
        }
    };

    // 3. Build From/To/Via. OPTIONS is out-of-dialog; From gets a fresh
    //    tag, To has none.
    let to = rsipstack::sip::typed::To {
        display_name: None,
        uri: server_uri.clone(),
        params: vec![],
    };
    let from = rsipstack::sip::typed::From {
        display_name: None,
        uri: server_uri.clone(),
        params: vec![],
    }
    .with_tag(make_tag());

    // Resolve the trunk's configured transport so the Via header and
    // underlying connection use the correct protocol (TCP/TLS/UDP).
    let transport = match sip_cfg.sip_transport {
        crate::models::trunk::SipTransport::Tcp => rsipstack::sip::Transport::Tcp,
        crate::models::trunk::SipTransport::Tls => rsipstack::sip::Transport::Tls,
        crate::models::trunk::SipTransport::Udp => rsipstack::sip::Transport::Udp,
    };
    let addrs = endpoint.transport_layer.get_addrs();
    let sip_addr = addrs
        .iter()
        .find(|a| a.r#type == Some(transport))
        .or_else(|| addrs.first())
        .cloned();
    let via = match endpoint.get_via(sip_addr, None) {
        Ok(v) => v,
        Err(e) => {
            return ProbeOutcome {
                ok: false,
                latency_ms: elapsed_ms(start),
                detail: format!("get_via failed: {}", e),
            };
        }
    };

    // 4. Build the OPTIONS request and wrap in a client transaction.
    let request = endpoint.make_request(
        Method::Options,
        server_uri.clone(),
        via,
        from,
        to,
        1, // CSeq
        None,
    );
    let key = match TransactionKey::from_request(&request, TransactionRole::Client) {
        Ok(k) => k,
        Err(e) => {
            return ProbeOutcome {
                ok: false,
                latency_ms: elapsed_ms(start),
                detail: format!("tx key: {}", e),
            };
        }
    };
    let mut tx = Transaction::new_client(key, request, endpoint.clone(), None);

    // Set the destination with the correct transport so the transaction layer
    // opens a TCP/TLS connection instead of falling back to UDP.
    let dest_addr = rsipstack::transport::SipAddr::try_from(&server_uri);
    if let Ok(mut dest_sip_addr) = dest_addr {
        dest_sip_addr.r#type = Some(transport);
        tx.destination = Some(dest_sip_addr);
    }

    // 5. Send, then await the first non-provisional response or time out.
    if let Err(e) = tx.send().await {
        return ProbeOutcome {
            ok: false,
            latency_ms: elapsed_ms(start),
            detail: format!("tx send: {}", e),
        };
    }

    let outcome = tokio::time::timeout(timeout, async {
        while let Some(msg) = tx.receive().await {
            if let SipMessage::Response(resp) = msg {
                if resp.status_code == StatusCode::Trying {
                    continue;
                }
                return Some(resp.status_code);
            }
        }
        None
    })
    .await;

    let latency_ms = elapsed_ms(start);
    match outcome {
        Ok(Some(code)) if code.kind() == StatusCodeKind::Successful => ProbeOutcome {
            ok: true,
            latency_ms,
            detail: code.to_string(),
        },
        Ok(Some(code)) => ProbeOutcome {
            ok: true,
            latency_ms,
            detail: format!("reachable ({})", code),
        },
        Ok(None) => ProbeOutcome {
            ok: false,
            latency_ms,
            detail: "transaction ended without final response".into(),
        },
        Err(_) => ProbeOutcome {
            ok: false,
            latency_ms: timeout.as_millis() as u64,
            detail: "timeout".into(),
        },
    }
}

/// Periodic OPTIONS health monitor for outbound trunks.
///
/// The probe cadence is dynamic per trunk:
/// - Trunks with `status = Healthy` are probed every `healthy_interval`
///   (default 300 s) — slow keepalive once a trunk is known good. TCP/TLS
///   SIP trunks tighten this to `STREAM_HEALTHY_INTERVAL_SECS` (60 s) so the
///   pooled stream socket stays warm inside carrier/NAT idle windows.
/// - Trunks with any other status (`Offline`, `Warning`, `Standby`) are
///   probed every `unhealthy_interval` (default 30 s) — fast recovery
///   detection so a flapping or down trunk gets re-evaluated quickly.
/// - A per-trunk `health_check_interval_secs` column on `rustpbx_trunks`
///   still wins when set (operator override).
pub struct GatewayHealthMonitor {
    db: DatabaseConnection,
    tick: Duration,
    healthy_interval: Duration,
    /// Back-off policy used while a trunk is non-healthy. `base` and `cap`
    /// here are the operator-facing knobs; the rest of the curve is
    /// derived (see [`BackoffPolicy`]).
    backoff: BackoffPolicy,
    probe_timeout: Duration,
}

impl GatewayHealthMonitor {
    /// `endpoint` is accepted for back-compat but is no longer stored on
    /// the monitor — the SIP prober owns the endpoint via the
    /// `health_probers` registry. The parameter is retained so existing
    /// call sites (and tests) need no signature change; rename or drop in
    /// a follow-up if you want a cleaner API.
    pub fn new(
        db: DatabaseConnection,
        _endpoint: Option<rsipstack::transaction::endpoint::EndpointInnerRef>,
    ) -> Self {
        Self {
            db,
            tick: Duration::from_secs(10),
            healthy_interval: Duration::from_secs(300),
            backoff: BackoffPolicy::default(),
            probe_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }

    /// Back-compat: callers using the old single-interval builder set the
    /// healthy interval (steady-state probe cadence, what the prior
    /// implementation used uniformly).
    pub fn with_default_interval(mut self, interval: Duration) -> Self {
        self.healthy_interval = interval;
        self
    }

    pub fn with_healthy_interval(mut self, interval: Duration) -> Self {
        self.healthy_interval = interval;
        self
    }

    pub fn with_unhealthy_interval(mut self, interval: Duration) -> Self {
        self.backoff.base = interval;
        self
    }

    pub fn with_max_unhealthy_interval(mut self, interval: Duration) -> Self {
        self.backoff.cap = interval;
        self
    }

    /// Replace the full backoff policy. Use this when you need to tune
    /// `linear_repeats`, `linear_levels`, or the geometric phase — the
    /// per-interval setters only touch `base` and `cap`.
    pub fn with_backoff_policy(mut self, policy: BackoffPolicy) -> Self {
        self.backoff = policy;
        self
    }

    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// Run the loop until `cancel` fires.
    ///
    /// On entry we reset every active trunk's `consecutive_failures` /
    /// `consecutive_successes` to 0 so the backoff curve always starts
    /// fresh after a service restart (previously the counters survived
    /// restart and a chronically-failed trunk immediately probed at the
    /// cap interval, which masked recoveries). Statuses themselves are
    /// preserved — a trunk known to be Offline before restart stays
    /// Offline until the prober proves otherwise.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        if let Err(e) = reset_counters_on_startup(&self.db).await {
            tracing::warn!(error = %e, "gateway_health: failed to reset counters on startup");
        }
        tracing::info!(
            tick_secs = self.tick.as_secs(),
            healthy_interval_secs = self.healthy_interval.as_secs(),
            backoff_base_secs = self.backoff.base.as_secs(),
            backoff_cap_secs = self.backoff.cap.as_secs(),
            backoff_linear_repeats = self.backoff.linear_repeats,
            backoff_linear_levels = self.backoff.linear_levels,
            "gateway_health: monitor started"
        );
        let mut interval = tokio::time::interval(self.tick);
        // Skip the immediate first tick so we don't blast the DB on boot.
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.tick_once().await {
                        tracing::warn!(error = %e, "gateway_health: tick_once failed");
                    }
                }
                _ = cancel.cancelled() => {
                    tracing::info!("gateway_health: shutdown");
                    return;
                }
            }
        }
    }

    /// Probe trunks whose last check is older than their per-trunk interval.
    ///
    /// Dispatches per-trunk via the [`crate::proxy::health_probers`]
    /// registry: every kind whose prober is registered gets probed; kinds
    /// without a prober are silently skipped (logged once at trace level
    /// to avoid log spam from unsupported kinds). The legacy `Kind.eq("sip")`
    /// filter is gone — non-SIP trunks (e.g. WebRTC) now show real health.
    pub async fn tick_once(&self) -> anyhow::Result<()> {
        let probe_timeout = self.probe_timeout;
        let probe = move |trunk: TrunkModel| {
            async move {
                match crate::proxy::health_probers::lookup(&trunk.kind) {
                    Some(prober) => prober.probe(&trunk, probe_timeout).await,
                    None => ProbeOutcome {
                        ok: false,
                        latency_ms: 0,
                        detail: format!("no prober registered for kind '{}'", trunk.kind),
                    },
                }
            }
        };
        self.tick_with_probe(probe).await
    }

    /// Test-friendly tick that accepts an injected probe function so unit
    /// tests don't need a live SIP endpoint or HTTP server.
    pub async fn tick_with_probe<F, Fut>(&self, probe: F) -> anyhow::Result<()>
    where
        F: Fn(TrunkModel) -> Fut,
        Fut: std::future::Future<Output = ProbeOutcome>,
    {
        // Probe every active trunk regardless of direction. Inbound trunks
        // were historically skipped (no outbound traffic to test) but that
        // meant a dead carrier on the incoming PSTN side stayed silently
        // broken until the next call dropped. Now we probe everything;
        // inbound + outbound have identical observability.
        let rows = TrunkEntity::find()
            .filter(TrunkColumn::IsActive.eq(true))
            .all(&self.db)
            .await?;

        let now = Utc::now();
        for trunk in rows {
            let interval_secs =
                effective_interval_secs(&trunk, self.healthy_interval, &self.backoff);
            if interval_secs > 0 {
                if let Some(last) = trunk.last_health_check_at {
                    if (now - last).num_seconds() < interval_secs {
                        continue;
                    }
                }
            }

            let outcome = probe(trunk.clone()).await;
            if let Err(e) = apply_probe_outcome(&self.db, &trunk, &outcome).await {
                tracing::warn!(trunk = %trunk.name, error = %e, "gateway_health: db update failed");
            }
        }
        Ok(())
    }
}

/// Healthy-state probe cadence for stream-transport (TCP/TLS) SIP trunks.
/// Carrier/NAT idle timers commonly kill silent TCP sockets after 60–120s
/// while the global healthy interval is 300s; probing every 60s keeps the
/// pooled connection warm inside that window. UDP keeps the global cadence —
/// there is no stream socket to keep alive.
pub const STREAM_HEALTHY_INTERVAL_SECS: u64 = 60;

/// Effective interval, in seconds, before the next probe should fire for
/// `trunk` given the global monitor settings. Per-trunk
/// `health_check_interval_secs` always wins (operator override). Otherwise:
///
/// - Healthy, TCP/TLS SIP trunk → `min(60s, healthy_interval)` so the pooled
///   stream socket is re-warmed inside typical carrier idle windows (an
///   explicitly lower global cadence still wins).
/// - Healthy, everything else → `healthy_interval` (steady-state cadence).
/// - Non-healthy → `unhealthy_interval * 2^consecutive_failures`, clamped to
///   `max_unhealthy_interval`. This keeps a flapping trunk re-checked
///   aggressively for the first few failures and then backs off so we don't
///   hammer a known-bad endpoint indefinitely.
pub fn effective_interval_secs(
    trunk: &TrunkModel,
    healthy_interval: Duration,
    backoff: &BackoffPolicy,
) -> i64 {
    if let Some(override_secs) = trunk.health_check_interval_secs {
        return override_secs as i64;
    }
    // A trunk that's still nominally Healthy but has started failing
    // (cf > 0) drops to the backoff cadence immediately. Previously a
    // freshly-broken Healthy trunk waited the full healthy_interval (5
    // min) between each of the three failures required to flip to
    // Offline — a 15-minute detection lag. Now the second failure is
    // probed within 30 s, so the worst case is healthy_interval + 60 s
    // (~6 min) instead of 3 × healthy_interval (~15 min).
    let still_healthy = matches!(trunk.status, TrunkStatus::Healthy) && trunk.consecutive_failures == 0;
    let dur = if still_healthy {
        // Non-SIP kinds and unparseable kind_configs fall back to the
        // global cadence.
        let is_stream = trunk
            .sip()
            .is_ok_and(|cfg| matches!(cfg.sip_transport, SipTransport::Tcp | SipTransport::Tls));
        if is_stream {
            Duration::from_secs(STREAM_HEALTHY_INTERVAL_SECS).min(healthy_interval)
        } else {
            healthy_interval
        }
    } else {
        backoff.interval(trunk.consecutive_failures)
    };
    dur.as_secs() as i64
}

/// Reset `consecutive_failures`, `consecutive_successes`, and
/// `last_health_check_at` to NULL/0 for every active trunk. Called once
/// when the monitor starts so:
///   1. The backoff schedule restarts from the base interval (cf=0).
///   2. Every trunk is treated as "never probed" → the first tick fires
///      a fresh probe immediately rather than honoring stale timestamps
///      that may have come from a backoff cap window before the restart.
pub async fn reset_counters_on_startup(db: &DatabaseConnection) -> anyhow::Result<u64> {
    use sea_orm::QueryFilter;
    let updated = TrunkEntity::update_many()
        .filter(TrunkColumn::IsActive.eq(true))
        .col_expr(
            TrunkColumn::ConsecutiveFailures,
            sea_orm::sea_query::Expr::value(0_i32),
        )
        .col_expr(
            TrunkColumn::ConsecutiveSuccesses,
            sea_orm::sea_query::Expr::value(0_i32),
        )
        .col_expr(
            TrunkColumn::LastHealthCheckAt,
            sea_orm::sea_query::Expr::value(sea_orm::sea_query::Value::ChronoDateTimeUtc(None)),
        )
        .exec(db)
        .await?;
    if updated.rows_affected > 0 {
        tracing::info!(
            rows = updated.rows_affected,
            "gateway_health: reset consecutive_failures/successes + last_health_check_at for active trunks on startup"
        );
    }
    Ok(updated.rows_affected)
}

/// Two-phase backoff policy. All knobs are explicit — no constants buried
/// in the algorithm — so operators (or tests) can tune the curve without
/// touching the implementation.
///
/// Schedule, as a function of `consecutive_failures` (`cf`):
///
/// ### Phase 1 — linear ramp
/// The first `linear_levels` levels each hold the same value for
/// `linear_repeats` consecutive failures. Level `L` (0-indexed) yields
/// `base × (L + 1)`. With `linear_levels = linear_repeats = 3` and
/// `base = 30s` this produces `30 30 30 60 60 60 90 90 90`.
///
/// ### Phase 2 — geometric ramp
/// Once `cf` exceeds the linear span, each subsequent failure doubles the
/// multiplier starting from `geometric_start_multiplier`. With
/// `geometric_start_multiplier = 4` and `geometric_doubling_base = 2`,
/// the sequence is `base × 4`, `base × 8`, `base × 16`, … (e.g. 120, 240,
/// 480, 960).
///
/// Every value is clamped to `cap` and stays there forever once reached.
/// Negative `cf` is treated as 0.
#[derive(Debug, Clone, Copy)]
pub struct BackoffPolicy {
    /// Unit interval. Multiplied by the level/factor to produce the actual
    /// wait. Default 30s.
    pub base: Duration,
    /// Upper bound — the wait never exceeds this. Default 1800s (30 min).
    pub cap: Duration,
    /// How many failures we sit at each linear level before advancing.
    /// Default 3.
    pub linear_repeats: u32,
    /// Number of distinct linear levels (multipliers `1..=linear_levels`).
    /// Default 3.
    pub linear_levels: u32,
    /// First multiplier used by the geometric phase. Default 4 so the
    /// curve continues smoothly past `linear_levels = 3` (which ends at
    /// multiplier 3). Operators wanting a sharper jump can raise this.
    pub geometric_start_multiplier: u32,
    /// Doubling base for the geometric phase. Default 2 (true doubling).
    pub geometric_doubling_base: u32,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(30),
            cap: Duration::from_secs(1800),
            linear_repeats: 3,
            linear_levels: 3,
            geometric_start_multiplier: 4,
            geometric_doubling_base: 2,
        }
    }
}

impl BackoffPolicy {
    /// Resolve the wait time for a given `consecutive_failures` value.
    /// Saturating arithmetic throughout — pathological `cf` values clamp
    /// to `cap` without panicking.
    pub fn interval(&self, consecutive_failures: i32) -> Duration {
        let cf = consecutive_failures.max(0) as u32;
        let base_secs = self.base.as_secs().max(1);
        let cap_secs = self.cap.as_secs().max(base_secs);
        let linear_repeats = self.linear_repeats.max(1);
        let linear_levels = self.linear_levels;
        let doubling_base = self.geometric_doubling_base.max(2) as u64;

        let linear_span = linear_levels.saturating_mul(linear_repeats);
        let scaled: u64 = if cf < linear_span {
            let level = (cf / linear_repeats) as u64;
            base_secs.saturating_mul(level + 1)
        } else {
            let offset = cf - linear_span;
            // factor = geometric_start_multiplier * doubling_base^offset
            // — capped so the multiplication can't overflow.
            let mut factor = self.geometric_start_multiplier as u64;
            for _ in 0..offset {
                factor = factor.saturating_mul(doubling_base);
                if base_secs.saturating_mul(factor) >= cap_secs {
                    return Duration::from_secs(cap_secs);
                }
            }
            base_secs.saturating_mul(factor)
        };

        Duration::from_secs(scaled.min(cap_secs))
    }
}

/// Back-compat free function: resolves an interval against a default
/// [`BackoffPolicy`] with the given `base` and `cap`.
pub fn backoff_interval(base: Duration, cap: Duration, consecutive_failures: i32) -> Duration {
    BackoffPolicy {
        base,
        cap,
        ..BackoffPolicy::default()
    }
    .interval(consecutive_failures)
}

/// Apply a single probe outcome to a trunk row: feeds the outcome into a
/// freshly-seeded `HealthTally` (using the DB row as the source of truth),
/// applies any status transition, and writes counters + status back. Used
/// by both the periodic monitor and the console "Check now" handler so the
/// two stay in lock-step.
///
/// Returns the persisted row on success so the caller (e.g. the manual
/// probe endpoint) can return the refreshed view to the UI without an
/// extra round-trip.
pub async fn apply_probe_outcome(
    db: &DatabaseConnection,
    trunk: &TrunkModel,
    outcome: &ProbeOutcome,
) -> anyhow::Result<TrunkModel> {
    let th = HealthThresholds {
        failure: trunk.failure_threshold.unwrap_or(3),
        recovery: trunk.recovery_threshold.unwrap_or(2),
    };
    let mut tally = HealthTally {
        status: trunk.status,
        consecutive_failures: trunk.consecutive_failures,
        consecutive_successes: trunk.consecutive_successes,
    };
    let transition = if outcome.ok {
        tally.record_success(&th)
    } else {
        tally.record_failure(&th)
    };
    let now = Utc::now();
    let mut am: trunk_model::ActiveModel = trunk.clone().into();
    am.last_health_check_at = Set(Some(now));
    am.consecutive_failures = Set(tally.consecutive_failures);
    am.consecutive_successes = Set(tally.consecutive_successes);
    if let Transition::To(new_status) = transition {
        am.status = Set(new_status);
        tracing::warn!(
            trunk = %trunk.name,
            ?new_status,
            detail = %outcome.detail,
            "gateway health transition"
        );
    }
    let updated = am.update(db).await?;
    Ok(updated)
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    fn default_policy() -> BackoffPolicy {
        BackoffPolicy::default()
    }

    #[test]
    fn default_policy_matches_operator_specified_sequence() {
        // The operator's stated curve, in seconds:
        //   cf=0..2 → 30,  cf=3..5 → 60,  cf=6..8 → 90,
        //   cf=9 → 120,  cf=10 → 240,  cf=11 → 480,  cf=12 → 960,
        //   cf≥13 → 1800 (cap).
        let p = default_policy();
        let expected: [(i32, u64); 14] = [
            (0, 30),  (1, 30),  (2, 30),
            (3, 60),  (4, 60),  (5, 60),
            (6, 90),  (7, 90),  (8, 90),
            (9, 120),
            (10, 240),
            (11, 480),
            (12, 960),
            (13, 1800),
        ];
        for (cf, want) in expected {
            assert_eq!(
                p.interval(cf).as_secs(),
                want,
                "cf={} expected {}s",
                cf,
                want,
            );
        }
    }

    #[test]
    fn cap_holds_for_all_subsequent_failures() {
        let p = default_policy();
        for cf in [14, 20, 73, 1000, i32::MAX] {
            assert_eq!(p.interval(cf), Duration::from_secs(1800), "cf={} should be cap", cf);
        }
    }

    #[test]
    fn negative_cf_treated_as_zero() {
        assert_eq!(default_policy().interval(-5), Duration::from_secs(30));
    }

    #[test]
    fn custom_policy_with_no_linear_phase_is_pure_doubling() {
        let p = BackoffPolicy {
            base: Duration::from_secs(10),
            cap: Duration::from_secs(10_000),
            linear_repeats: 1,
            linear_levels: 0,
            geometric_start_multiplier: 1,
            geometric_doubling_base: 2,
        };
        assert_eq!(p.interval(0).as_secs(), 10);
        assert_eq!(p.interval(1).as_secs(), 20);
        assert_eq!(p.interval(2).as_secs(), 40);
        assert_eq!(p.interval(5).as_secs(), 320);
    }

    #[test]
    fn custom_policy_longer_linear_phase() {
        // 5 repeats × 4 levels — useful for trunks where flapping is fine.
        let p = BackoffPolicy {
            base: Duration::from_secs(20),
            cap: Duration::from_secs(3600),
            linear_repeats: 5,
            linear_levels: 4,
            geometric_start_multiplier: 5,
            geometric_doubling_base: 2,
        };
        assert_eq!(p.interval(0).as_secs(), 20);   // level 0
        assert_eq!(p.interval(4).as_secs(), 20);
        assert_eq!(p.interval(5).as_secs(), 40);   // level 1
        assert_eq!(p.interval(14).as_secs(), 60);  // level 2
        assert_eq!(p.interval(19).as_secs(), 80);  // level 3 (last linear)
        assert_eq!(p.interval(20).as_secs(), 100); // geometric: 20 × 5
        assert_eq!(p.interval(21).as_secs(), 200); // 20 × 10
    }

    #[test]
    fn cap_clamps_pathological_inputs_without_overflow() {
        let p = BackoffPolicy {
            base: Duration::from_secs(0), // forced to 1s by guard
            cap: Duration::from_secs(60),
            linear_repeats: 1,
            linear_levels: 0,
            geometric_start_multiplier: u32::MAX,
            geometric_doubling_base: u32::MAX,
        };
        for cf in [0, 1, 5, 100, i32::MAX] {
            assert!(p.interval(cf) <= Duration::from_secs(60));
        }
    }

    fn fake_trunk(status: TrunkStatus, cf: i32, override_secs: Option<i32>) -> TrunkModel {
        TrunkModel {
            status,
            consecutive_failures: cf,
            health_check_interval_secs: override_secs,
            ..TrunkModel::default()
        }
    }

    #[test]
    fn effective_interval_uses_healthy_when_truly_healthy() {
        let t = fake_trunk(TrunkStatus::Healthy, 0, None);
        assert_eq!(
            effective_interval_secs(&t, Duration::from_secs(300), &BackoffPolicy::default()),
            300,
        );
    }

    #[test]
    fn effective_interval_drops_to_backoff_when_healthy_starts_failing() {
        // Healthy trunk with cf=1 must NOT wait 5 minutes for the next
        // probe — we want to confirm/clear the failure quickly.
        let t = fake_trunk(TrunkStatus::Healthy, 1, None);
        assert_eq!(
            effective_interval_secs(&t, Duration::from_secs(300), &BackoffPolicy::default()),
            30,
        );
        let t2 = fake_trunk(TrunkStatus::Healthy, 2, None);
        assert_eq!(
            effective_interval_secs(&t2, Duration::from_secs(300), &BackoffPolicy::default()),
            30,
        );
    }

    #[test]
    fn effective_interval_follows_backoff_when_offline() {
        let t = fake_trunk(TrunkStatus::Offline, 9, None);
        assert_eq!(
            effective_interval_secs(&t, Duration::from_secs(300), &BackoffPolicy::default()),
            120,
        );
    }

    #[test]
    fn effective_interval_per_trunk_override_wins_even_when_offline() {
        let t = fake_trunk(TrunkStatus::Offline, 50, Some(15));
        assert_eq!(
            effective_interval_secs(&t, Duration::from_secs(300), &BackoffPolicy::default()),
            15,
        );
    }

    fn fake_sip_trunk(
        status: TrunkStatus,
        cf: i32,
        override_secs: Option<i32>,
        transport: &str,
    ) -> TrunkModel {
        TrunkModel {
            status,
            consecutive_failures: cf,
            health_check_interval_secs: override_secs,
            kind: "sip".to_string(),
            kind_config: serde_json::json!({ "sip_transport": transport }),
            ..TrunkModel::default()
        }
    }

    #[test]
    fn healthy_stream_trunks_default_to_60s_cadence() {
        for transport in ["tcp", "tls"] {
            let t = fake_sip_trunk(TrunkStatus::Healthy, 0, None, transport);
            assert_eq!(
                effective_interval_secs(&t, Duration::from_secs(300), &BackoffPolicy::default()),
                STREAM_HEALTHY_INTERVAL_SECS as i64,
                "transport={}",
                transport,
            );
        }
    }

    #[test]
    fn healthy_udp_trunk_keeps_global_cadence() {
        let t = fake_sip_trunk(TrunkStatus::Healthy, 0, None, "udp");
        assert_eq!(
            effective_interval_secs(&t, Duration::from_secs(300), &BackoffPolicy::default()),
            300,
        );
    }

    #[test]
    fn stream_default_never_raises_an_explicitly_lower_global_cadence() {
        let t = fake_sip_trunk(TrunkStatus::Healthy, 0, None, "tcp");
        assert_eq!(
            effective_interval_secs(&t, Duration::from_secs(5), &BackoffPolicy::default()),
            5,
        );
    }

    #[test]
    fn per_trunk_override_beats_stream_default() {
        let t = fake_sip_trunk(TrunkStatus::Healthy, 0, Some(300), "tcp");
        assert_eq!(
            effective_interval_secs(&t, Duration::from_secs(300), &BackoffPolicy::default()),
            300,
        );
    }

    #[test]
    fn failing_stream_trunk_uses_backoff_not_stream_default() {
        let t = fake_sip_trunk(TrunkStatus::Healthy, 1, None, "tcp");
        assert_eq!(
            effective_interval_secs(&t, Duration::from_secs(300), &BackoffPolicy::default()),
            30,
        );
    }

    #[test]
    fn probe_destination_prefers_sip_server_like_invite_routing() {
        let cfg = trunk_model::SipTrunkConfig {
            sip_server: Some("sip.carrier.example:5060".into()),
            outbound_proxy: Some("proxy.other.example:5060".into()),
            ..Default::default()
        };
        assert_eq!(
            probe_destination(&cfg).as_deref(),
            Some("sip.carrier.example:5060"),
        );
    }

    #[test]
    fn probe_destination_falls_back_to_outbound_proxy() {
        let cfg = trunk_model::SipTrunkConfig {
            sip_server: None,
            outbound_proxy: Some("proxy.other.example:5060".into()),
            ..Default::default()
        };
        assert_eq!(
            probe_destination(&cfg).as_deref(),
            Some("proxy.other.example:5060"),
        );
    }

    #[test]
    fn probe_destination_none_when_unconfigured() {
        assert_eq!(probe_destination(&Default::default()), None);
        // Whitespace-only sip_server is "configured but empty" — no silent
        // fallback to outbound_proxy (mirrors the pre-existing behavior).
        let cfg = trunk_model::SipTrunkConfig {
            sip_server: Some("   ".into()),
            outbound_proxy: Some("proxy.other.example:5060".into()),
            ..Default::default()
        };
        assert_eq!(probe_destination(&cfg), None);
    }
}
