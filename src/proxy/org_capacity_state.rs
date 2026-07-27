//! Org-level capacity enforcement — CPS and concurrent-call limits scoped
//! to an `org_id` rather than a trunk. Structurally identical to
//! `TrunkCapacityGate` (`trunk_capacity_state.rs`): an atomic active-call
//! counter clamped at `max_calls`, plus a per-second token bucket for
//! `max_cps`. Reuses that module's `AcquireOutcome`/`Permit` types — a
//! capacity gate's semantics don't change based on what it's keyed by.
//!
//! Checked *in addition to* the trunk-level gate on call admission: either
//! exhausted rejects the call. Callers skip this gate entirely when the
//! call's `org_id` is the unassigned sentinel (`organization::UNASSIGNED_ORG_ID`)
//! or has no matching `organizations` row.

use crate::proxy::trunk_capacity_state::{AcquireOutcome, TrunkCapacityGate};
use dashmap::DashMap;
use std::sync::Arc;

/// Shared registry of per-org capacity gates.
#[derive(Default, Debug)]
pub struct OrgCapacityState {
    gates: DashMap<String, Arc<TrunkCapacityGate>>,
}

impl OrgCapacityState {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_gate(
        &self,
        org_id: &str,
        max_calls: Option<u32>,
        max_cps: Option<u32>,
    ) -> Arc<TrunkCapacityGate> {
        if let Some(g) = self.gates.get(org_id) {
            let arc = g.clone();
            arc.update_limits(max_calls, max_cps);
            arc
        } else {
            let arc = Arc::new(TrunkCapacityGate::new(max_calls, max_cps));
            self.gates.insert(org_id.to_string(), arc.clone());
            arc
        }
    }

    pub fn try_acquire(
        &self,
        org_id: &str,
        max_calls: Option<u32>,
        max_cps: Option<u32>,
    ) -> AcquireOutcome {
        let gate = self.ensure_gate(org_id, max_calls, max_cps);
        gate.try_acquire()
    }

    pub fn current_active(&self, org_id: &str) -> u32 {
        self.gates
            .get(org_id)
            .map(|g| g.current_active())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_org_always_admits() {
        let state = OrgCapacityState::new();
        assert!(matches!(
            state.try_acquire("acme", None, None),
            AcquireOutcome::Ok(_)
        ));
    }

    #[test]
    fn max_calls_exhausted_rejects_second_call() {
        let state = OrgCapacityState::new();
        let _p1 = match state.try_acquire("acme", Some(1), None) {
            AcquireOutcome::Ok(p) => p,
            other => panic!("expected Ok, got {other:?}"),
        };
        assert!(matches!(
            state.try_acquire("acme", Some(1), None),
            AcquireOutcome::CallsExhausted
        ));
    }

    #[test]
    fn different_orgs_have_independent_gates() {
        let state = OrgCapacityState::new();
        let _p1 = match state.try_acquire("acme", Some(1), None) {
            AcquireOutcome::Ok(p) => p,
            other => panic!("expected Ok, got {other:?}"),
        };
        // A different org's max_calls=1 gate is independent of acme's.
        assert!(matches!(
            state.try_acquire("globex", Some(1), None),
            AcquireOutcome::Ok(_)
        ));
    }

    #[test]
    fn releasing_a_permit_frees_capacity() {
        let state = OrgCapacityState::new();
        let p1 = match state.try_acquire("acme", Some(1), None) {
            AcquireOutcome::Ok(p) => p,
            other => panic!("expected Ok, got {other:?}"),
        };
        drop(p1);
        assert!(matches!(
            state.try_acquire("acme", Some(1), None),
            AcquireOutcome::Ok(_)
        ));
    }
}
