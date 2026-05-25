//! Shared abstractions over the two bridge kinds (`webrtc`, `livekit`).
//!
//! `MediaBridge` is the media-plane half — owns the WebRTC PCs / LiveKit
//! tracks and the forwarding tasks. Dropping the last Arc cancels all
//! tasks via the bridge's internal CancellationToken.
//!
//! `BridgeTeardown` is the signaling-plane half — tells the remote side
//! to tear down. Errors are logged-but-never-propagated; the SIP BYE
//! always succeeds.

use async_trait::async_trait;
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BridgeKind {
    WebRtc,
    LiveKit,
}

impl BridgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WebRtc => "webrtc",
            Self::LiveKit => "livekit",
        }
    }
}

#[derive(Debug, Error)]
pub enum TeardownError {
    #[error("remote close failed: {0}")]
    Remote(String),
}

#[async_trait]
pub trait MediaBridge: Send + Sync + 'static {
    /// Kick off forwarding tasks. Called once after the session is stashed
    /// in the registry, so a racing BYE can find the entry.
    async fn start(&self) -> anyhow::Result<()>;
    fn kind(&self) -> BridgeKind;

    /// Future that resolves when the media-plane bridge has internally
    /// signalled end-of-call (e.g. LiveKit `RoomEvent::Disconnected`). The
    /// default impl never resolves — WebRTC kind has no such signal, SIP
    /// BYE drives teardown there. LiveKit overrides to expose its
    /// `cancel_token.cancelled()` future, which call.rs watches to drive
    /// SIP-side teardown when the LiveKit room ends the session.
    fn watch_disconnect(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

#[async_trait]
pub trait BridgeTeardown: Send + Sync + 'static {
    async fn close(&self) -> Result<(), TeardownError>;
}

/// Successful dispatch outcome for either kind.
pub struct DispatchOutcome {
    /// SDP answer for the inbound SIP 200 OK.
    pub sip_sdp_answer: String,
    /// Bridge media-plane handle. Stash on the session; dropping last
    /// clone cancels forwarders.
    pub bridge: std::sync::Arc<dyn MediaBridge>,
    /// Teardown signaling-plane action. Stash on the session.
    pub teardown: Box<dyn BridgeTeardown>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_kind_as_str() {
        assert_eq!(BridgeKind::WebRtc.as_str(), "webrtc");
        assert_eq!(BridgeKind::LiveKit.as_str(), "livekit");
    }

    #[test]
    fn bridge_kind_round_trips_through_clone_copy() {
        let k = BridgeKind::WebRtc;
        let k2 = k;
        assert_eq!(k, k2);
    }
}
