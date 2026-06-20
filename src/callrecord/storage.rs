use crate::config::CallRecordConfig;
use crate::storage::{Storage, StorageConfig};
use anyhow::{Context, Result};
use bytes::Bytes;
use std::path::PathBuf;

// ─── Phase 7 D-06 — call.completed webhook emit (WH-02) ──────────────────
//
// Pure builder + emit helper for the `call.completed` webhook event. The
// `data` field passes the CallRecord JSON through unchanged (D-07 explicit:
// no translation layer — receivers see the same shape Phase 1 storage
// produces). The send is non-fatal: a missing subscriber (no processor
// running, e.g. early boot) is silently ignored so finalize never fails on
// webhook plumbing.

use crate::callrecord::CallRecord;
use crate::proxy::webhook::{
    WebhookEvent, WebhookEventSender, current_unix_timestamp, derive_event_id,
};

/// Build the `call.completed` envelope per D-07. `data` = full CallRecord
/// JSON (existing serializer, no translation).
pub fn build_call_completed_event(record: &CallRecord) -> WebhookEvent {
    let mut data = match serde_json::to_value(record) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("call.completed serialize failed: {}", e);
            serde_json::Value::Null
        }
    };
    // task 2.3: surface billable (answered) seconds in the payload (NULL when
    // unanswered) so unpod bills off this, not the wall-clock duration.
    if let Some(obj) = data.as_object_mut() {
        let billable = record
            .answer_time
            .map(|ans| (record.end_time - ans).num_seconds().max(0));
        obj.insert(
            "billable_duration_secs".to_string(),
            serde_json::json!(billable),
        );
    }
    WebhookEvent {
        // task 2.2: stable id keyed on the call so a redelivery dedups (the
        // call.completed event is the one unpod bills + dedups on).
        event_id: derive_event_id(&record.call_id, "call.completed"),
        event: "call.completed".to_string(),
        timestamp: current_unix_timestamp(),
        data,
    }
}

/// Emit `call.completed` over the broadcast channel. Returns `true` when at
/// least one subscriber received the event (broadcast `send` returns the
/// active-receiver count). Send errors (no subscribers) are silently
/// ignored — a missing processor is non-fatal per D-06.
pub fn emit_call_completed(
    sender: Option<&WebhookEventSender>,
    record: &CallRecord,
) -> bool {
    if let Some(s) = sender {
        let event = build_call_completed_event(record);
        return s.send(event).is_ok();
    }
    false
}

#[derive(Clone)]
pub struct CdrStorage {
    inner: Storage,
}

impl CdrStorage {
    pub fn new(storage: Storage) -> Self {
        Self { inner: storage }
    }

    pub fn is_local(&self) -> bool {
        self.inner.is_local()
    }

    pub fn local_full_path(&self, path: &str) -> Option<PathBuf> {
        self.inner.local_path(path)
    }

    pub fn path_for_metadata(&self, path: &str) -> String {
        if self.is_local() {
            self.inner
                .local_path(path)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        } else {
            path.to_string()
        }
    }

    pub async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<String> {
        self.inner
            .write(path, Bytes::copy_from_slice(bytes))
            .await?;
        Ok(path.to_string())
    }

    pub async fn read_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let bytes = self.inner.read(path).await?;
        Ok(bytes.to_vec())
    }

    pub async fn read_to_string(&self, path: &str) -> Result<String> {
        let bytes = self.read_bytes(path).await?;
        String::from_utf8(bytes).with_context(|| format!("decode UTF-8 for {}", path))
    }
}

pub fn resolve_storage(config: Option<&CallRecordConfig>) -> Result<Option<CdrStorage>> {
    match config {
        Some(CallRecordConfig::Local { root }) => {
            let storage_config = StorageConfig::Local { path: root.clone() };
            let storage = Storage::new(&storage_config)?;
            Ok(Some(CdrStorage::new(storage)))
        }
        Some(CallRecordConfig::S3 {
            vendor,
            bucket,
            region,
            access_key,
            secret_key,
            endpoint,
            root,
            ..
        }) => {
            let storage_config = StorageConfig::S3 {
                vendor: vendor.clone(),
                bucket: bucket.clone(),
                region: region.clone(),
                access_key: access_key.clone(),
                secret_key: secret_key.clone(),
                endpoint: Some(endpoint.clone()),
                prefix: Some(root.clone()),
            };
            let storage = Storage::new(&storage_config)?;
            Ok(Some(CdrStorage::new(storage)))
        }
        Some(CallRecordConfig::Http { .. }) => Ok(None),
        Some(CallRecordConfig::Database { .. }) => Ok(None),
        None => Ok(None),
    }
}

#[cfg(test)]
mod webhook_emit_tests {
    use super::*;
    use crate::callrecord::CallRecord;
    use chrono::Utc;

    fn fixture_record() -> CallRecord {
        let mut rec = CallRecord::default();
        rec.call_id = "call-abc".to_string();
        rec
    }

    #[test]
    fn build_call_completed_event_uses_d07_envelope() {
        let rec = fixture_record();
        let _ = Utc::now();
        // Diagnostic: confirm the serializer produces a non-null body.
        let raw = serde_json::to_value(&rec).expect("serialize CallRecord");
        assert!(raw.is_object(), "expected object, got {:?}", raw);
        let ev = build_call_completed_event(&rec);
        assert_eq!(ev.event, "call.completed");
        assert!(ev.event_id.starts_with("evt_"));
        assert!(ev.timestamp > 0);
        // CallRecord uses #[serde(rename_all = "camelCase")] so call_id → callId.
        assert_eq!(ev.data["callId"], "call-abc");
    }

    #[test]
    fn build_call_completed_event_sets_billable_when_answered() {
        // billable = ended − answered = 15s, NOT the 20s wall-clock span.
        let mut rec = fixture_record();
        let start = Utc::now();
        rec.start_time = start;
        rec.answer_time = Some(start + chrono::Duration::seconds(5));
        rec.end_time = start + chrono::Duration::seconds(20);
        let ev = build_call_completed_event(&rec);
        assert_eq!(ev.data["billable_duration_secs"], 15);
    }

    #[test]
    fn build_call_completed_event_billable_null_when_unanswered() {
        let mut rec = fixture_record();
        let start = Utc::now();
        rec.start_time = start;
        rec.answer_time = None;
        rec.end_time = start + chrono::Duration::seconds(20);
        let ev = build_call_completed_event(&rec);
        assert!(ev.data["billable_duration_secs"].is_null());
    }

    #[test]
    fn emit_with_no_sender_is_noop_returns_false() {
        let rec = fixture_record();
        assert!(!emit_call_completed(None, &rec));
    }

    #[test]
    fn emit_with_sender_no_subscribers_returns_false_silently() {
        // Broadcast send returns Err when there are zero receivers; emit
        // helper swallows it (non-fatal per D-06).
        let (sender, _) = tokio::sync::broadcast::channel::<WebhookEvent>(16);
        let rec = fixture_record();
        // No subscribers attached.
        assert!(!emit_call_completed(Some(&sender), &rec));
    }

    #[test]
    fn emit_with_subscriber_delivers_event() {
        let (sender, mut rx) = tokio::sync::broadcast::channel::<WebhookEvent>(16);
        let rec = fixture_record();
        let delivered = emit_call_completed(Some(&sender), &rec);
        assert!(delivered, "send should report 1 subscriber");
        let ev = rx.try_recv().expect("event delivered");
        assert_eq!(ev.event, "call.completed");
        assert_eq!(ev.data["callId"], "call-abc");
    }
}
