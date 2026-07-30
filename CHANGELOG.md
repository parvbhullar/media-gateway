# Changelog

All notable changes to this project are documented in this file.

## [0.4.3]

**Theme: Pre-answer call engine, cluster routing fix, connection-pool reliability**

- **New pre-answer call-handling engine** — controls exactly when a call gets answered (only once the bot/agent is verifiably ready — via ICE connection, agent-joined, or media-ready signals depending on call type, not just "signaling succeeded"), sends proper ringback immediately, enforces ring timeouts, and adds a replay cache so carrier retry storms on rejected calls don't re-trigger full call setup. If the bot hangs up first, the carrier is now properly notified instead of being left hanging.
- **Fixed a real cluster routing bug** — in multi-server cluster setups, outbound calls being routed from one node to another were bypassing the correct destination node and going straight to the far end's own address, meaning the call could get silently lost. Root-caused to a regression from months ago; fixed and covered by a new end-to-end test.
- **Fixed dead connection pool cleanup** — carrier TCP/TLS connections that died unexpectedly weren't being reliably removed from the connection pool, risking reuse of a dead socket. Cleanup now happens precisely at the point of failure, verified with a real-socket test. Also fixed the trunk health-check to probe the same address actual calls use, and increased health-check frequency for TCP/TLS trunks (every 60s instead of 5 min) to keep those connections warm against carrier idle-timeout kills.
- **Fixed two call-teardown race conditions**: (1) a rejected call's failure response could get lost if the caller's ring-timeout watchdog gave up too early — now it waits long enough for the rejection to actually reach the wire; (2) cancelling a call could get needlessly stuck for up to ~32 seconds waiting on the CANCEL's own transaction, even after the real call-ending response had already arrived — now decoupled so the real response isn't blocked.
- **Fixed and hardened test coverage**: a missing test-database migration, a stale test asserting outdated (already-changed) behavior, a resampler audio-quality rounding bug, and added a dedicated unit test for the frame-accumulation logic that feeds the sidecar-to-SIP audio path.
- Minor: dropped the build date from the SIP User-Agent string (kept as `{brand}/{version}` only).

## [0.4.2]

**Theme: High-quality audio resampling + jitter buffering**

- **New HQ resampler** (`VoiceResampler`, built on `rubato`'s sinc resampler) replacing the legacy polyphase resampler — measured ~32 dB better audio quality than the old one, with a regression test to prove it and a throughput tripwire (979x realtime at 8k→24k, 470x at 8k→48k) so future changes can't silently tank performance.
- **Jitter buffering** added to the media pipeline — a policy-driven `JitterStage` wrapping rustrtc's jitter buffer, applied automatically on transcoded call legs and configurable per-trunk. Both directions covered: an ingress jitter stage and an egress audio pacing stage for transcoded legs.
- **HD codec upgrade path** — trunk media config can now enforce jitter/quality policy on the egress path, and caller-leg jitter policy is now derived from the inbound trunk automatically.
- **Configurable sidecar PCM rate** — HQ resampling wired into LiveKit and external-media/sidecar paths, with the sidecar's PCM sample rate configurable per trunk.
- **Crash-proof SIP DNS resolver** — hardened all resolver call sites against failures that could previously crash the process.
- **Dead-TCP-connection eviction** — a vendored `rsipstack` patch to detect and evict stale TCP connections instead of trying to reuse dead sockets.
- Removed unused legacy code (`TranscodingPipeline`/`LinearResampler`) now that the new resampler replaces them.
