//! Phase 5 Plan 05-04 Task 1 — TrunkCapacityState
//!
//! In-memory enforcement state for per-trunk capacity (D-03, D-06, D-07, D-08).
//!
//! - `TrunkCapacityState` owns a DashMap<trunk_group_id, Arc<TrunkCapacityGate>>.
//! - `TrunkCapacityGate` is a hand-rolled atomic gate combining:
//!     * an `active` AtomicU32 counter clamped at `max_calls` (0 = unlimited),
//!     * a token bucket for max_cps refilled once per second (0 = unlimited).
//! - `Permit` is an RAII handle returned by `try_acquire`; dropping it
//!   atomically decrements the active counter so capacity tracks the live
//!   call count without explicit release calls (D-03).
//!
//! No external token-bucket crate is used (D-07: prefer to avoid new deps).
//! `dashmap` is already a project dependency.

use dashmap::DashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct TrunkCapacityGate {
    active: AtomicU32,
    /// 0 = unlimited.
    max_calls: AtomicU32,
    /// Current available tokens in the per-second bucket.
    bucket_tokens: AtomicU32,
    /// 0 = unlimited (no CPS gating).
    bucket_max: AtomicU32,
    /// epoch ms when the bucket was last refilled.
    bucket_last_refill_ms: AtomicU64,
}

#[derive(Debug)]
pub enum AcquireOutcome {
    Ok(Permit),
    CallsExhausted,
    CpsExhausted,
}

pub struct Permit {
    gate: Arc<TrunkCapacityGate>,
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permit")
            .field("active", &self.gate.active.load(Ordering::Acquire))
            .finish()
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl TrunkCapacityGate {
    pub fn new(max_calls: Option<u32>, max_cps: Option<u32>) -> Self {
        let mc = max_calls.unwrap_or(0);
        let cps = max_cps.unwrap_or(0);
        Self {
            active: AtomicU32::new(0),
            max_calls: AtomicU32::new(mc),
            bucket_tokens: AtomicU32::new(cps),
            bucket_max: AtomicU32::new(cps),
            bucket_last_refill_ms: AtomicU64::new(now_epoch_ms()),
        }
    }

    pub fn update_limits(&self, max_calls: Option<u32>, max_cps: Option<u32>) {
        if let Some(mc) = max_calls {
            self.max_calls.store(mc, Ordering::Release);
        }
        if let Some(cps) = max_cps {
            self.bucket_max.store(cps, Ordering::Release);
            // Refill to new max immediately so admins increasing the cap see effect now.
            self.bucket_tokens.store(cps, Ordering::Release);
            self.bucket_last_refill_ms
                .store(now_epoch_ms(), Ordering::Release);
        }
    }

    /// Try to refill the bucket if >= 1 second has elapsed. Single-CAS claim
    /// of the refill slot to avoid double-refill under contention.
    fn refill_if_due(&self) {
        let max = self.bucket_max.load(Ordering::Acquire);
        if max == 0 {
            return; // unlimited; bucket disabled
        }
        let now = now_epoch_ms();
        let last = self.bucket_last_refill_ms.load(Ordering::Acquire);
        if now.saturating_sub(last) < 1000 {
            return;
        }
        // Claim the refill slot.
        if self
            .bucket_last_refill_ms
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.bucket_tokens.store(max, Ordering::Release);
        }
    }

    pub fn try_acquire(self: &Arc<Self>) -> AcquireOutcome {
        // Step 1: try to bump the active counter under max_calls cap.
        let max = self.max_calls.load(Ordering::Acquire);
        if max > 0 {
            // Atomic CAS loop: load current, ensure < max, increment.
            let mut current = self.active.load(Ordering::Acquire);
            loop {
                if current >= max {
                    return AcquireOutcome::CallsExhausted;
                }
                match self.active.compare_exchange_weak(
                    current,
                    current + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
        } else {
            self.active.fetch_add(1, Ordering::AcqRel);
        }

        // Step 2: try to consume one CPS token.
        self.refill_if_due();
        let cps_max = self.bucket_max.load(Ordering::Acquire);
        if cps_max > 0 {
            let mut tokens = self.bucket_tokens.load(Ordering::Acquire);
            loop {
                if tokens == 0 {
                    // CPS exhausted; rollback active counter we already bumped.
                    self.active.fetch_sub(1, Ordering::AcqRel);
                    return AcquireOutcome::CpsExhausted;
                }
                match self.bucket_tokens.compare_exchange_weak(
                    tokens,
                    tokens - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => tokens = actual,
                }
            }
        }

        AcquireOutcome::Ok(Permit { gate: self.clone() })
    }

    pub fn release(&self) {
        // Saturating sub via fetch_update.
        let _ = self.active.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |v| if v == 0 { None } else { Some(v - 1) },
        );
    }

    /// Tokens consumed in the current 1-second window (approximate, useful
    /// for backpressure visibility per D-04).
    pub fn snapshot_cps_rate(&self) -> u32 {
        let max = self.bucket_max.load(Ordering::Acquire);
        if max == 0 {
            return 0;
        }
        let avail = self.bucket_tokens.load(Ordering::Acquire);
        max.saturating_sub(avail)
    }

    pub fn current_active(&self) -> u32 {
        self.active.load(Ordering::Acquire)
    }
}

/// Shared registry of per-trunk_group capacity gates.
#[derive(Default, Debug)]
pub struct TrunkCapacityState {
    gates: DashMap<i64, Arc<TrunkCapacityGate>>,
}

impl TrunkCapacityState {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_gate(
        &self,
        trunk_group_id: i64,
        max_calls: Option<u32>,
        max_cps: Option<u32>,
    ) -> Arc<TrunkCapacityGate> {
        if let Some(g) = self.gates.get(&trunk_group_id) {
            let arc = g.clone();
            // Keep limits fresh on each acquire so PUT /capacity propagates.
            arc.update_limits(max_calls, max_cps);
            arc
        } else {
            let arc = Arc::new(TrunkCapacityGate::new(max_calls, max_cps));
            self.gates.insert(trunk_group_id, arc.clone());
            arc
        }
    }

    pub fn try_acquire(
        &self,
        trunk_group_id: i64,
        max_calls: Option<u32>,
        max_cps: Option<u32>,
    ) -> AcquireOutcome {
        let gate = self.ensure_gate(trunk_group_id, max_calls, max_cps);
        gate.try_acquire()
    }

    pub fn snapshot_cps_rate(&self, trunk_group_id: i64) -> u32 {
        self.gates
            .get(&trunk_group_id)
            .map(|g| g.snapshot_cps_rate())
            .unwrap_or(0)
    }

    pub fn current_active(&self, trunk_group_id: i64) -> u32 {
        self.gates
            .get(&trunk_group_id)
            .map(|g| g.current_active())
            .unwrap_or(0)
    }

    /// Phase 5 Plan 05-04: idempotent live limits update for the trunk group.
    /// Creates a fresh gate when none exists yet (allows the PUT /capacity
    /// handler to pre-warm the limits before the first INVITE arrives).
    pub fn update_limits(
        &self,
        trunk_group_id: i64,
        max_calls: Option<u32>,
        max_cps: Option<u32>,
    ) {
        self.ensure_gate(trunk_group_id, max_calls, max_cps);
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(max_calls: Option<u32>, max_cps: Option<u32>) -> Arc<TrunkCapacityGate> {
        Arc::new(TrunkCapacityGate::new(max_calls, max_cps))
    }

    #[test]
    fn acquire_within_max_calls_returns_ok() {
        let g = gate(Some(2), None);
        assert!(matches!(g.try_acquire(), AcquireOutcome::Ok(_)));
        assert!(matches!(g.try_acquire(), AcquireOutcome::Ok(_)));
    }

    #[test]
    fn acquire_at_max_calls_returns_calls_exhausted() {
        let g = gate(Some(2), None);
        let _p1 = match g.try_acquire() {
            AcquireOutcome::Ok(p) => p,
            _ => panic!("expected ok"),
        };
        let _p2 = match g.try_acquire() {
            AcquireOutcome::Ok(p) => p,
            _ => panic!("expected ok"),
        };
        assert!(matches!(g.try_acquire(), AcquireOutcome::CallsExhausted));
    }

    #[test]
    fn permit_drop_releases_active() {
        let g = gate(Some(1), None);
        {
            let _p = match g.try_acquire() {
                AcquireOutcome::Ok(p) => p,
                _ => panic!("expected ok"),
            };
            assert!(matches!(g.try_acquire(), AcquireOutcome::CallsExhausted));
        }
        // Permit dropped; next acquire succeeds.
        assert!(matches!(g.try_acquire(), AcquireOutcome::Ok(_)));
    }

    #[test]
    fn unlimited_max_calls_never_exhausts() {
        let g = gate(None, None);
        let mut permits = Vec::new();
        for _ in 0..1000 {
            match g.try_acquire() {
                AcquireOutcome::Ok(p) => permits.push(p),
                _ => panic!("unlimited should never exhaust"),
            }
        }
    }

    #[test]
    fn cps_token_bucket_drains() {
        let g = gate(None, Some(3));
        for _ in 0..3 {
            assert!(matches!(g.try_acquire(), AcquireOutcome::Ok(_)));
        }
        assert!(matches!(g.try_acquire(), AcquireOutcome::CpsExhausted));
    }

    #[tokio::test]
    async fn cps_token_bucket_refills_after_one_second() {
        let g = gate(None, Some(3));
        for _ in 0..3 {
            assert!(matches!(g.try_acquire(), AcquireOutcome::Ok(_)));
        }
        assert!(matches!(g.try_acquire(), AcquireOutcome::CpsExhausted));
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert!(matches!(g.try_acquire(), AcquireOutcome::Ok(_)));
    }

    #[test]
    fn unlimited_cps_never_exhausts() {
        let g = gate(None, None);
        for _ in 0..1000 {
            assert!(matches!(g.try_acquire(), AcquireOutcome::Ok(_)));
        }
    }

    #[test]
    fn update_limits_resizes_in_place() {
        let g = gate(Some(2), None);
        let _p1 = match g.try_acquire() {
            AcquireOutcome::Ok(p) => p,
            _ => panic!(),
        };
        let _p2 = match g.try_acquire() {
            AcquireOutcome::Ok(p) => p,
            _ => panic!(),
        };
        assert!(matches!(g.try_acquire(), AcquireOutcome::CallsExhausted));
        g.update_limits(Some(4), None);
        let _p3 = match g.try_acquire() {
            AcquireOutcome::Ok(p) => p,
            _ => panic!("expected ok after resize"),
        };
        let _p4 = match g.try_acquire() {
            AcquireOutcome::Ok(p) => p,
            _ => panic!("expected ok after resize"),
        };
        assert!(matches!(g.try_acquire(), AcquireOutcome::CallsExhausted));
    }

    #[tokio::test]
    async fn concurrent_acquires_dont_overshoot() {
        let g = gate(Some(10), None);
        let mut handles = Vec::new();
        let ok = Arc::new(AtomicU32::new(0));
        let exh = Arc::new(AtomicU32::new(0));
        for _ in 0..100 {
            let g = g.clone();
            let ok = ok.clone();
            let exh = exh.clone();
            handles.push(tokio::spawn(async move {
                match g.try_acquire() {
                    AcquireOutcome::Ok(p) => {
                        ok.fetch_add(1, Ordering::AcqRel);
                        // Hold the permit so it doesn't release while others run.
                        std::mem::forget(p);
                    }
                    AcquireOutcome::CallsExhausted => {
                        exh.fetch_add(1, Ordering::AcqRel);
                    }
                    AcquireOutcome::CpsExhausted => panic!("no cps gate configured"),
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(ok.load(Ordering::Acquire), 10, "exactly 10 ok");
        assert_eq!(exh.load(Ordering::Acquire), 90, "90 exhausted");
    }
}
