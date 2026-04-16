# Media

## What it does

The media module handles all RTP media processing in SuperSip. It provides
WebRTC PeerConnection management, codec transcoding, SDP negotiation, call
recording, audio file playback (ringback tones, hold music, announcements),
conference mixing, and audio source switching. It bridges SIP/RTP endpoints
with WebRTC clients.

## Key types & entry points

- **`StreamWriter`** (trait) — generic interface for writing encoded audio data (header, packets, finalize). `src/media/mod.rs`
- **`Track`** (trait) — represents a media track with SDP handshake, mute control, and PeerConnection access. `src/media/mod.rs`
- **`AudioSource`** (trait) — pluggable audio sample source for playback tracks. `src/media/audio_source.rs`
- **`MediaStream`** — manages a collection of named tracks with recording options. `src/media/mod.rs`
- **`MediaStreamBuilder`** — builder for creating `MediaStream` instances with cancel tokens and recorder config. `src/media/mod.rs`
- **`Recorder`** — records audio from a PeerConnection to WAV files. `src/media/recorder.rs`
- **`RecorderOption`** — recording configuration (paths, format). `src/media/recorder.rs`
- **`MediaMixer`** — multi-party audio mixing engine for conferencing. `src/media/mixer.rs`
- **`Transcoder`** — codec transcoding between different audio formats. `src/media/transcoder.rs`
- **`SdpBridge`** — SDP offer/answer bridging between RTP and WebRTC endpoints. `src/media/sdp_bridge.rs`
- **`RtcTrack`** — concrete Track implementation using a PeerConnection (RTP/WebRTC). `src/media/mod.rs`
- **`RtpTrackBuilder`** — builder for RtcTrack with codec preferences, ICE servers, port ranges. `src/media/mod.rs`
- **`FileTrack`** — audio file playback track with loop support and dynamic source switching. `src/media/mod.rs`
- **`AudioSourceManager`** — manages audio source lifecycle (file, silence, switching). `src/media/audio_source.rs`

## Sub-modules

- `audio_source.rs` — AudioSource trait and AudioSourceManager
- `bridge.rs` — Media bridge utilities
- `mixer.rs` / `mixer_input.rs` / `mixer_output.rs` / `mixer_registry.rs` — Conference mixing engine
- `conference_mixer.rs` — High-level conference mixer
- `negotiate.rs` — SDP codec negotiation (CodecInfo)
- `sdp_bridge.rs` — SDP bridging between RTP and WebRTC
- `recorder.rs` — Call recording to WAV
- `transcoder.rs` — Audio codec transcoding
- `forwarding_track.rs` — RTP packet forwarding
- `wav_writer.rs` — WAV file writer

## Configuration

Media settings are under `[proxy]` config: `media_proxy` mode (auto/always/never),
`external_ip`, `rtp_start_port`, `rtp_end_port`, `webrtc_port_start`,
`webrtc_port_end`, `ice_servers`, and `enable_latching`.

**Codecs supported:** PCMU, PCMA, G722, G729, Opus (feature-gated via `opus` feature flag), TelephoneEvent (DTMF).

## Public API surface

The media module does not expose HTTP routes. It is consumed by the call
and proxy layers for RTP/WebRTC media handling.

## See also

- [call.md](call.md) — Call layer that configures media streams
- [proxy.md](proxy.md) — SIP proxy that negotiates SDP
- [../03-concepts/](../03-concepts/) — Media processing concepts

---
**Status:** ✅ Shipped
**Source:** `src/media/`
**Related phases:** (media spans multiple phases)
**Last reviewed:** 2026-04-16

> TODO: deep-dive pending
