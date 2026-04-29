# Phase 8 — Deferred Items

## Pre-existing RTP e2e test flakes (out of scope)

The following 11-12 e2e tests fail on the `sip_fix` baseline both with and
without Phase 8 changes (verified by `git stash` regression run during
08-04 execution). They are RTP timing / media-loop tests that depend on
host networking and concurrent test scheduling. Phase 8 does not touch
the RTP path or media loop.

- `proxy::tests::test_media_e2e::test_p2p_callee_hangup_rtp_and_cdr`
- `proxy::tests::test_media_e2e::test_p2p_caller_hangup_rtp_and_cdr`
- `proxy::tests::test_media_e2e::test_rtp_payload_integrity_through_proxy`
- `proxy::tests::test_media_e2e::test_p2p_pcma_codec_through_proxy`
- `proxy::tests::test_media_e2e::test_p2p_pcmu_codec_through_proxy`
- `proxy::tests::test_media_e2e::test_p2p_unidirectional_rtp_caller_only`
- `proxy::tests::test_rtp_e2e::test_rtp_through_proxy`
- `proxy::tests::test_wholesale_e2e::test_wholesale_inbound_caller_hangup_rtp_cdr`
- `proxy::tests::test_wholesale_e2e::test_wholesale_inbound_user_hangup_rtp_cdr`
- `proxy::tests::test_wholesale_e2e::test_wholesale_pcma_rtp_cdr`
- `proxy::tests::test_wholesale_e2e::test_wholesale_rtp_payload_integrity`

Failure signature: `panicked at .../test_media_e2e.rs:403:5: Callee should
receive RTP`. Indicates a media-side delivery/timing issue, not a routing
or translation regression.

**Tracked for:** Phase 12 (Hardening) — investigate flake root cause and
either fix or move to `#[ignore]` with clear `cargo test --ignored` runs.
