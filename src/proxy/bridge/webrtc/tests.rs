use rustrtc::MediaKind;
use serde_json::{Value, json};

use crate::models::trunk;
use crate::models::trunk::{TrunkDirection, TrunkStatus};
use crate::proxy::bridge::common::{DispatchContext, build_inbound_rtp_pc, resolve_ice_servers};
use crate::proxy::bridge::signaling::{
    self, NegotiateOutcome, SessionHandle, SignalingContext, WebRtcSignalingAdapter,
};

use super::codecs::configure_webrtc_bridge_transcoders;
use super::dispatch::dispatch_webrtc;
use super::sdp::build_outbound_webrtc_pc;

use crate::media::bridge::{BridgeEndpoint, BridgePeer};
use crate::media::negotiate::MediaNegotiator;

use audio_codec::CodecType;
use anyhow::Result;
use rustrtc::IceServer;

fn webrtc_trunk(signaling_name: &str) -> trunk::Model {
    trunk::Model {
        id: 1,
        name: "test_webrtc_trunk".into(),
        kind: "webrtc".into(),
        status: TrunkStatus::Healthy,
        direction: TrunkDirection::Outbound,
        is_active: true,
        kind_config: json!({
            "signaling": signaling_name,
            "endpoint_url": "http://127.0.0.1:1/offer",
            "audio_codec": "opus",
            "protocol": {
                "request_body_template": r#"{"sdp":"{offer_sdp}"}"#,
                "response_answer_path": "$.sdp",
            }
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn build_outbound_webrtc_pc_creates_pc_with_audio_transceiver() {
    let pc = build_outbound_webrtc_pc(None, "opus", None).unwrap();
    let transceivers = pc.get_transceivers();
    let audio_count = transceivers
        .iter()
        .filter(|t| matches!(t.kind(), MediaKind::Audio))
        .count();
    assert_eq!(audio_count, 1, "expected exactly one audio transceiver");
}

#[tokio::test]
async fn build_outbound_webrtc_pc_rejects_unknown_codec() {
    match build_outbound_webrtc_pc(None, "carrier-pigeon", None) {
        Ok(_) => panic!("expected unknown-codec error"),
        Err(e) => assert!(e.to_string().contains("carrier-pigeon")),
    }
}

#[test]
fn resolve_ice_servers_prefers_per_trunk() {
    let per_trunk = json!([{"urls": ["stun:per-trunk:3478"]}]);
    let global = vec![IceServer::new(vec!["stun:global:3478".to_string()])];
    let out = resolve_ice_servers(Some(&per_trunk), Some(&global)).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].urls, vec!["stun:per-trunk:3478".to_string()]);
}

#[test]
fn resolve_ice_servers_falls_back_to_global() {
    let global = vec![IceServer::new(vec!["stun:global:3478".to_string()])];
    let out = resolve_ice_servers(None, Some(&global)).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].urls, vec!["stun:global:3478".to_string()]);
}

#[test]
fn resolve_ice_servers_empty_when_neither_set() {
    let out = resolve_ice_servers(None, None).unwrap();
    assert!(out.is_empty());
}

/// Adapter that always succeeds on `negotiate` (returning an
/// intentionally invalid answer SDP so the post-negotiate parse step
/// fails) and records every `close` call. Used to prove the
/// session-leak fix: when a post-negotiate step errors, `close` must
/// fire before the error propagates.
struct LeakProbeAdapter {
    closed_count: std::sync::Arc<std::sync::Mutex<usize>>,
    last_closed_session: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
}

#[async_trait::async_trait]
impl WebRtcSignalingAdapter for LeakProbeAdapter {
    async fn negotiate(
        &self,
        _ctx: &SignalingContext,
        _offer_sdp: &str,
    ) -> Result<NegotiateOutcome, crate::proxy::bridge::signaling::SignalingError> {
        Ok(NegotiateOutcome {
            // Intentionally garbage — parse will fail.
            answer_sdp: "not a valid SDP".to_string(),
            session: SessionHandle(json!({"id": "leak-probe-sess-1"})),
        })
    }
    async fn close(
        &self,
        _ctx: &SignalingContext,
        session: &SessionHandle,
    ) -> Result<(), crate::proxy::bridge::signaling::SignalingError> {
        *self.closed_count.lock().unwrap() += 1;
        *self.last_closed_session.lock().unwrap() = Some(session.0.clone());
        Ok(())
    }
}

#[tokio::test]
async fn dispatch_closes_session_when_post_negotiate_step_fails() {
    // Register the leak probe under a unique adapter name and point a
    // trunk at it.
    let closed_count = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let last_session = std::sync::Arc::new(std::sync::Mutex::new(None));
    let probe = std::sync::Arc::new(LeakProbeAdapter {
        closed_count: closed_count.clone(),
        last_closed_session: last_session.clone(),
    });
    signaling::register("leak_probe", probe);

    let trunk = webrtc_trunk("leak_probe");
    // Use a minimal valid SDP offer so steps 1-3 succeed; step 4
    // (SDP parse of the bogus answer) is what we want to trigger.
    let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                 t=0 0\r\nm=audio 10000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
    let result = dispatch_webrtc(&trunk, offer, None, &DispatchContext::default()).await;

    assert!(result.is_err(), "expected post-negotiate failure");
    let n = *closed_count.lock().unwrap();
    assert_eq!(
        n, 1,
        "adapter.close should fire exactly once on post-negotiate failure, got {n}"
    );
    let session = last_session.lock().unwrap().clone();
    assert_eq!(
        session,
        Some(json!({"id": "leak-probe-sess-1"})),
        "close should be invoked with the session handle returned by negotiate"
    );
}

/// Carrier offers only PCMA — the bridge's SIP-side answer must
/// negotiate PCMA (not silently fall back to PCMU as the old hardcoded
/// `AudioCapability::pcmu()` would). Proves codec passthrough works
/// for the simplest case.
#[tokio::test]
async fn inbound_rtp_pc_negotiates_pcma_when_carrier_only_offers_pcma() {
    let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                 t=0 0\r\nm=audio 10000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
    let (_pc, answer_sdp, negotiated, _dtmf_pt) = build_inbound_rtp_pc(offer, &DispatchContext::default())
        .await
        .expect("PCMA-only offer should negotiate");
    assert_eq!(
        negotiated.codec_name, "PCMA",
        "negotiated codec should be PCMA when that's all the carrier offered"
    );
    assert_eq!(negotiated.payload_type, 8);
    assert!(
        answer_sdp.to_lowercase().contains("pcma"),
        "answer SDP must advertise PCMA, got:\n{}",
        answer_sdp
    );
}

/// Carrier offers Opus-on-SIP — the bridge must accept that too,
/// not force PCMU/PCMA. Proves we genuinely picked from the carrier's
/// list rather than imposing our own.
#[tokio::test]
async fn inbound_rtp_pc_negotiates_opus_when_carrier_offers_it() {
    let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                 t=0 0\r\nm=audio 10000 RTP/AVP 111\r\n\
                 a=rtpmap:111 opus/48000/2\r\n";
    let (_pc, _answer_sdp, negotiated, _dtmf_pt) = build_inbound_rtp_pc(offer, &DispatchContext::default())
        .await
        .expect("opus-on-SIP offer should negotiate");
    assert_eq!(
        negotiated.codec_name.to_lowercase(),
        "opus",
        "negotiated codec should be opus when the carrier offered it"
    );
}

/// Carrier offers PCMU + PCMA + G722 → answerer picks the carrier's
/// preferred (first listed) codec. With the new intersection logic
/// we walk the carrier's offer in order; PCMU is listed first so it
/// wins.
#[tokio::test]
async fn inbound_rtp_pc_honours_carrier_codec_preference() {
    let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                 t=0 0\r\nm=audio 10000 RTP/AVP 0 8 9\r\n\
                 a=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\n\
                 a=rtpmap:9 G722/8000\r\n";
    let (_pc, _answer_sdp, negotiated, _dtmf_pt) = build_inbound_rtp_pc(offer, &DispatchContext::default())
        .await
        .expect("multi-codec offer should negotiate");
    assert_eq!(
        negotiated.codec_name.to_uppercase(),
        "PCMU",
        "negotiated codec should be PCMU (carrier's first preference), got: {}",
        negotiated.codec_name
    );
}

/// Carrier offers PCMU + telephone-event PT 101 → voice codec lands
/// on PCMU (NOT telephone-event), and the DTMF payload type is
/// captured separately for `set_dtmf_sink`.
#[tokio::test]
async fn inbound_rtp_pc_separates_voice_codec_from_dtmf_pt() {
    let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                 t=0 0\r\nm=audio 10000 RTP/AVP 0 101\r\n\
                 a=rtpmap:0 PCMU/8000\r\n\
                 a=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-16\r\n";
    let (_pc, _answer_sdp, voice, dtmf_pt) = build_inbound_rtp_pc(offer, &DispatchContext::default())
        .await
        .expect("PCMU + telephone-event offer should negotiate");
    assert_eq!(
        voice.codec_name.to_uppercase(),
        "PCMU",
        "voice codec must be PCMU, not telephone-event"
    );
    assert_eq!(
        dtmf_pt,
        Some(101),
        "DTMF payload type must be captured separately for set_dtmf_sink"
    );
}

/// Carrier offers a non-standard telephone-event PT (96 instead of
/// the usual 101) → we honour whatever PT the carrier chose.
#[tokio::test]
async fn inbound_rtp_pc_honours_carrier_dtmf_pt() {
    let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                 t=0 0\r\nm=audio 10000 RTP/AVP 0 96\r\n\
                 a=rtpmap:0 PCMU/8000\r\n\
                 a=rtpmap:96 telephone-event/8000\r\na=fmtp:96 0-16\r\n";
    let (_pc, _answer_sdp, _voice, dtmf_pt) = build_inbound_rtp_pc(offer, &DispatchContext::default())
        .await
        .expect("non-standard DTMF PT should still negotiate");
    assert_eq!(
        dtmf_pt,
        Some(96),
        "DTMF PT must follow carrier's offer, not be hardcoded to 101"
    );
}

/// Carrier does NOT offer telephone-event → DTMF PT is None.
/// dispatch path will skip the `set_dtmf_sink` call and log that
/// DTMF will only flow as in-band audio (if at all).
#[tokio::test]
async fn inbound_rtp_pc_no_dtmf_when_carrier_omits_it() {
    let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                 t=0 0\r\nm=audio 10000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
    let (_pc, _answer_sdp, _voice, dtmf_pt) = build_inbound_rtp_pc(offer, &DispatchContext::default())
        .await
        .expect("PCMU-only offer (no telephone-event) should still negotiate");
    assert_eq!(
        dtmf_pt, None,
        "no telephone-event in offer must yield dtmf_pt=None"
    );
}

/// Carrier offers a codec we don't support (G.729) → dispatch should
/// surface a clear error rather than silently fall back. Important
/// because the previous hardcoded-PCMU path would have continued
/// regardless and produced silent audio.
#[tokio::test]
async fn inbound_rtp_pc_rejects_unsupported_only_offer() {
    let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                 t=0 0\r\nm=audio 10000 RTP/AVP 18\r\na=rtpmap:18 G729/8000\r\n";
    let result = build_inbound_rtp_pc(offer, &DispatchContext::default()).await;
    match result {
        Ok((_, _, neg, _)) => panic!(
            "expected error for G729-only offer, got negotiated={}",
            neg.codec_name
        ),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("no voice codecs the bridge supports")
                    || msg.to_lowercase().contains("g729"),
                "expected codec-mismatch error message, got: {msg}"
            );
        }
    }
}

#[tokio::test]
async fn dispatch_rejects_unknown_adapter() {
    // Make sure the *known* adapters are registered, but use a name
    // guaranteed not to exist.
    signaling::register_builtins();
    let trunk = webrtc_trunk("frobnicate");
    let result = dispatch_webrtc(&trunk, "v=0\r\n", None, &DispatchContext::default()).await;
    let err = match result {
        Ok(_) => panic!("expected unknown-adapter error"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("frobnicate"),
        "expected error to mention adapter name, got: {msg}"
    );
    assert!(
        msg.contains("not registered"),
        "expected `not registered` in error, got: {msg}"
    );
}

// ── Codec-mismatch transcoder wiring (the SIP→WebRTC "deaf call" fix) ───────

/// Build a real `BridgePeer` (two PeerConnections) for transcoder-wiring
/// tests. The PCs are only needed so `BridgePeer::new` is satisfied; the
/// transcoder decision is driven by the codec args we pass to
/// `configure_webrtc_bridge_transcoders`.
async fn test_bridge() -> BridgePeer {
    let pcmu_offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                      t=0 0\r\nm=audio 10000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
    let (rtp_pc, _answer, _cap, _dtmf) = build_inbound_rtp_pc(pcmu_offer, &DispatchContext::default())
        .await
        .expect("rtp pc");
    let webrtc_pc = build_outbound_webrtc_pc(None, "opus", None).expect("webrtc pc");
    BridgePeer::new("codec-test".into(), webrtc_pc, rtp_pc)
}

const WEBRTC_ANSWER_OPUS: &str = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\n\
    c=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2\r\n";

/// Mismatch (SIP PCMA pt 8 vs WebRTC Opus) MUST install a transcoder in BOTH
/// directions — otherwise raw G.711 is forwarded under the Opus PT and the
/// bot is deaf. This is the regression guard for the reported bug.
#[tokio::test]
async fn codec_mismatch_installs_both_transcoders() {
    let bridge = test_bridge().await;
    configure_webrtc_bridge_transcoders(&bridge, CodecType::PCMA, 8, WEBRTC_ANSWER_OPUS);

    // caller→bot (source = Rtp/SIP) transcodes G.711 → Opus, emitted under
    // the WebRTC leg's PT (111).
    assert_eq!(
        bridge.transcoder_target_pt(BridgeEndpoint::Rtp),
        Some(111),
        "SIP→WebRTC leg must transcode to the WebRTC Opus PT"
    );
    // bot→caller (source = WebRtc) transcodes Opus → G.711, emitted under
    // the SIP leg's PT (8 = PCMA).
    assert_eq!(
        bridge.transcoder_target_pt(BridgeEndpoint::WebRtc),
        Some(8),
        "WebRTC→SIP leg must transcode to the SIP PCMA PT"
    );
}

/// Matching codecs (Opus on both legs) must NOT install transcoders —
/// preserve zero-cost passthrough.
#[tokio::test]
async fn matching_codecs_install_no_transcoder() {
    let bridge = test_bridge().await;
    configure_webrtc_bridge_transcoders(&bridge, CodecType::Opus, 111, WEBRTC_ANSWER_OPUS);

    assert_eq!(bridge.transcoder_target_pt(BridgeEndpoint::Rtp), None);
    assert_eq!(bridge.transcoder_target_pt(BridgeEndpoint::WebRtc), None);
}

/// The bug this fix exists for: rustrtc's RTP-mode answer SDP enumerates our
/// FULL offered set (Opus first), so the answer's first codec is NOT the one
/// the leg negotiated. The transcoder decision MUST use the authoritative
/// `negotiated` capability, not a re-parse of the answer SDP — otherwise a
/// PCMA carrier reads back as Opus, "matches" the Opus bot, and audio is
/// forwarded raw (deaf). This test reproduces that exact scenario end-to-end.
#[tokio::test]
async fn uses_negotiated_codec_not_misleading_answer_sdp() {
    // Carrier prefers PCMA (listed first) and also offers Opus-on-SIP.
    let offer = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\n\
                 t=0 0\r\nm=audio 10000 RTP/AVP 8 111\r\n\
                 a=rtpmap:8 PCMA/8000\r\na=rtpmap:111 opus/48000/2\r\n";
    let (rtp_pc, answer_sdp, negotiated, _dtmf) =
        build_inbound_rtp_pc(offer, &DispatchContext::default())
            .await
            .expect("PCMA-preferred offer should negotiate");

    // Authoritative negotiation picked PCMA pt 8 (carrier's first preference).
    assert_eq!(negotiated.codec_name.to_uppercase(), "PCMA");
    assert_eq!(negotiated.payload_type, 8);

    // The trap: the answer SDP re-parse reports Opus (our full offered set is
    // enumerated, Opus first) — proving why re-parsing the answer was wrong.
    let from_answer = MediaNegotiator::extract_leg_profile(&answer_sdp).audio;
    assert_eq!(
        from_answer.map(|c| c.codec),
        Some(CodecType::Opus),
        "answer SDP misreports the codec as Opus — must NOT be used for the decision"
    );

    // The fix: feeding the authoritative negotiated cap still installs the
    // PCMA↔Opus transcoder (does not get fooled into 'matching' Opus/Opus).
    let webrtc_pc = build_outbound_webrtc_pc(None, "opus", None).expect("webrtc pc");
    let bridge = BridgePeer::new("codec-trap".into(), webrtc_pc, rtp_pc);
    let sip_codec = CodecType::try_from(negotiated.codec_name.as_str()).expect("known codec");
    configure_webrtc_bridge_transcoders(&bridge, sip_codec, negotiated.payload_type, WEBRTC_ANSWER_OPUS);

    assert_eq!(
        bridge.transcoder_target_pt(BridgeEndpoint::Rtp),
        Some(111),
        "must install PCMA→Opus transcoder despite the answer SDP saying Opus"
    );
    assert_eq!(
        bridge.transcoder_target_pt(BridgeEndpoint::WebRtc),
        Some(8),
        "must install Opus→PCMA transcoder toward the SIP PCMA PT"
    );
}
