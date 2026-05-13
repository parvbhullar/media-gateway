//! Gateway OPTIONS health monitor (Plan 1).
//!
//! Background task periodically probes every active outbound SIP trunk by
//! sending a SIP OPTIONS request, then transitions `sip_trunk.status`
//! between `healthy`/`offline` based on consecutive success/failure
//! thresholds. The persistent state machine is `HealthTally`; the actual
//! probe lives in `probe_trunk`; the orchestrator is `GatewayHealthMonitor`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::models::trunk::{
    self as trunk_model, Column as TrunkColumn, Entity as TrunkEntity, Model as TrunkModel,
    TrunkDirection, TrunkStatus,
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

    // 1. Resolve destination. Prefer explicit outbound proxy, fall back to
    //    sip_server.
    let dest = match sip_cfg
        .outbound_proxy
        .as_deref()
        .or(sip_cfg.sip_server.as_deref())
    {
        Some(d) if !d.trim().is_empty() => d.trim().to_string(),
        _ => {
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
pub struct GatewayHealthMonitor {
    db: DatabaseConnection,
    endpoint: Option<rsipstack::transaction::endpoint::EndpointInnerRef>,
    tallies: Mutex<HashMap<i64, HealthTally>>,
    tick: Duration,
    default_interval: Duration,
    probe_timeout: Duration,
}

impl GatewayHealthMonitor {
    pub fn new(
        db: DatabaseConnection,
        endpoint: Option<rsipstack::transaction::endpoint::EndpointInnerRef>,
    ) -> Self {
        Self {
            db,
            endpoint,
            tallies: Mutex::new(HashMap::new()),
            tick: Duration::from_secs(10),
            default_interval: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }

    pub fn with_default_interval(mut self, interval: Duration) -> Self {
        self.default_interval = interval;
        self
    }

    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// Run the loop until `cancel` fires.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        tracing::info!(
            tick_secs = self.tick.as_secs(),
            default_interval_secs = self.default_interval.as_secs(),
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
    pub async fn tick_once(&self) -> anyhow::Result<()> {
        // Default probe = bound real method to the live endpoint.
        let endpoint = self.endpoint.clone();
        let probe_timeout = self.probe_timeout;
        let probe = move |trunk: TrunkModel| {
            let endpoint = endpoint.clone();
            async move {
                match endpoint {
                    Some(ep) => probe_trunk(&ep, &trunk, probe_timeout).await,
                    None => ProbeOutcome {
                        ok: false,
                        latency_ms: 0,
                        detail: "no endpoint".into(),
                    },
                }
            }
        };
        self.tick_with_probe(probe).await
    }

    /// Test-friendly tick that accepts an injected probe function so unit
    /// tests don't need a live SIP endpoint.
    pub async fn tick_with_probe<F, Fut>(&self, probe: F) -> anyhow::Result<()>
    where
        F: Fn(TrunkModel) -> Fut,
        Fut: std::future::Future<Output = ProbeOutcome>,
    {
        let rows = TrunkEntity::find()
            .filter(TrunkColumn::IsActive.eq(true))
            // OPTIONS probing only applies to SIP trunks. WebRTC and any
            // future kinds are skipped here so non-SIP rows never reach
            // SIP-typed field access below.
            .filter(TrunkColumn::Kind.eq("sip"))
            .filter(TrunkColumn::Direction.ne(TrunkDirection::Inbound.as_str()))
            .all(&self.db)
            .await?;

        let now = Utc::now();
        for trunk in rows {
            let interval_secs = trunk
                .health_check_interval_secs
                .unwrap_or(self.default_interval.as_secs() as i32)
                as i64;
            if interval_secs > 0 {
                if let Some(last) = trunk.last_health_check_at {
                    if (now - last).num_seconds() < interval_secs {
                        continue;
                    }
                }
            }
            let th = HealthThresholds {
                failure: trunk.failure_threshold.unwrap_or(3),
                recovery: trunk.recovery_threshold.unwrap_or(2),
            };

            let outcome = probe(trunk.clone()).await;

            let transition;
            let new_failures;
            let new_successes;
            {
                let mut tallies = self.tallies.lock().await;
                let tally = tallies
                    .entry(trunk.id)
                    .or_insert_with(|| HealthTally::new(trunk.status));
                transition = if outcome.ok {
                    tally.record_success(&th)
                } else {
                    tally.record_failure(&th)
                };
                new_failures = tally.consecutive_failures;
                new_successes = tally.consecutive_successes;
            }

            let mut am: trunk_model::ActiveModel = trunk.clone().into();
            am.last_health_check_at = Set(Some(now));
            am.consecutive_failures = Set(new_failures);
            am.consecutive_successes = Set(new_successes);
            if let Transition::To(new_status) = transition {
                am.status = Set(new_status);
                tracing::warn!(
                    trunk = %trunk.name,
                    ?new_status,
                    detail = %outcome.detail,
                    "gateway health transition"
                );
            }
            if let Err(e) = am.update(&self.db).await {
                tracing::warn!(trunk = %trunk.name, error = %e, "gateway_health: db update failed");
            }
        }
        Ok(())
    }
}
