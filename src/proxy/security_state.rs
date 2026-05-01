//! Phase 10 Plan 10-01 — Security suite shared state (SEC-01..SEC-05).
//!
//! In-memory enforcement state for the security suite. Mirrors the
//! `trunk_capacity_state.rs` DashMap+atomic CAS pattern (Phase 5 D-03..D-08).
//!
//! Composition (CONTEXT.md D-02, D-15):
//! - `flood`: `DashMap<IpAddr, Arc<WindowState>>` — per-IP sliding window
//!   counter for any incoming SIP message. Hot path; zero DB hits.
//! - `brute_force`: `DashMap<(IpAddr, Realm), Arc<FailureState>>` — per
//!   (IP, realm) auth failure counter; threshold breach writes a row to
//!   `supersip_security_blocks` and seeds `block_cache`.
//! - `firewall_rules`: `RwLock<Vec<FirewallRule>>` — mirror of
//!   `supersip_security_rules` ordered by position; reloaded on PATCH.
//! - `block_cache`: `RwLock<Vec<BlockEntry>>` — mirror of active rows
//!   (`unblocked_at IS NULL`) from `supersip_security_blocks`; refreshed by
//!   the periodic flush task and on auto-block writes.
//!
//! Stats vs durability (D-02):
//! - Flood + brute-force counters live ONLY in-memory; up to one
//!   `flush_interval_secs` of stats may be lost on crash.
//! - Auto-block writes hit the DB synchronously — blocks survive restart.

use dashmap::DashMap;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::MissedTickBehavior;

// Tunables (CONTEXT.md D-05, D-08).

/// Phase 10 D-05 — flood threshold (requests per window).
pub const FLOOD_THRESHOLD: u64 = 100;
/// Phase 10 D-05 — flood sliding-window width.
pub const FLOOD_WINDOW_MS: u64 = 10_000;
/// Phase 10 D-08 — brute-force auth-failure threshold per window.
pub const BRUTE_FORCE_THRESHOLD: u64 = 10;
/// Phase 10 D-08 — brute-force window width.
pub const BRUTE_FORCE_WINDOW_MS: u64 = 60_000;

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// WindowState (CAS-reset sliding window).

/// Sliding-window counter with CAS reset. Used both for the flood tracker
/// (per-IP) and the brute-force tracker (per (IP, realm)).
#[derive(Debug)]
pub struct WindowState {
    count: AtomicU64,
    window_start_ms: AtomicU64,
}

impl Default for WindowState {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowState {
    pub fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            window_start_ms: AtomicU64::new(now_epoch_ms()),
        }
    }

    /// Increment the counter and return `true` if the threshold has been
    /// breached within the current window. Resets the window via CAS once
    /// `window_ms` has elapsed (single-claim slot under contention).
    pub fn record_and_check(&self, threshold: u64, window_ms: u64) -> bool {
        let now = now_epoch_ms();
        let start = self.window_start_ms.load(Ordering::Acquire);
        if now.saturating_sub(start) >= window_ms {
            if self
                .window_start_ms
                .compare_exchange(start, now, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.count.store(0, Ordering::Release);
            }
        }
        let new_count = self.count.fetch_add(1, Ordering::AcqRel) + 1;
        new_count >= threshold
    }

    /// Snapshot `(count, window_start_ms)` for API read paths.
    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.count.load(Ordering::Acquire),
            self.window_start_ms.load(Ordering::Acquire),
        )
    }
}

/// Brute-force per-(IP, realm) counter — same shape as `WindowState`.
pub type FailureState = WindowState;

// Cached rule + block snapshot rows.

/// Mirror of one `supersip_security_rules` row (CONTEXT.md D-15).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FirewallRule {
    pub position: i32,
    pub action: String,
    pub cidr: String,
    pub description: Option<String>,
}

/// Mirror of one active `supersip_security_blocks` row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockEntry {
    pub ip: String,
    pub realm: String,
    pub block_reason: String,
}

// API snapshot DTOs (used by 10-02 handlers).

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FloodEntry {
    pub ip: String,
    pub request_count: u64,
    pub window_start_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthFailureEntry {
    pub ip: String,
    pub realm: String,
    pub failure_count: u64,
    pub window_start_ms: u64,
}

// SecurityState.

#[derive(Default, Debug)]
pub struct SecurityState {
    flood: DashMap<IpAddr, Arc<WindowState>>,
    brute_force: DashMap<(IpAddr, String), Arc<FailureState>>,
    firewall_rules: std::sync::RwLock<Vec<FirewallRule>>,
    block_cache: std::sync::RwLock<Vec<BlockEntry>>,
}

impl SecurityState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hot-path: record one inbound SIP message from `ip`. Returns true if
    /// the flood threshold has been breached in the current window.
    pub fn record_message(&self, ip: IpAddr) -> bool {
        let entry = self
            .flood
            .entry(ip)
            .or_insert_with(|| Arc::new(WindowState::new()))
            .clone();
        entry.record_and_check(FLOOD_THRESHOLD, FLOOD_WINDOW_MS)
    }

    /// Hot-path: record one auth failure for `(ip, realm)`. Returns true if
    /// the brute-force threshold has been breached.
    pub fn record_auth_failure(&self, ip: IpAddr, realm: String) -> bool {
        let entry = self
            .brute_force
            .entry((ip, realm))
            .or_insert_with(|| Arc::new(FailureState::new()))
            .clone();
        entry.record_and_check(BRUTE_FORCE_THRESHOLD, BRUTE_FORCE_WINDOW_MS)
    }

    /// Read-path: snapshot live flood counters for `GET /flood-tracker`.
    pub fn snapshot_flood_entries(&self) -> Vec<FloodEntry> {
        self.flood
            .iter()
            .filter_map(|kv| {
                let (count, start) = kv.value().snapshot();
                if count == 0 {
                    None
                } else {
                    Some(FloodEntry {
                        ip: kv.key().to_string(),
                        request_count: count,
                        window_start_ms: start,
                    })
                }
            })
            .collect()
    }

    /// Read-path: snapshot live auth failure counters.
    pub fn snapshot_auth_failure_entries(&self) -> Vec<AuthFailureEntry> {
        self.brute_force
            .iter()
            .filter_map(|kv| {
                let (count, start) = kv.value().snapshot();
                if count == 0 {
                    None
                } else {
                    let (ip, realm) = kv.key();
                    Some(AuthFailureEntry {
                        ip: ip.to_string(),
                        realm: realm.clone(),
                        failure_count: count,
                        window_start_ms: start,
                    })
                }
            })
            .collect()
    }

    /// Replace the entire firewall cache. Called by PATCH handler (10-02)
    /// AFTER the DB transaction commits (CONTEXT.md D-15).
    pub fn replace_firewall_cache(&self, rules: Vec<FirewallRule>) {
        if let Ok(mut w) = self.firewall_rules.write() {
            *w = rules;
        }
    }

    /// Read-locked clone of the firewall cache for evaluation on the hot path.
    pub fn firewall_rules_snapshot(&self) -> Vec<FirewallRule> {
        self.firewall_rules
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Replace the block cache wholesale.
    pub fn load_block_cache(&self, entries: Vec<BlockEntry>) {
        if let Ok(mut w) = self.block_cache.write() {
            *w = entries;
        }
    }

    /// Hot-path: is `ip` currently blocked (any realm)?
    pub fn is_ip_blocked(&self, ip: IpAddr) -> bool {
        let needle = ip.to_string();
        self.block_cache
            .read()
            .map(|guard| guard.iter().any(|e| e.ip == needle))
            .unwrap_or(false)
    }

    /// Drop every cached block entry for `ip`.
    pub fn purge_block_cache_for_ip(&self, ip: &str) {
        if let Ok(mut w) = self.block_cache.write() {
            w.retain(|e| e.ip != ip);
        }
    }

    /// Persist a new auto-block row via SeaORM ActiveModel. Idempotent
    /// against the UNIQUE (ip, realm) index.
    pub async fn write_block(
        &self,
        db: &sea_orm::DatabaseConnection,
        ip: IpAddr,
        realm: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        use crate::models::security_blocks::{ActiveModel as BlockActive, Entity as BlockEntity};
        let now = chrono::Utc::now();
        let am = BlockActive {
            ip: Set(ip.to_string()),
            realm: Set(realm.to_string()),
            block_reason: Set(reason.to_string()),
            blocked_at: Set(now),
            unblocked_at: Set(None),
            auto_unblock_at: Set(None),
            ..Default::default()
        };
        match BlockEntity::insert(am).exec(db).await {
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(
                    "supersip_security_blocks insert collision (ok if duplicate): {e}"
                );
            }
        }
        self.load_block_cache_from_db(db).await.ok();
        Ok(())
    }

    /// Reload the block cache from `unblocked_at IS NULL` rows.
    pub async fn load_block_cache_from_db(
        &self,
        db: &sea_orm::DatabaseConnection,
    ) -> anyhow::Result<()> {
        use crate::models::security_blocks::{Column as BlockCol, Entity as SecurityBlockEntity};
        let rows = SecurityBlockEntity::find()
            .filter(BlockCol::UnblockedAt.is_null())
            .all(db)
            .await?;
        let entries: Vec<BlockEntry> = rows
            .into_iter()
            .map(|m| BlockEntry {
                ip: m.ip,
                realm: m.realm,
                block_reason: m.block_reason,
            })
            .collect();
        self.load_block_cache(entries);
        Ok(())
    }

    /// Periodic flush hook (CONTEXT.md D-02). Stats live only in DashMap.
    /// This evicts stale window entries (count == 0 AND window expired
    /// more than 2x duration) to bound memory under spoofed-IP DDoS
    /// (threat T-10-01-01 / RISK-05).
    pub async fn flush_stats_to_db(
        &self,
        _db: &sea_orm::DatabaseConnection,
    ) -> anyhow::Result<()> {
        let now = now_epoch_ms();
        let flood_cutoff = FLOOD_WINDOW_MS.saturating_mul(2);
        self.flood.retain(|_ip, ws| {
            let (count, start) = ws.snapshot();
            count > 0 || now.saturating_sub(start) < flood_cutoff
        });
        let bf_cutoff = BRUTE_FORCE_WINDOW_MS.saturating_mul(2);
        self.brute_force.retain(|_key, fs| {
            let (count, start) = fs.snapshot();
            count > 0 || now.saturating_sub(start) < bf_cutoff
        });
        Ok(())
    }
}

// Background flush task.

/// Spawned in `SipServer::build()` alongside the webhook processor.
/// Wakes every `flush_interval_secs` to:
///   1. Evict stale flood / brute-force entries.
///   2. Refresh `block_cache` from DB.
/// Cancellation: respects `token` for graceful shutdown via select!.
pub async fn run_flush_task(
    db: sea_orm::DatabaseConnection,
    state: Arc<SecurityState>,
    flush_interval_secs: u64,
    token: tokio_util::sync::CancellationToken,
) {
    let interval_dur = std::time::Duration::from_secs(flush_interval_secs.max(1));
    let mut ticker = tokio::time::interval(interval_dur);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // First tick fires immediately; skip it so we don't double-flush at
    // startup when the cache is already empty.
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = token.cancelled() => {
                tracing::debug!("security flush task cancelled");
                break;
            }
            _ = ticker.tick() => {
                if let Err(e) = state.flush_stats_to_db(&db).await {
                    tracing::warn!("security flush_stats_to_db error: {e}");
                }
                if let Err(e) = state.load_block_cache_from_db(&db).await {
                    tracing::warn!("security load_block_cache_from_db error: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn flood_threshold_breach_is_detected() {
        let s = SecurityState::new();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        for i in 0..(FLOOD_THRESHOLD - 1) {
            assert!(!s.record_message(ip), "no breach at {i}");
        }
        assert!(s.record_message(ip), "breach at threshold");
    }

    #[test]
    fn brute_force_threshold_breach_is_detected() {
        let s = SecurityState::new();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        for i in 0..(BRUTE_FORCE_THRESHOLD - 1) {
            assert!(!s.record_auth_failure(ip, "site".into()), "no breach at {i}");
        }
        assert!(s.record_auth_failure(ip, "site".into()), "breach");
    }

    #[test]
    fn firewall_cache_round_trips() {
        let s = SecurityState::new();
        s.replace_firewall_cache(vec![FirewallRule {
            position: 0,
            action: "deny".into(),
            cidr: "10.0.0.0/8".into(),
            description: None,
        }]);
        let snap = s.firewall_rules_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].cidr, "10.0.0.0/8");
    }

    #[test]
    fn block_cache_lookup_and_purge() {
        let s = SecurityState::new();
        s.load_block_cache(vec![BlockEntry {
            ip: "1.2.3.4".into(),
            realm: "site".into(),
            block_reason: "brute_force".into(),
        }]);
        let ip = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));
        assert!(s.is_ip_blocked(ip));
        s.purge_block_cache_for_ip("1.2.3.4");
        assert!(!s.is_ip_blocked(ip));
    }

    #[test]
    fn snapshot_filters_zero_count_entries() {
        let s = SecurityState::new();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));
        s.record_message(ip);
        let snap = s.snapshot_flood_entries();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].request_count, 1);
    }
}
