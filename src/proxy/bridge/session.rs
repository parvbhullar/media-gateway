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
    /// SIP terminated in rustpbx; decoded PCM piped to a co-located sidecar
    /// process over localhost UDP (sidecar owns the far-end media, e.g. a
    /// LiveKit room participant). See `proxy::bridge::external_media`.
    ExternalMedia,
}

impl BridgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WebRtc => "webrtc",
            Self::LiveKit => "livekit",
            Self::ExternalMedia => "external_media",
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

    /// Attach a shared call recorder before `start()` is called. The
    /// default impl is a no-op; only kinds with a `BridgePeer`-backed
    /// data plane (currently WebRTC) wire it through. Must be called
    /// before `start()` so the forwarder picks the recorder up when it
    /// spawns.
    fn attach_recorder(
        &self,
        _recorder: std::sync::Arc<parking_lot::RwLock<Option<crate::media::recorder::Recorder>>>,
    ) {
    }

    /// Future that resolves when the media-plane bridge has internally
    /// signalled end-of-call (e.g. LiveKit `RoomEvent::Disconnected`,
    /// the no-bot watchdog firing, etc.). The future's output is the
    /// cause that should be recorded in the CDR. The default impl
    /// never resolves — WebRTC kind has no such signal, SIP BYE drives
    /// teardown there. LiveKit overrides to expose its
    /// `cancel_token.cancelled()` future + a side-channel cause.
    fn watch_disconnect(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = BridgeHangupCause> + Send + '_>>
    {
        Box::pin(std::future::pending())
    }
}

/// Why a bridged dialog was torn down. Set by the media-plane bridge
/// when it signals end-of-call, threaded through the CDR emit path so
/// the final record reflects who hung up (or which watchdog fired).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeHangupCause {
    /// SIP-side BYE (carrier or caller initiated).
    ByCaller,
    /// WebRTC-side disconnect / bot-initiated teardown.
    ByCallee,
    /// adapter.close() / room.close() errored at teardown time.
    TeardownFailed,
    /// LiveKit `bot_join_timeout_ms` watchdog fired — no remote
    /// participant had subscribed an audio track within the deadline.
    BotJoinTimeout,
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
