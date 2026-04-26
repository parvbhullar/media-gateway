use crate::proxy::proxy_call::sip_session::SipSessionHandle;
use crate::proxy::trunk_capacity_state::Permit;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ActiveProxyCallStatus {
    Ringing,
    Talking,
}

impl ToString for ActiveProxyCallStatus {
    fn to_string(&self) -> String {
        match self {
            ActiveProxyCallStatus::Ringing => "ringing".to_string(),
            ActiveProxyCallStatus::Talking => "talking".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ActiveProxyCallEntry {
    pub session_id: String,
    pub caller: Option<String>,
    pub callee: Option<String>,
    pub direction: String,
    pub started_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    pub status: ActiveProxyCallStatus,
    /// Phase 5 Plan 05-04: name of the trunk group this call was dispatched
    /// against. None for calls not routed via a trunk group (extension dial,
    /// app, queue without trunk_group resolution). Used by GET
    /// /trunks/{name}/capacity to count live active calls.
    #[serde(default)]
    pub trunk_group_name: Option<String>,
}

#[derive(Default)]
struct RegistryState {
    entries: HashMap<String, ActiveProxyCallEntry>,
    handles: HashMap<String, SipSessionHandle>,
    handles_by_dialog: HashMap<String, SipSessionHandle>,
    // session_id -> all registered dialog_ids (multiple dialogs per session during failover)
    dialog_by_session: HashMap<String, Vec<String>>,
    /// Phase 5 Plan 05-04: capacity Permits keyed on session_id. Stored
    /// out-of-band (NOT inside the cloneable ActiveProxyCallEntry) so that
    /// snapshot reads via list_recent / get never carry a Permit copy.
    /// Removing a session id from this map drops the Permit and its Drop
    /// impl decrements the gate's active counter.
    permits: HashMap<String, Permit>,
}

pub struct ActiveProxyCallRegistry {
    inner: Mutex<RegistryState>,
}

impl ActiveProxyCallRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RegistryState::default()),
        }
    }

    pub fn upsert(&self, entry: ActiveProxyCallEntry, handle: SipSessionHandle) {
        let mut guard = self.inner.lock().unwrap();
        guard.entries.insert(entry.session_id.clone(), entry);
        guard
            .handles
            .insert(handle.session_id().to_string(), handle);
    }

    pub fn register_dialog(&self, dialog_id: String, handle: SipSessionHandle) {
        let mut guard = self.inner.lock().unwrap();
        guard
            .dialog_by_session
            .entry(handle.session_id().to_string())
            .or_default()
            .push(dialog_id.clone());
        guard.handles_by_dialog.insert(dialog_id, handle);
    }

    pub fn unregister_dialog(&self, dialog_id: &str) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(handle) = guard.handles_by_dialog.remove(dialog_id) {
            if let Some(dialogs) = guard.dialog_by_session.get_mut(handle.session_id()) {
                dialogs.retain(|d| d != dialog_id);
                if dialogs.is_empty() {
                    guard.dialog_by_session.remove(handle.session_id());
                }
            }
        }
    }

    pub fn get_handle_by_dialog(&self, dialog_id: &str) -> Option<SipSessionHandle> {
        let guard = self.inner.lock().unwrap();
        guard.handles_by_dialog.get(dialog_id).cloned()
    }

    pub fn update<F>(&self, session_id: &str, updater: F)
    where
        F: FnOnce(&mut ActiveProxyCallEntry),
    {
        if let Some(entry) = self.inner.lock().unwrap().entries.get_mut(session_id) {
            updater(entry);
        }
    }

    pub fn remove(&self, session_id: &str) {
        let mut guard = self.inner.lock().unwrap();
        guard.entries.remove(session_id);
        guard.handles.remove(session_id);
        // Remove all dialog handles registered for this session
        if let Some(dialog_ids) = guard.dialog_by_session.remove(session_id) {
            for dialog_id in dialog_ids {
                guard.handles_by_dialog.remove(&dialog_id);
            }
        }
        // Drop the capacity Permit (RAII releases the trunk's active counter).
        guard.permits.remove(session_id);
    }

    /// Phase 5 Plan 05-04: stash a capacity Permit alongside the call. The
    /// Permit's Drop impl is invoked when `remove(session_id)` is called or
    /// the registry is dropped. The Permit lives outside the cloneable
    /// ActiveProxyCallEntry so list/get snapshots never accidentally extend
    /// the permit lifetime.
    pub fn attach_permit(&self, session_id: &str, permit: Permit) {
        self.inner
            .lock()
            .unwrap()
            .permits
            .insert(session_id.to_string(), permit);
    }

    /// Phase 5 Plan 05-04: count active entries dispatched against a given
    /// trunk group name. Used by GET /api/v1/trunks/{name}/capacity to
    /// surface live `current_active`.
    pub fn count_active_for_trunk(&self, trunk_group_name: &str) -> u32 {
        let guard = self.inner.lock().unwrap();
        guard
            .entries
            .values()
            .filter(|e| e.trunk_group_name.as_deref() == Some(trunk_group_name))
            .count() as u32
    }

    pub fn count(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }

    pub fn list_recent(&self, limit: usize) -> Vec<ActiveProxyCallEntry> {
        let mut entries: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .entries
            .values()
            .cloned()
            .collect();
        entries.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        if entries.len() > limit {
            entries.truncate(limit);
        }
        entries
    }

    pub fn get(&self, session_id: &str) -> Option<ActiveProxyCallEntry> {
        self.inner.lock().unwrap().entries.get(session_id).cloned()
    }

    pub fn get_handle(&self, session_id: &str) -> Option<SipSessionHandle> {
        self.inner.lock().unwrap().handles.get(session_id).cloned()
    }

    /// Get all active session IDs
    pub fn session_ids(&self) -> Vec<String> {
        self.inner.lock().unwrap().entries.keys().cloned().collect()
    }

    /// Alias for count() for SessionRegistry compatibility
    pub fn len(&self) -> usize {
        self.count()
    }

    /// Register a unified session handle
    /// This is used by SipSession to register itself
    pub fn register_handle(&self, session_id: String, handle: SipSessionHandle) {
        let entry = ActiveProxyCallEntry {
            session_id: session_id.clone(),
            caller: None,
            callee: None,
            direction: "inbound".to_string(),
            started_at: Utc::now(),
            answered_at: None,
            status: ActiveProxyCallStatus::Ringing,
            trunk_group_name: None,
        };
        self.upsert(entry, handle);
    }

    #[cfg(test)]
    pub fn handles_by_dialog_count(&self) -> usize {
        self.inner.lock().unwrap().handles_by_dialog.len()
    }

    #[cfg(test)]
    pub fn dialog_by_session_count(&self) -> usize {
        self.inner.lock().unwrap().dialog_by_session.len()
    }

    /// Cleanup stale entries that have been inactive for longer than max_age
    /// Returns the number of entries removed
    pub fn cleanup_stale(&self, max_age: std::time::Duration) -> usize {
        let cutoff = Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_else(|_| chrono::Duration::hours(1));
        let mut guard = self.inner.lock().unwrap();
        
        let stale_ids: Vec<String> = guard
            .entries
            .iter()
            .filter(|(_, entry)| {
                let last_activity = entry.answered_at.unwrap_or(entry.started_at);
                last_activity < cutoff
            })
            .map(|(id, _)| id.clone())
            .collect();
        
        let count = stale_ids.len();
        for id in stale_ids {
            guard.entries.remove(&id);
            guard.handles.remove(&id);
            if let Some(dialog_ids) = guard.dialog_by_session.remove(&id) {
                for dialog_id in dialog_ids {
                    guard.handles_by_dialog.remove(&dialog_id);
                }
            }
        }
        
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::proxy_call::sip_session::SipSession;

    fn make_handle(session_id: &str) -> SipSessionHandle {
        use crate::call::runtime::SessionId;

        let id = SessionId::from(session_id);
        let (handle, _cmd_rx) = SipSession::with_handle(id);
        handle
    }

    fn make_entry(session_id: &str) -> ActiveProxyCallEntry {
        ActiveProxyCallEntry {
            session_id: session_id.to_string(),
            caller: None,
            callee: None,
            direction: "outbound".to_string(),
            started_at: chrono::Utc::now(),
            answered_at: None,
            status: ActiveProxyCallStatus::Ringing,
            trunk_group_name: None,
        }
    }

    fn make_entry_with_trunk(session_id: &str, trunk: Option<&str>) -> ActiveProxyCallEntry {
        let mut e = make_entry(session_id);
        e.trunk_group_name = trunk.map(|s| s.to_string());
        e
    }

    /// Phase 5 Plan 05-04 Task 3 unit test 1.
    #[test]
    fn count_active_for_trunk_returns_zero_when_no_entries() {
        let registry = ActiveProxyCallRegistry::new();
        assert_eq!(registry.count_active_for_trunk("any"), 0);
    }

    /// Phase 5 Plan 05-04 Task 3 unit test 2.
    #[test]
    fn count_active_for_trunk_filters_by_trunk_group_name() {
        let registry = ActiveProxyCallRegistry::new();
        let h1 = make_handle("s1");
        let h2 = make_handle("s2");
        let h3 = make_handle("s3");
        registry.upsert(make_entry_with_trunk("s1", Some("a")), h1);
        registry.upsert(make_entry_with_trunk("s2", Some("a")), h2);
        registry.upsert(make_entry_with_trunk("s3", Some("b")), h3);
        assert_eq!(registry.count_active_for_trunk("a"), 2);
        assert_eq!(registry.count_active_for_trunk("b"), 1);
        assert_eq!(registry.count_active_for_trunk("c"), 0);
    }

    /// Phase 5 Plan 05-04 Task 3 unit test 3: Permit drops on registry remove.
    #[test]
    fn permit_drops_when_entry_removed() {
        use crate::proxy::trunk_capacity_state::{AcquireOutcome, TrunkCapacityGate};
        use std::sync::Arc;
        let registry = ActiveProxyCallRegistry::new();
        let gate = Arc::new(TrunkCapacityGate::new(Some(1), None));
        let permit = match gate.try_acquire() {
            AcquireOutcome::Ok(p) => p,
            _ => panic!("first acquire must succeed"),
        };
        let session = "s-permit";
        let handle = make_handle(session);
        registry.upsert(make_entry(session), handle);
        registry.attach_permit(session, permit);

        // Gate at max — second acquire fails.
        assert!(matches!(gate.try_acquire(), AcquireOutcome::CallsExhausted));

        // Removing the registry entry drops the Permit.
        registry.remove(session);
        assert!(matches!(gate.try_acquire(), AcquireOutcome::Ok(_)));
    }

    /// Phase 5 Plan 05-04 Task 3 unit test 4: cloned entries don't carry Permit.
    /// Permits live in a sibling map keyed by session_id, so cloning the
    /// ActiveProxyCallEntry struct cannot duplicate a Permit. This test
    /// asserts the structural property: a cloned entry's snapshot lifetime
    /// is independent of the gate's active counter.
    #[test]
    fn clone_does_not_clone_permit() {
        use crate::proxy::trunk_capacity_state::{AcquireOutcome, TrunkCapacityGate};
        use std::sync::Arc;
        let registry = ActiveProxyCallRegistry::new();
        let gate = Arc::new(TrunkCapacityGate::new(Some(1), None));
        let permit = match gate.try_acquire() {
            AcquireOutcome::Ok(p) => p,
            _ => panic!(),
        };
        let session = "s-clone";
        let handle = make_handle(session);
        registry.upsert(make_entry(session), handle);
        registry.attach_permit(session, permit);

        // list_recent clones the entries.
        let recent = registry.list_recent(10);
        assert_eq!(recent.len(), 1);
        // The cloned entry exists; the registry still owns the permit.
        // Removing from registry must still release the permit even though
        // a cloned snapshot is alive.
        registry.remove(session);
        drop(recent);
        assert!(matches!(gate.try_acquire(), AcquireOutcome::Ok(_)));
    }

    /// Before fix: dialog_by_session stored only the LAST dialog, so remove() only
    /// cleaned the last entry — all previous handles_by_dialog entries leaked.
    /// After fix: all dialog ids are tracked and fully cleaned on remove().
    #[test]
    fn test_remove_cleans_all_dialog_handles() {
        let registry = ActiveProxyCallRegistry::new();
        let session = "session-1";
        let handle = make_handle(session);
        let entry = make_entry(session);

        // Simulate the sequence that happens during a real call:
        // 1. register_active_call  → registers server dialog
        registry.upsert(entry, handle.clone());
        registry.register_dialog("server-dialog".to_string(), handle.clone());
        assert_eq!(registry.handles_by_dialog_count(), 1);

        // 2. add_callee_dialog (trunk 1 attempt)
        registry.register_dialog("callee-dialog-1".to_string(), handle.clone());
        assert_eq!(registry.handles_by_dialog_count(), 2);

        // 3. add_callee_dialog (failover trunk 2)
        registry.register_dialog("callee-dialog-2".to_string(), handle.clone());
        assert_eq!(registry.handles_by_dialog_count(), 3);

        // All 3 dialogs should be tracked under this session
        assert_eq!(
            registry.inner.lock().unwrap().dialog_by_session[session].len(),
            3
        );

        // 4. Session ends → remove() must clean ALL three handles_by_dialog entries
        registry.remove(session);

        assert_eq!(registry.count(), 0, "entry should be gone");
        assert_eq!(
            registry.handles_by_dialog_count(),
            0,
            "all dialog handles must be cleaned up (was leaking before fix)"
        );
        assert_eq!(
            registry.dialog_by_session_count(),
            0,
            "dialog_by_session must be empty"
        );
    }

    /// Single-trunk call: server dialog + callee dialog → both must be cleaned.
    #[test]
    fn test_single_trunk_call_no_leak() {
        let registry = ActiveProxyCallRegistry::new();
        let session = "session-single";
        let handle = make_handle(session);

        registry.upsert(make_entry(session), handle.clone());
        registry.register_dialog("server-dlg".to_string(), handle.clone());
        registry.register_dialog("callee-dlg".to_string(), handle.clone());

        assert_eq!(registry.handles_by_dialog_count(), 2);

        registry.remove(session);

        assert_eq!(registry.handles_by_dialog_count(), 0);
        assert_eq!(registry.dialog_by_session_count(), 0);
    }

    /// unregister_dialog removes one dialog entry without touching others.
    #[test]
    fn test_unregister_dialog_partial() {
        let registry = ActiveProxyCallRegistry::new();
        let session = "session-partial";
        let handle = make_handle(session);

        registry.upsert(make_entry(session), handle.clone());
        registry.register_dialog("dlg-a".to_string(), handle.clone());
        registry.register_dialog("dlg-b".to_string(), handle.clone());

        // Unregister one
        registry.unregister_dialog("dlg-a");
        assert_eq!(registry.handles_by_dialog_count(), 1, "dlg-b should remain");

        // session still has 1 dialog tracked
        assert_eq!(
            registry.inner.lock().unwrap().dialog_by_session[session].len(),
            1
        );

        // Unregister second
        registry.unregister_dialog("dlg-b");
        assert_eq!(registry.handles_by_dialog_count(), 0);
        // session should be removed from dialog_by_session when empty
        assert_eq!(registry.dialog_by_session_count(), 0);
    }

    /// Multiple concurrent sessions should not interfere with each other.
    #[test]
    fn test_multiple_sessions_independent() {
        let registry = ActiveProxyCallRegistry::new();

        let h1 = make_handle("s1");
        let h2 = make_handle("s2");

        registry.upsert(make_entry("s1"), h1.clone());
        registry.upsert(make_entry("s2"), h2.clone());
        registry.register_dialog("s1-server".to_string(), h1.clone());
        registry.register_dialog("s1-callee".to_string(), h1.clone());
        registry.register_dialog("s2-server".to_string(), h2.clone());
        registry.register_dialog("s2-callee".to_string(), h2.clone());

        assert_eq!(registry.handles_by_dialog_count(), 4);

        // Remove session 1 — session 2 must be intact
        registry.remove("s1");
        assert_eq!(registry.count(), 1, "s2 still active");
        assert_eq!(
            registry.handles_by_dialog_count(),
            2,
            "only s2 dialogs remain"
        );

        registry.remove("s2");
        assert_eq!(registry.count(), 0);
        assert_eq!(registry.handles_by_dialog_count(), 0);
    }
}
