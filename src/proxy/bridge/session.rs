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

    /// Await the trunk's media-commitment signal before the pre-answer
    /// engine sends the 200 OK (see the `bridge-pre-answer` capability).
    ///
    /// The default impl resolves immediately — correct for `livekit` and
    /// `external_media`, whose dispatchers already block until the bot has
    /// joined / the sidecar is READY before returning a [`DispatchOutcome`].
    /// The `webrtc` kind overrides this to wait for the bot PeerConnection
    /// to reach ICE/DTLS-connected (`answer_on = ice_connected`, the
    /// default) or resolves immediately for `answer_on = signaling`.
    ///
    /// Returns `Err` if the bot side fails before committing (e.g. the PC
    /// reaches Failed/Closed). Timeout is NOT enforced here — the engine
    /// races this future against the trunk's ring timeout and rejects with
    /// 480 on expiry.
    async fn await_media_commitment(
        &self,
        _answer_on: crate::models::trunk::AnswerOn,
    ) -> anyhow::Result<()> {
        Ok(())
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

    /// Stop the media plane immediately, without waiting for the last `Arc` to
    /// drop. Called by the liveness teardown so a disconnect-watcher holding an
    /// `Arc` clone releases it (its `watch_disconnect` resolves once the media
    /// plane is cancelled). Default no-op — kinds whose `teardown.close()`
    /// already stops the media plane (LiveKit room close, external_media
    /// sidecar kill) don't need it; WebRTC overrides to cancel its
    /// `BridgePeer` forwarders (adapter close only tears down signaling).
    fn shutdown(&self) {}

    /// Start the media-inactivity timeout for this bridge (no RTP within the
    /// configured windows → cancel + `MediaTimeout`). Default no-op; the WebRTC
    /// bridge polls its `BridgePeer` packet counters. A `0` window disables it.
    fn start_media_timeout(
        &self,
        _initial: std::time::Duration,
        _rolling: std::time::Duration,
    ) {
    }
}

/// Why a bridged dialog was torn down. Set by the media-plane bridge
/// when it signals end-of-call, threaded through the CDR emit path so
/// the final record reflects who hung up (or which watchdog fired).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeHangupCause {
    /// SIP-side BYE (carrier or caller initiated). No BYE is sent back — the
    /// carrier hung up.
    ByCaller,
    /// Bot-side clean disconnect (WebRTC PC closed, LiveKit room disconnect,
    /// external-media sidecar BYE datagram). A BYE is sent to the carrier.
    ByCallee,
    /// Bot-side abnormal loss (PC Failed, ICE/DTLS failure, sidecar crash).
    /// A BYE is sent to the carrier.
    BotLost,
    /// Media-inactivity timeout fired (no RTP within the configured window).
    /// A BYE is sent to the carrier.
    MediaTimeout,
    /// adapter.close() / room.close() errored at teardown time.
    TeardownFailed,
    /// LiveKit `bot_join_timeout_ms` watchdog fired — no remote
    /// participant had subscribed an audio track within the deadline.
    BotJoinTimeout,
}

impl BridgeHangupCause {
    /// Whether the gateway should send a BYE toward the carrier for this
    /// cause. Bot-initiated terminations require us to notify the carrier;
    /// caller-initiated (they sent the BYE) and transport-failure causes do
    /// not.
    pub fn should_send_carrier_bye(self) -> bool {
        matches!(self, Self::ByCallee | Self::BotLost | Self::MediaTimeout)
    }

    /// Q.850-style reason text for a gateway-initiated BYE.
    pub fn bye_reason(self) -> &'static str {
        match self {
            Self::ByCallee => "SIP;cause=16;text=\"bot hangup\"",
            Self::BotLost => "SIP;cause=41;text=\"bot connection lost\"",
            Self::MediaTimeout => "SIP;cause=41;text=\"media timeout\"",
            _ => "SIP;cause=16;text=\"normal clearing\"",
        }
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

    #[test]
    fn only_bot_initiated_causes_send_a_carrier_bye() {
        // Bot ended the call → we must notify the carrier.
        assert!(BridgeHangupCause::ByCallee.should_send_carrier_bye());
        assert!(BridgeHangupCause::BotLost.should_send_carrier_bye());
        assert!(BridgeHangupCause::MediaTimeout.should_send_carrier_bye());
        // Caller/transport-failure causes must NOT emit a BYE.
        assert!(!BridgeHangupCause::ByCaller.should_send_carrier_bye());
        assert!(!BridgeHangupCause::TeardownFailed.should_send_carrier_bye());
        assert!(!BridgeHangupCause::BotJoinTimeout.should_send_carrier_bye());
    }

    #[test]
    fn bye_reason_carries_a_q850_cause() {
        assert!(BridgeHangupCause::BotLost.bye_reason().contains("cause=41"));
        assert!(BridgeHangupCause::MediaTimeout.bye_reason().contains("cause=41"));
        assert!(BridgeHangupCause::ByCallee.bye_reason().contains("cause=16"));
    }
}
