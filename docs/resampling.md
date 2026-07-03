# Audio Resampling, Jitter Handling & HD Codec Upgrade

This guide covers the media-quality features of the gateway: high-quality
resampling on every transcoded path, per-trunk jitter buffering, per-trunk
HD codec upgrade, and the configurable sidecar PCM rate. It explains how
the pipeline works, how to configure each feature, and how to verify it.

Design/implementation history: `docs/plans/2026-07-03-trunk-resampling-jitter-design.md`.

---

## 1. Overview

Whenever two call legs use different codecs (or a leg feeds an AI consumer
at a different sample rate), audio passes through a
**decode → resample → encode** pipeline. The resampler is
`VoiceResampler` (`src/media/resampler.rs`), built on
[rubato](https://crates.io/crates/rubato) band-limited sinc interpolation —
pure Rust, NEON/AVX accelerated, no C dependencies.

Where it applies (all automatic, no configuration needed):

| Path | Conversion | Used by |
|---|---|---|
| Trunk ↔ trunk / WebRTC transcode | e.g. PCMU 8 kHz ↔ Opus 48 kHz | `Transcoder` in the media bridge |
| SIP → LiveKit | codec rate → 48 kHz | LiveKit bridge (rate fixed by libwebrtc) |
| LiveKit → SIP | 48 kHz → codec rate | LiveKit bridge |
| SIP → sidecar (pipecat/AI) | codec rate → `pcm_sample_rate` | external_media bridge |
| Sidecar → SIP | `pcm_sample_rate` → codec rate | external_media bridge |

Codec-native rates are always preferred over resampling:

- **PCMU/PCMA** decode to 8 kHz PCM (RTP clock 8 kHz).
- **G.722** decodes to 16 kHz PCM (RTP clock stays 8 kHz per RFC 3551).
- **Opus** is natively 48 kHz here — when the far side is a 48 kHz
  consumer, no resampler is inserted at all.

RTP timestamps and SDP always describe the **wire codec clock**; internal
PCM rates never leak into signaling.

### Quality and cost

Measured by the in-tree regression tests (1 kHz tone, Goertzel analysis of
spectral images above the source Nyquist):

| Conversion | Legacy polyphase | VoiceResampler |
|---|---|---|
| 8 kHz → 24 kHz | −54.5 dBc | **−86.8 dBc** |
| 8 kHz → 48 kHz | −57.5 dBc | **−89.4 dBc** |

Throughput on Apple Silicon (release build): **979× realtime** for
8 k→24 k, **470× realtime** for 8 k→48 k, per core — CPU cost per call is
negligible. Group delay is ~8 ms at 8 kHz input (`sinc_len = 128`), plus a
one-time sub-millisecond startup trim on the first frame.

Profile (fixed; there is deliberately no quality knob):
`sinc_len 128, f_cutoff 0.95, oversampling 64, cubic interpolation,
BlackmanHarris2 window` — the "lean real-time voice" point from the
research that produced this design. Clock-drift correction is not wired
(both ends of every resample are clocked by this process); rubato's
`set_resample_ratio_relative` is the upgrade path if long-call buffer
creep is ever observed.

---

## 2. HD codec upgrade per trunk

Pin the codecs a trunk speaks via the trunk media API. When a call egresses
through that trunk, the offer contains **exactly those codecs, quality-first,
even if the caller didn't offer them** — the bridge transcodes between the
legs (with HQ resampling) when the answer differs from the ingress codec.

```bash
# Make every call leaving trunk group "ai-agents" negotiate Opus:
curl -X PUT http://localhost:8080/api/v1/trunks/ai-agents/media \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"codecs": ["opus"], "dtmf_mode": null, "srtp": null, "media_mode": null}'
```

Rules:

- Codec names are **lowercase**: `pcmu`, `pcma`, `g722`, `g729`, `opus`,
  `telephone-event`. RFC 3551 static payload-type numbers (`0`, `8`, `9`,
  `18`, `101`) are also accepted when read on the hot path.
- An **empty list disables the filter** (offer follows the caller and
  global `codec_strategy` as before).
- Trunk `media_config.codecs` is the most specific policy — it takes
  precedence over route-rule `codecs` and the global config codec list,
  and implies quality-first ordering (Opus > G729 > G722 > G711).
- The API keys on **trunk group name** (`rustpbx_trunk_groups`); on the
  hot path the lookup also resolves member gateway names to their group.
- Best-effort: a missing/malformed `media_config` never fails routing.

A caller offering only PCMU dialed toward the trunk above receives a
PCMU answer on their own leg; the egress leg negotiates Opus; the bridge
transcodes PCMU↔Opus with the HQ resampler in both directions.

---

## 3. Jitter buffering

An ingress jitter buffer (packet-domain reorder + adaptive delay) sits
**before decode**, so decoders never see out-of-order packets — reordered
input corrupts stateful decoders (especially Opus) and no amount of
post-processing recovers it.

### Default behavior (no configuration)

| Leg type | Jitter buffer |
|---|---|
| Transcoded leg | **On automatically** (min 20 ms / max 120 ms, adaptive) |
| Passthrough (relay) leg | Off — zero added latency; endpoints handle jitter |
| Video | Never buffered |

Transcoded egress is additionally **paced** to a clean 20 ms cadence, so a
bursty ingress (TTS chunk bursts, congested carriers) doesn't land
unsmoothed on the far trunk.

### Per-trunk override

```bash
# Re-time ALL calls from a trunk on a bad network (passthrough included):
curl -X PUT http://localhost:8080/api/v1/trunks/flaky-carrier/media \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"codecs": [], "jitter_buffer": {"mode": "adaptive", "min_ms": 40, "max_ms": 200}}'

# Escape hatch — disable even on transcoded legs:
curl -X PUT http://localhost:8080/api/v1/trunks/lab-trunk/media \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"codecs": [], "jitter_buffer": {"mode": "off"}}'
```

- `mode: "adaptive"` — applies to **all** inbound legs from this trunk.
  `min_ms` (default 20) is the smoothing delay for in-order packets;
  `max_ms` (default 120) is how long to wait for a gap to fill before
  giving up on the missing packet. Constraints: `min_ms < max_ms`,
  `max_ms ≤ 500`.
- `mode: "off"` — no jitter buffer for this trunk, ever.
- `null` / absent — the default matrix above.
- Direction mapping: the **inbound** trunk's policy governs the
  caller-side leg; the **egress** trunk's policy governs the callee-side
  leg (each trunk controls the media *arriving from it*).
- In-order audio is delayed by `min_ms`; a burst of loss adds at most
  `max_ms`. Budget accordingly for latency-sensitive routes.
- DTMF (RFC 4733) flows through the buffer in order but always bypasses
  transcoding/resampling.

---

## 4. Sidecar PCM rate (external_media trunks)

The PCM pipe between the gateway and a per-call sidecar (pipecat / AI
agent) defaults to 48 kHz for backward compatibility. **24 kHz is the
preferred rate for AI consumers** — half the bandwidth/CPU and the native
rate of most ASR/TTS stacks.

Set it in the trunk's `kind_config`:

```toml
[trunks.ai-sidecar]
kind = "external_media"
# ...
[trunks.ai-sidecar.kind_config]
command = "python3 /opt/agents/participant.py"
audio_codec = "opus"
pcm_sample_rate = 24000   # 16000 | 24000 | 48000 (default 48000)
```

- Datagram framing stays 20 ms mono s16le, so the frame size scales with
  the rate: **1920 bytes @ 48 k, 960 @ 24 k, 640 @ 16 k**.
- When the rate is non-default, the gateway appends `--sample-rate=<hz>`
  to the sidecar command so the sidecar can configure its pipeline to
  match. Sidecars that don't understand the flag keep working as long as
  you leave the default; opting into 24 k implies sidecar support.
- No resampler is inserted when the SIP codec's native rate already
  matches (e.g. G.722 → 16 k pipe).

---

## 5. Verifying

```bash
# Unit + quality regression (imaging must beat the legacy resampler):
RUSTC_WRAPPER="" cargo test --lib media::resampler

# Jitter stage behavior (reorder, bypass, wait bounds):
RUSTC_WRAPPER="" cargo test --lib media::jitter

# Trunk media API (codecs + jitter_buffer validation):
RUSTC_WRAPPER="" cargo test --test api_v1_trunk_media

# HD-upgrade offer shape (Opus offered beyond the caller's list):
RUSTC_WRAPPER="" cargo test --lib media::negotiate

# Throughput tripwire (release; prints ×-realtime figures):
RUSTC_WRAPPER="" cargo test --release --lib media::resampler -- --ignored --nocapture
```

At runtime, look for these log lines:

- `Bridge transcoder configured for selected codec mismatch` — transcode
  (and therefore HQ resampling + auto jitter buffer + pacing) is active.
- `Bridge ingress jitter configured` — a per-trunk policy was applied.
- `trunk media_config codecs applied to egress offer` — HD upgrade fired.
- `system DNS config unusable for SRV resolver; falling back to public
  DNS` — unrelated to media, but emitted by the same release: the SIP
  resolver degrades instead of crashing on unparsable `resolv.conf`
  entries.

## 6. Troubleshooting

| Symptom | Check |
|---|---|
| Muffled/aliased audio on transcoded calls | Should be fixed by the HQ resampler; confirm the transcoder log line appears and the build includes `rubato` (`cargo tree -p rubato`). |
| Choppy audio from one carrier | Set `jitter_buffer: {"mode":"adaptive"}` on that trunk (§3). If the far end is bursty on egress, transcoded legs are already paced; passthrough legs are not — consider forcing a transcode via trunk codecs, or accept relay semantics. |
| Trunk still negotiates G.711 despite pinned `codecs` | The pin lives on the **trunk group** row; verify with `GET /api/v1/trunks/{name}/media`. The hot path resolves member gateways to their group, but the group row must exist and the routing state must be DB-wired. |
| Sidecar hears garbage / wrong pitch | Rate mismatch: sidecar must consume the configured `pcm_sample_rate` (watch for the `--sample-rate` flag in its argv). Frame size must match §4. |
| Added latency after enabling jitter buffer | In-order delay = `min_ms`; lower it (e.g. 10–20 ms) or scope the policy to the problematic trunk only. |
