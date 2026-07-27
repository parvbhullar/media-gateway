//! WebRtcMediaBridge — wraps the existing Arc<BridgePeer> so it satisfies
//! the kind-agnostic MediaBridge trait.

use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use rustrtc::PeerConnectionState;

use crate::media::bridge::BridgePeer;
use crate::models::trunk::AnswerOn;
use crate::proxy::bridge::session::{BridgeHangupCause, BridgeKind, MediaBridge};

pub struct WebRtcMediaBridge(pub Arc<BridgePeer>);

impl WebRtcMediaBridge {
    /// The hangup cause to report when the bridge was cancelled from under the
    /// disconnect watcher — `MediaTimeout` if the inactivity timer fired,
    /// otherwise a plain bot-side close (`ByCallee`).
    fn cancelled_cause(&self) -> BridgeHangupCause {
        if self.0.media_timed_out() {
            BridgeHangupCause::MediaTimeout
        } else {
            BridgeHangupCause::ByCallee
        }
    }
}

#[async_trait]
impl MediaBridge for WebRtcMediaBridge {
    async fn start(&self) -> anyhow::Result<()> {
        self.0.start_bridge().await;
        Ok(())
    }
    fn kind(&self) -> BridgeKind {
        BridgeKind::WebRtc
    }
    fn attach_recorder(
        &self,
        recorder: Arc<parking_lot::RwLock<Option<crate::media::recorder::Recorder>>>,
    ) {
        self.0.attach_recorder(recorder);
    }

    /// WebRTC's `teardown.close()` only closes the signaling adapter session —
    /// the local PCs/forwarders keep running until the last `Arc` drops. Cancel
    /// them explicitly so a disconnect-watcher holding an `Arc` releases it.
    fn shutdown(&self) {
        self.0.cancel();
    }

    fn start_media_timeout(&self, initial: Duration, rolling: Duration) {
        self.0.start_media_timeout(initial, rolling);
    }

    /// Gate the 200 OK on the bot PeerConnection's connection state.
    ///
    /// * `signaling` — resolve immediately (legacy behaviour; media unproven).
    /// * `ice_connected` (default) / `first_media` — wait for the bot PC to
    ///   reach `Connected`; error if it reaches Failed/Closed/Disconnected
    ///   first. (`first_media`'s stricter "wait for first inbound RTP"
    ///   semantic is a follow-on; it currently behaves as `ice_connected` —
    ///   ICE-connected is a strict prerequisite for any media anyway.)
    ///
    /// No timeout here: the pre-answer engine races this against the trunk's
    /// ring timeout and rejects with 480 on expiry.
    async fn await_media_commitment(&self, answer_on: AnswerOn) -> anyhow::Result<()> {
        if matches!(answer_on, AnswerOn::Signaling) {
            return Ok(());
        }
        let mut rx = self.0.webrtc_peer_state();
        loop {
            let state = *rx.borrow_and_update();
            match state {
                PeerConnectionState::Connected => return Ok(()),
                PeerConnectionState::Failed
                | PeerConnectionState::Closed
                | PeerConnectionState::Disconnected => {
                    return Err(anyhow!(
                        "bot PeerConnection reached {state:?} before media commitment"
                    ));
                }
                PeerConnectionState::New | PeerConnectionState::Connecting => {}
            }
            rx.changed()
                .await
                .map_err(|_| anyhow!("bot PeerConnection state channel closed"))?;
        }
    }

    /// Resolve when the bot PeerConnection ends the call: `Closed` → `ByCallee`
    /// (clean), `Failed` → `BotLost`. A `Disconnected` state is debounced for
    /// 5 s (transient packet loss recovers) before being treated as `BotLost`.
    /// When the bridge is torn down from the SIP side, dropping it closes the
    /// PC and this resolves `ByCallee` — harmless, since teardown is idempotent.
    fn watch_disconnect(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = BridgeHangupCause> + Send + '_>>
    {
        Box::pin(async move {
            let mut rx = self.0.webrtc_peer_state();
            // Resolve promptly when the bridge is cancelled from the SIP side
            // (teardown → shutdown), so this watcher releases its Arc clone.
            let cancelled = self.0.cancel_token();
            loop {
                let state = *rx.borrow_and_update();
                match state {
                    PeerConnectionState::Closed => return BridgeHangupCause::ByCallee,
                    PeerConnectionState::Failed => return BridgeHangupCause::BotLost,
                    PeerConnectionState::Disconnected => {
                        // Debounce: a transient disconnect may recover.
                        tokio::select! {
                            _ = cancelled.cancelled() => return self.cancelled_cause(),
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                                return BridgeHangupCause::BotLost;
                            }
                            changed = rx.changed() => {
                                if changed.is_err() {
                                    return BridgeHangupCause::BotLost;
                                }
                                // Loop re-reads the (possibly recovered) state.
                            }
                        }
                    }
                    PeerConnectionState::New
                    | PeerConnectionState::Connecting
                    | PeerConnectionState::Connected => {
                        tokio::select! {
                            _ = cancelled.cancelled() => return self.cancelled_cause(),
                            changed = rx.changed() => {
                                if changed.is_err() {
                                    // Channel closed = bridge dropped = call over.
                                    return BridgeHangupCause::ByCallee;
                                }
                            }
                        }
                    }
                }
            }
        })
    }
}
