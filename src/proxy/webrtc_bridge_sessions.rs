//! Per-dialog state for active SIP↔WebRTC bridge sessions.
//!
//! When the routing matcher resolves an inbound INVITE to a `kind="webrtc"`
//! trunk and `proxy/call.rs` drives the bridge dispatcher, the resulting
//! [`BridgePeer`] + signaling session must outlive the INVITE transaction —
//! they live until BYE (or transport failure) tears the dialog down. This
//! module is the side-table keyed by [`DialogId`] where that state lives
//! between INVITE-time setup and BYE-time teardown.
//!
//! The regular SIP forward path uses `proxy::active_call_registry`
//! (`ActiveProxyCallRegistry`) for the same kind of bookkeeping, but those
//! entries carry a `SipSession` handle — something the WebRTC bridge path
//! deliberately doesn't construct (it short-circuits the full SIP forward
//! machinery). Hence a dedicated, much smaller registry here.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rsipstack::dialog::DialogId;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::callrecord::{
    CallDetails, CallRecord, CallRecordHangupReason, CallRecordLastError, CallRecordSender,
    LegTimeline,
};
use crate::media::bridge::BridgePeer;
use crate::proxy::bridge::signaling::{SessionHandle, WebRtcSignalingAdapter};

/// State pinned for the lifetime of a SIP dialog whose INVITE was bridged to
/// a WebRTC trunk. On BYE we remove the entry, call `adapter.close(...)`,
/// and drop the `Arc<BridgePeer>` — its [`Drop`] impl cancels the media
/// forwarding tasks via the bridge's internal cancellation token.
pub struct WebRtcBridgeSession {
    /// The wired bridge. Dropping the last clone closes both PeerConnections
    /// and the forwarding tasks shut down via `cancel_token`.
    pub bridge: Arc<BridgePeer>,
    /// Adapter used to negotiate the WebRTC leg — kept for the teardown
    /// `close(&ctx, &session)` call.
    pub adapter: Arc<dyn WebRtcSignalingAdapter>,
    /// Adapter-defined session blob echoed back on close.
    pub session: SessionHandle,
    /// Endpoint URL captured from the trunk's `kind_config` at INVITE time —
    /// preserved verbatim so the teardown `SignalingContext` matches the one
    /// used at `negotiate` time.
    pub endpoint_url: String,
    /// Auth header captured from the trunk's `kind_config` at INVITE time.
    pub auth_header: Option<String>,
    /// Capacity-gate permit acquired before dispatch. Dropping it at BYE
    /// time releases the slot back to the trunk's concurrent-call budget.
    /// `None` when both `max_concurrent` and `max_cps` were unset (no
    /// limits to enforce → no gate created).
    pub _permit: Option<crate::proxy::trunk_capacity_state::Permit>,

    // --- CDR fields ---------------------------------------------------------
    // Populated at dispatch time and consumed by BYE-time teardown to emit
    // a CallRecord on the existing CDR pipeline so bridge calls show up in
    // billing/audit reports alongside regular SIP calls.
    /// SIP Call-ID of the originating INVITE — used as the CDR `call_id`.
    pub call_id: String,
    /// Caller URI (From: header) captured from the INVITE.
    pub caller_uri: String,
    /// Callee URI (Request-URI / To: header) captured from the INVITE.
    pub callee_uri: String,
    /// User-part of the From URI, surfaced as `from_number`.
    pub from_number: Option<String>,
    /// User-part of the Request-URI / To URI, surfaced as `to_number`.
    pub to_number: Option<String>,
    /// Resolved trunk name.
    pub trunk_name: String,
    /// Resolved trunk DB id. Populates `rustpbx_call_records.sip_trunk_id`.
    pub trunk_id: Option<i64>,
    /// Dispatch time — becomes the CDR `start_time` / `answer_time`.
    pub start_time: DateTime<Utc>,
}

/// Inputs to [`emit_bridge_call_record`]. Grouped in a struct so the
/// call-sites in `proxy/call.rs` stay readable.
pub struct BridgeCallRecordInfo {
    pub call_id: String,
    pub caller_uri: String,
    pub callee_uri: String,
    pub from_number: Option<String>,
    pub to_number: Option<String>,
    pub trunk_name: String,
    pub trunk_id: Option<i64>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub status_code: u16,
    pub hangup_reason: CallRecordHangupReason,
    /// Free-form reason text for failure dispositions; surfaces in
    /// `CallRecordLastError.reason`.
    pub last_error_reason: Option<String>,
}

/// Emit a CDR row for a webrtc-bridge dialog. The CDR pipeline is an
/// actor reachable through an unbounded `CallRecordSender`; a closed
/// channel logs a warn and is otherwise swallowed — a CDR write must
/// never fail the call.
pub fn emit_bridge_call_record(
    sender: Option<&CallRecordSender>,
    info: &BridgeCallRecordInfo,
) {
    let Some(sender) = sender else {
        return;
    };

    let mut metadata = HashMap::new();
    metadata.insert("call_type".to_string(), "webrtc_bridge".to_string());
    metadata.insert("trunk_name".to_string(), info.trunk_name.clone());

    let last_error = info
        .last_error_reason
        .as_ref()
        .map(|reason| CallRecordLastError {
            code: info.status_code,
            reason: Some(reason.clone()),
        });

    let details = CallDetails {
        direction: "outbound".to_string(),
        status: if (200..300).contains(&info.status_code) {
            "completed".to_string()
        } else {
            "failed".to_string()
        },
        from_number: info.from_number.clone(),
        to_number: info.to_number.clone(),
        sip_trunk_id: info.trunk_id,
        metadata: Some(metadata),
        last_error,
        ..Default::default()
    };

    let record = CallRecord {
        call_id: info.call_id.clone(),
        start_time: info.start_time,
        ring_time: None,
        answer_time: if (200..300).contains(&info.status_code) {
            Some(info.start_time)
        } else {
            None
        },
        end_time: info.end_time,
        caller: info.caller_uri.clone(),
        callee: info.callee_uri.clone(),
        status_code: info.status_code,
        hangup_reason: Some(info.hangup_reason.clone()),
        hangup_messages: Vec::new(),
        recorder: Vec::new(),
        sip_leg_roles: HashMap::new(),
        leg_timeline: LegTimeline::default(),
        details,
        extensions: http::Extensions::new(),
    };

    if sender.send(record).is_err() {
        warn!(call_id = %info.call_id, "webrtc bridge CDR send failed (channel closed)");
    }
}

/// Process-wide registry of active webrtc-bridged dialogs.
#[derive(Default)]
pub struct WebRtcBridgeSessions {
    inner: RwLock<HashMap<DialogId, WebRtcBridgeSession>>,
}

impl WebRtcBridgeSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stash a freshly-built bridge session, keyed by the server-side dialog
    /// id of the originating INVITE. Replaces any existing entry under the
    /// same key (which would only happen for a re-used Call-ID — pathological
    /// but not a panic-worthy condition).
    pub async fn insert(&self, dialog_id: DialogId, session: WebRtcBridgeSession) {
        let mut guard = self.inner.write().await;
        // On replace, the net active count is unchanged — only increment when
        // the key is genuinely new.
        let replaced = guard.insert(dialog_id.clone(), session).is_some();
        if replaced {
            debug!(%dialog_id, "WebRtcBridgeSessions: replaced existing entry");
        } else {
            crate::metrics::bridge::inc_active_sessions();
        }
    }

    /// Remove and return the session for `dialog_id`, if any.
    pub async fn remove(&self, dialog_id: &DialogId) -> Option<WebRtcBridgeSession> {
        let mut guard = self.inner.write().await;
        let popped = guard.remove(dialog_id);
        if popped.is_some() {
            crate::metrics::bridge::dec_active_sessions();
        }
        popped
    }

    /// Returns `true` iff a session is currently stashed for `dialog_id`.
    /// Used by the BYE/CANCEL fast-path to decide whether to short-circuit
    /// the dialog-layer dispatch.
    pub async fn contains(&self, dialog_id: &DialogId) -> bool {
        self.inner.read().await.contains_key(dialog_id)
    }

    /// Number of live entries (used by tests and diagnostics).
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn dialog_id(call: &str) -> DialogId {
        DialogId {
            call_id: call.into(),
            local_tag: "lt".into(),
            remote_tag: "rt".into(),
        }
    }

    struct NoopAdapter;
    #[async_trait::async_trait]
    impl WebRtcSignalingAdapter for NoopAdapter {
        async fn negotiate(
            &self,
            _ctx: &crate::proxy::bridge::signaling::SignalingContext,
            _offer_sdp: &str,
        ) -> Result<
            crate::proxy::bridge::signaling::NegotiateOutcome,
            crate::proxy::bridge::signaling::SignalingError,
        > {
            unreachable!("test stub")
        }
    }

    fn fake_session() -> WebRtcBridgeSession {
        use rustrtc::{
            PeerConnection, RtcConfiguration, TransportMode,
            config::{AudioCapability, MediaCapabilities, SdpCompatibilityMode, VideoCapability},
        };
        let mk = || {
            PeerConnection::new(RtcConfiguration {
                transport_mode: TransportMode::Rtp,
                media_capabilities: Some(MediaCapabilities {
                    audio: vec![AudioCapability::pcmu()],
                    video: Vec::<VideoCapability>::new(),
                    application: None,
                }),
                sdp_compatibility: SdpCompatibilityMode::Standard,
                ..Default::default()
            })
        };
        let bridge = Arc::new(BridgePeer::new("test".into(), mk(), mk()));
        WebRtcBridgeSession {
            bridge,
            adapter: Arc::new(NoopAdapter),
            session: SessionHandle(Value::Null),
            endpoint_url: "http://127.0.0.1:1/offer".into(),
            auth_header: None,
            _permit: None,
            call_id: "test-call".into(),
            caller_uri: "sip:alice@example.com".into(),
            callee_uri: "sip:bot@example.com".into(),
            from_number: Some("alice".into()),
            to_number: Some("bot".into()),
            trunk_name: "trunk-test".into(),
            trunk_id: Some(42),
            start_time: Utc::now(),
        }
    }

    #[tokio::test]
    async fn emit_bridge_call_record_completed() {
        let (sender, mut receiver) =
            tokio::sync::mpsc::unbounded_channel::<CallRecord>();
        let start = Utc::now() - chrono::Duration::seconds(7);
        emit_bridge_call_record(
            Some(&sender),
            &BridgeCallRecordInfo {
                call_id: "lifecycle-call".into(),
                caller_uri: "sip:1001@pbx.example.com".into(),
                callee_uri: "sip:bot42@bots.example.com".into(),
                from_number: Some("1001".into()),
                to_number: Some("bot42".into()),
                trunk_name: "webrtc-bot".into(),
                trunk_id: Some(99),
                start_time: start,
                end_time: Utc::now(),
                status_code: 200,
                hangup_reason: CallRecordHangupReason::ByCaller,
                last_error_reason: None,
            },
        );
        let record = receiver.recv().await.expect("CDR record sent");
        assert_eq!(record.call_id, "lifecycle-call");
        assert_eq!(record.status_code, 200);
        assert_eq!(record.details.status, "completed");
        assert_eq!(record.details.direction, "outbound");
        assert_eq!(record.details.sip_trunk_id, Some(99));
        let md = record.details.metadata.as_ref().unwrap();
        assert_eq!(md.get("call_type").map(String::as_str), Some("webrtc_bridge"));
        assert_eq!(md.get("trunk_name").map(String::as_str), Some("webrtc-bot"));
    }

    #[tokio::test]
    async fn emit_bridge_call_record_failed_dispatch() {
        let (sender, mut receiver) =
            tokio::sync::mpsc::unbounded_channel::<CallRecord>();
        emit_bridge_call_record(
            Some(&sender),
            &BridgeCallRecordInfo {
                call_id: "failed-call".into(),
                caller_uri: "sip:a@x".into(),
                callee_uri: "sip:b@y".into(),
                from_number: Some("a".into()),
                to_number: Some("b".into()),
                trunk_name: "webrtc-bot".into(),
                trunk_id: Some(7),
                start_time: Utc::now(),
                end_time: Utc::now(),
                status_code: 503,
                hangup_reason: CallRecordHangupReason::ServerUnavailable,
                last_error_reason: Some("trunk concurrent-call cap reached".into()),
            },
        );
        let record = receiver.recv().await.expect("CDR record sent");
        assert_eq!(record.status_code, 503);
        assert_eq!(record.details.status, "failed");
        assert!(record.answer_time.is_none());
        let last_err = record.details.last_error.as_ref().expect("last_error set");
        assert_eq!(last_err.code, 503);
        assert_eq!(last_err.reason.as_deref(), Some("trunk concurrent-call cap reached"));
    }

    #[tokio::test]
    async fn insert_then_remove_roundtrips() {
        let reg = WebRtcBridgeSessions::new();
        let id = dialog_id("abc");
        assert_eq!(reg.len().await, 0);
        reg.insert(id.clone(), fake_session()).await;
        assert_eq!(reg.len().await, 1);
        let popped = reg.remove(&id).await;
        assert!(popped.is_some(), "expected entry to be present");
        assert_eq!(reg.len().await, 0);
        assert!(
            reg.remove(&id).await.is_none(),
            "second remove must be None"
        );
    }
}
