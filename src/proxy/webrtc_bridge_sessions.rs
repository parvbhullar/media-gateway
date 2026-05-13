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

use rsipstack::dialog::DialogId;
use tokio::sync::RwLock;
use tracing::debug;

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
        if guard.insert(dialog_id.clone(), session).is_some() {
            debug!(%dialog_id, "WebRtcBridgeSessions: replaced existing entry");
        }
    }

    /// Remove and return the session for `dialog_id`, if any.
    pub async fn remove(&self, dialog_id: &DialogId) -> Option<WebRtcBridgeSession> {
        let mut guard = self.inner.write().await;
        guard.remove(dialog_id)
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
        }
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
