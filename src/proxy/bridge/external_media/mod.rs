//! SIP↔sidecar PCM bridge — `kind="external_media"`.
//!
//! rustpbx terminates SIP + RTP (decode/encode, external_ip/latching) and
//! pipes decoded 48 kHz mono PCM to a co-located sidecar process over a
//! localhost UDP socket. The sidecar owns the far-end media entirely (e.g.
//! joining a LiveKit room as a participant via the Python SDK), decoupling
//! rustpbx from the flaky Rust LiveKit SDK.
//!
//! See spec: `docs/superpowers/specs/2026-05-29-external-media-bridge-design.md`.

pub mod dispatch;
pub mod media;
pub mod teardown;
