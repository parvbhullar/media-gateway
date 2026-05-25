//! Provider-agnostic SIP↔WebRTC bridge dispatcher.
//!
//! See plan: `/home/anuj/.claude/plans/imperative-sauteeing-cake.md` (Phase 6).
//!
//! Given a WebRTC trunk row and the inbound SIP INVITE's SDP offer, this
//! module:
//!
//! 1. Resolves the configured signaling adapter via [`signaling::lookup`]
//!    (looking up the trunk's `kind_config.signaling` field).
//! 2. Builds an inbound `TransportMode::Rtp` `PeerConnection` and feeds it
//!    the SIP-side offer SDP, producing an SDP answer for the INVITE.
//! 3. Builds an outbound `TransportMode::WebRtc` `PeerConnection` (offerer)
//!    using the trunk's per-row ICE servers (falling back to the global ICE
//!    list).
//! 4. Calls into the adapter to drive offer→answer with the remote
//!    signaling peer.
//! 5. Sets the WebRTC PC's remote description to the negotiated answer.
//! 6. Wires both PCs into a [`BridgePeer`] and arms it with Opus↔PCMU
//!    transcoding via `setup_bridge_with_codecs`.
//!
//! No vendor names appear in this file — all vendor-specific logic is
//! confined to the adapter (selected by name) and the per-trunk `protocol`
//! blob it interprets.

pub mod dispatch;
pub mod dtmf;
pub mod media_bridge;
pub mod sdp;
pub mod teardown;

#[cfg(test)]
mod tests;

pub use dispatch::dispatch_webrtc;
pub use media_bridge::WebRtcMediaBridge;
pub use sdp::build_outbound_webrtc_pc;
pub use teardown::WebRtcTeardown;

/// Re-export the kind-agnostic `DispatchOutcome` so callers (e.g. the
/// integration tests and `webrtc_route_dispatch`) can keep importing from
/// this module path.
pub use crate::proxy::bridge::session::DispatchOutcome;
