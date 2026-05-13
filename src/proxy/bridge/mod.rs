//! Provider-agnostic SIP↔WebRTC bridge module.
//!
//! See plan: `/home/anuj/.claude/plans/imperative-sauteeing-cake.md` (PR 3,
//! Phases 4–6).
//!
//! Three pieces:
//! - [`signaling`] — `WebRtcSignalingAdapter` trait and process-global
//!   adapter registry. The built-in `http_json` adapter is registered by
//!   [`signaling::register_builtins`].
//! - [`webrtc`] — the dispatcher [`dispatch_webrtc`] which, given a WebRTC
//!   trunk row and an inbound SIP INVITE offer, constructs the inbound RTP
//!   PeerConnection, builds an outbound WebRTC PeerConnection, drives the
//!   adapter's offer/answer negotiation, and wires the two PCs into a
//!   `BridgePeer`.
//!
//! The dispatcher is provider-agnostic: vendor specifics live entirely in
//! per-trunk `kind_config.protocol` JSON and are interpreted by the adapter
//! selected via the trunk's `signaling` field.

pub mod signaling;
pub mod webrtc;

pub use webrtc::dispatch_webrtc;
