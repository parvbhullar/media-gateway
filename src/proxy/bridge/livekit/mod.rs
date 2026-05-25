//! SIP↔LiveKit room-participant bridge — parallel to `proxy/bridge/webrtc/`.
//!
//! See spec: `docs/superpowers/specs/2026-05-25-livekit-trunk-design.md`.
//! See plan: `docs/superpowers/plans/2026-05-25-livekit-trunk.md`.

pub mod client;
pub mod decision;
pub mod dispatch;
pub mod media;
pub mod teardown;
pub mod template;
pub mod token;
pub mod webhook;
#[cfg(test)]
mod tests;
