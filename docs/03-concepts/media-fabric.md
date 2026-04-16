# Media Fabric

SuperSip handles all audio (and optionally video) media between call
participants. This page explains how RTP streams are managed, codecs
are negotiated, and advanced features like conferencing work.

## RTP relay modes

The `media_proxy_mode` setting in `proxy.rtp` controls how SuperSip
handles RTP packets:

| Mode   | Behaviour                                             |
|--------|-------------------------------------------------------|
| `auto` | Relay RTP through SuperSip; bypass when both endpoints are on the same LAN. |
| `all`  | Always relay RTP through SuperSip (recommended for recording and WebRTC). |
| `nat`  | Relay only when NAT is detected.                       |
| `none` | Do not relay RTP; endpoints exchange media directly.   |

When relaying, SuperSip allocates a pair of RTP ports from the
configured range (`rtp_start_port` .. `rtp_end_port`) and rewrites SDP
`c=` and `m=` lines so both endpoints send media to SuperSip rather
than to each other.

## Codec negotiation

The `MediaNegotiator` in `media/negotiate.rs` handles SDP offer/answer
codec selection. Supported audio codecs:

| Codec       | Payload Type | Clock Rate | Notes              |
|-------------|:------------:|:----------:|--------------------|
| PCMU (G.711u) | 0          | 8000       | Default fallback   |
| PCMA (G.711a) | 8          | 8000       | European standard  |
| G.722       | 9            | 8000*      | Wideband (16 kHz)  |
| G.729       | 18           | 8000       | Low bitrate        |
| Opus        | 111          | 48000      | Feature-gated      |
| telephone-event | 101      | 8000       | RFC 2833 DTMF      |

*G.722 uses an 8000 Hz clock rate in SDP per RFC 3551 despite encoding
16 kHz audio.

Codec preference can be set per-trunk (`codec` field in `TrunkConfig`)
or globally. The negotiator intersects the caller's offered codecs with
the configured preference list and selects the highest-priority match.

When the two legs negotiate different codecs, the `Transcoder` module
handles real-time PCM conversion between them.

## Track architecture

SuperSip uses a **Track** abstraction (`media/mod.rs`) to represent
each media endpoint. Track types include:

| Type          | Purpose                                    |
|---------------|--------------------------------------------|
| `RtcTrack`    | Standard RTP/SRTP/WebRTC media endpoint     |
| `FileTrack`   | Audio file playback (ringback, hold music, announcements) |

Tracks are grouped into a `MediaStream` that manages their lifecycle.
Each track exposes a PeerConnection for SDP negotiation and can be
independently muted or replaced at runtime.

The `RtpTrackBuilder` configures transport mode, port range, external
IP, codec preference, ICE servers, and SRTP/WebRTC parameters before
constructing a track.

## WebRTC-SIP bridging

The `sdp_bridge` module (`media/sdp_bridge.rs`) transforms SDP between
WebRTC and traditional SIP formats:

- **WebRTC side** — `UDP/TLS/RTP/SAVPF` profile with DTLS fingerprint,
  ICE candidates, and SRTP encryption.
- **SIP side** — `RTP/AVP` profile with plain RTP and no encryption.

The bridge generates ICE credentials, DTLS info, and rewrites media
sections so that a browser (via SIP.js or JsSIP over WebSocket) can
communicate with a traditional SIP phone or PSTN trunk.

## Conference mixing

The `ConferenceMixer` (`media/conference_mixer.rs`) provides MCU-style
multi-party audio mixing:

- Each participant contributes decoded PCM frames via an input channel.
- The mixer combines all participants' audio, excluding each
  participant's own contribution (so they do not hear themselves).
- Mixed PCM is sent back through an output channel for re-encoding.
- Participants can be individually muted.

The `AudioMixer` utility (`media/mixer.rs`) handles the underlying
sample-level mixing and supports supervisor modes:

| Mode      | Behaviour                                   |
|-----------|---------------------------------------------|
| Listen    | Supervisor hears both parties, neither hears supervisor. |
| Whisper   | Supervisor can talk to agent only; caller cannot hear supervisor. |
| Barge     | Supervisor joins the conversation as a full participant. |

## DTMF handling

DTMF tones are carried via RFC 2833 telephone-event RTP packets
(payload type 101). The `negotiate` module ensures telephone-event is
included in SDP offers when supported.

For in-band DTMF, the `RwiRouting` module can intercept digit sequences
and trigger local actions (transfer, recording, hold) or forward them
to the RWI application layer. DTMF rules support pattern matching with
configurable actions per digit sequence.

## SRTP and ICE

When the transport mode is set to `Srtp` or `WebRtc`, the
`RtpTrackBuilder` configures:

- **SRTP** — Encrypted RTP using DTLS key exchange.
- **ICE** — Interactive Connectivity Establishment for NAT traversal,
  with configurable STUN/TURN servers.
- **SDP compatibility** — WebRTC mode uses standard SDP (with `a=mid`,
  `rtcp-mux`); SIP/SRTP mode uses legacy SDP for compatibility with
  traditional endpoints like Linphone.

## Audio file playback

The `FileTrack` provides audio file playback with:

- WAV file decoding with automatic sample-rate conversion.
- Remote file support (HTTP/HTTPS URLs).
- Loop playback for hold music.
- Dynamic audio source switching at runtime.
- Codec encoding (PCM to negotiated codec) with 20 ms frame pacing.

## Further reading

- [SIP & B2BUA](sip-and-b2bua.md) — call legs and dialog lifecycle
- [RWI Model](rwi-model.md) — media control via WebSocket commands
- [Media subsystem](../04-subsystems/media.md) — implementation details
