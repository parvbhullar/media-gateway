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
use dashmap::DashMap;
use rsipstack::dialog::DialogId;
use tracing::{debug, warn};

use crate::callrecord::{
    CallDetails, CallRecord, CallRecordHangupReason, CallRecordLastError, CallRecordSender,
    LegTimeline,
};
use crate::media::bridge::BridgePeer;
use crate::proxy::bridge::signaling::{SessionHandle, SignalingContext, WebRtcSignalingAdapter};

/// Why a webrtc bridge dialog was torn down. Threaded into
/// [`emit_bridge_call_record`] so the final CDR reflects who hung up.
#[derive(Debug, Clone, Copy)]
pub enum BridgeHangupCause {
    /// SIP-side BYE (carrier or caller initiated).
    ByCaller,
    /// WebRTC-side disconnect / bot-initiated teardown.
    ByCallee,
    /// adapter.close() errored at teardown time.
    TeardownFailed,
}

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
    /// Signaling context captured verbatim from the dispatch call —
    /// preserves endpoint_url, auth_header, timeout_ms, AND the adapter
    /// `protocol` blob, so `adapter.close()` sees exactly what
    /// `adapter.negotiate()` saw. Replaces the older endpoint_url +
    /// auth_header pair which silently dropped the protocol blob.
    pub ctx: SignalingContext,
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
    /// Dispatch time — becomes the CDR `start_time`. PDD is `answer_time -
    /// start_time`.
    pub start_time: DateTime<Utc>,
    /// Wall-clock time the 200 OK was sent back to the SIP carrier — the
    /// real `answer_time` for the CDR.
    pub answer_time: DateTime<Utc>,
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
    /// CDR direction. WebRTC-bridge calls arrive as INVITEs on the SIP
    /// carrier — from the gateway's perspective these are inbound calls
    /// being terminated to a WebRTC bot. Default is "inbound".
    pub direction: BridgeCallDirection,
    /// When 200 OK was actually sent to the carrier — populates
    /// `answer_time`. Falls back to `start_time` when unset (used by
    /// failure-path CDRs where no 200 OK was ever issued).
    pub answer_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum BridgeCallDirection {
    #[default]
    Inbound,
    Outbound,
}

impl BridgeCallDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
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
        direction: info.direction.as_str().to_string(),
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
            Some(info.answer_time.unwrap_or(info.start_time))
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
///
/// Backed by `DashMap` — every in-dialog SIP request (BYE/CANCEL/ACK/INFO/
/// UPDATE/OPTIONS) hits `contains()` on this registry, and a tokio
/// `RwLock<HashMap>` would force every one of those requests through an
/// awaitable write lock on insert/remove, serializing them across cores.
/// `DashMap` shards the hash space so unrelated dialogs don't contend.
#[derive(Default)]
pub struct WebRtcBridgeSessions {
    inner: DashMap<DialogId, WebRtcBridgeSession>,
}

impl WebRtcBridgeSessions {
    pub fn new() -> Self {
        Self::default()
    }

    // ---------------------------------------------------------------------
    // API note: the methods below are `async fn` but internally synchronous —
    // `DashMap` does its own sharded locking without yielding. The async
    // signatures are preserved for API consistency with sibling registries
    // (e.g. `ActiveProxyCallRegistry`) and so a future swap to a genuinely
    // async backing store (Redis, distributed cache, etc.) won't churn every
    // call site.
    //
    // SAFETY: do NOT hold a DashMap entry/ref guard across a real `.await`
    // inside these methods or their callers. The inner locks are
    // `parking_lot::RwLock`s — they cannot yield, and awaiting while one is
    // held risks deadlocking the runtime worker. The current call sites
    // only ever take an entry, read/write it, and let it drop before any
    // `.await`, which is correct.
    // ---------------------------------------------------------------------

    /// Stash a freshly-built bridge session, keyed by the server-side dialog
    /// id of the originating INVITE. Replaces any existing entry under the
    /// same key (which would only happen for a re-used Call-ID — pathological
    /// but not a panic-worthy condition).
    pub async fn insert(&self, dialog_id: DialogId, session: WebRtcBridgeSession) {
        // On replace, the net active count is unchanged — only increment when
        // the key is genuinely new.
        let replaced = self.inner.insert(dialog_id.clone(), session).is_some();
        if replaced {
            debug!(%dialog_id, "WebRtcBridgeSessions: replaced existing entry");
        } else {
            crate::metrics::bridge::inc_active_sessions();
        }
    }

    /// Remove and return the session for `dialog_id`, if any.
    pub async fn remove(&self, dialog_id: &DialogId) -> Option<WebRtcBridgeSession> {
        let popped = self.inner.remove(dialog_id).map(|(_, v)| v);
        if popped.is_some() {
            crate::metrics::bridge::dec_active_sessions();
        }
        popped
    }

    /// Returns `true` iff a session is currently stashed for `dialog_id`.
    /// Used by the BYE/CANCEL fast-path to decide whether to short-circuit
    /// the dialog-layer dispatch.
    pub async fn contains(&self, dialog_id: &DialogId) -> bool {
        self.inner.contains_key(dialog_id)
    }

    /// Number of live entries (used by tests and diagnostics).
    pub async fn len(&self) -> usize {
        self.inner.len()
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
            ctx: SignalingContext {
                endpoint_url: "http://127.0.0.1:1/offer".into(),
                auth_header: None,
                timeout_ms: 5_000,
                protocol: None,
            },
            _permit: None,
            call_id: "test-call".into(),
            caller_uri: "sip:alice@example.com".into(),
            callee_uri: "sip:bot@example.com".into(),
            from_number: Some("alice".into()),
            to_number: Some("bot".into()),
            trunk_name: "trunk-test".into(),
            trunk_id: Some(42),
            start_time: Utc::now(),
            answer_time: Utc::now(),
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
                direction: BridgeCallDirection::Inbound,
                answer_time: None,
            },
        );
        let record = receiver.recv().await.expect("CDR record sent");
        assert_eq!(record.call_id, "lifecycle-call");
        assert_eq!(record.status_code, 200);
        assert_eq!(record.details.status, "completed");
        assert_eq!(record.details.direction, "inbound");
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
                direction: BridgeCallDirection::Inbound,
                answer_time: None,
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
