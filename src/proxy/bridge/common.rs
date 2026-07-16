//! Shared helpers used by both the WebRTC and LiveKit bridge kinds.
//!
//! These were originally co-located in `webrtc.rs`; they were pulled out
//! into this module as part of the LiveKit trunk work (Phase 1 / Task
//! 1.2) so the LiveKit dispatcher can reuse them verbatim. No semantic
//! changes — pure code relocation.

use anyhow::{Result, anyhow};
use rustrtc::{
    IceServer, PeerConnection, RtcConfiguration, RtpCodecParameters, SdpType,
    config::{AudioCapability, MediaCapabilities, SdpCompatibilityMode, VideoCapability},
    sdp::SessionDescription,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::Value;

use crate::models::trunk;
use crate::proxy::bridge::session::BridgeKind;

/// Per-INVITE context handed to per-kind dispatchers, carrying caller
/// details needed for template substitution.
#[derive(Clone, Debug, Default)]
pub struct DispatchContext {
    pub call_id: String,
    pub from_user: String,
    pub to_user: String,
    /// Public IP to advertise in the SIP-leg SDP answer (`c=`/candidate
    /// address). Sourced from the proxy's `rtp_config.external_ip`. When
    /// `None`, rustrtc falls back to the local bind interface — which is a
    /// private address on a NAT'd cloud VM (e.g. Oracle `10.0.0.x`),
    /// making the advertised RTP address unroutable from the SIP caller.
    pub external_ip: Option<String>,
    /// Local interface to bind the SIP-leg RTP socket to. From
    /// `rtp_config.bind_ip`. `None` = rustrtc default selection.
    pub bind_ip: Option<String>,
    /// SIP-leg RTP port range (from `rtp_config`), so the bound port falls
    /// inside the operator's firewall-opened range.
    pub rtp_start_port: Option<u16>,
    pub rtp_end_port: Option<u16>,
    /// Custom SIP headers lifted from the INVITE (e.g. `X-*`), to be exposed
    /// to the bridge far-end as participant attributes — mirrors
    /// livekit/sip's `HeadersToAttrs` → `sip.h.<Header>`. Only the
    /// `external_media` kind consumes this today; empty for other kinds.
    pub sip_headers: Vec<(String, String)>,
}

/// Resolve the effective ICE-server list for this trunk.
///
/// Precedence: per-trunk `kind_config.ice_servers` (a JSON array) wins; if
/// absent or empty, falls back to `global_ice_servers`; otherwise empty
/// (host candidates only).
pub(crate) fn resolve_ice_servers(
    per_trunk: Option<&Value>,
    global_ice_servers: Option<&[IceServer]>,
) -> Result<Vec<IceServer>> {
    if let Some(v) = per_trunk
        && !v.is_null()
    {
        let parsed: Vec<IceServer> = serde_json::from_value(v.clone())
            .map_err(|e| anyhow!("failed to parse per-trunk ice_servers: {e}"))?;
        if !parsed.is_empty() {
            return Ok(parsed);
        }
    }
    Ok(global_ice_servers.map(|s| s.to_vec()).unwrap_or_default())
}

pub(crate) fn audio_capability_for(codec: &str) -> Result<AudioCapability> {
    match codec.to_ascii_lowercase().as_str() {
        "opus" => Ok(AudioCapability::opus()),
        "g722" => Ok(AudioCapability::g722()),
        other => Err(anyhow!(
            "audio_codec '{other}' not supported (allowed: opus, g722)"
        )),
    }
}

pub(crate) fn codec_params_from_capability(cap: &AudioCapability) -> RtpCodecParameters {
    RtpCodecParameters {
        payload_type: cap.payload_type,
        clock_rate: cap.clock_rate,
        channels: cap.channels,
    }
}

/// SIP-side audio capabilities offered for negotiation. Ordered by
/// quality-preference: the carrier's offer is matched against this list in
/// the order it appears in the carrier's m=audio line, so this ordering
/// only matters when we're the offerer (we're the answerer in the inbound
/// flow, so the carrier's preference wins). Each entry is one we can
/// actually transcode to/from the WebRTC side via `BridgePeer`.
///
/// G.729 is intentionally omitted — it's patent-encumbered and
/// rustrtc's default build doesn't carry a real encoder/decoder.
///
/// `telephone-event` (RFC 2833 / RFC 4733 DTMF) is offered so the carrier
/// can negotiate out-of-band DTMF. The bridge's data plane already
/// detects DTMF packets and passes them through without transcoding
/// (`BridgePeer::forward_track_to_sender` checks `dtmf_sink`), provided
/// we call `set_dtmf_sink` on both endpoints after negotiation —
/// `dispatch_webrtc` does that below.
pub(crate) fn sip_side_audio_offer() -> Vec<AudioCapability> {
    vec![
        AudioCapability::opus(),
        AudioCapability::g722(),
        AudioCapability::pcmu(),
        AudioCapability::pcma(),
        AudioCapability::telephone_event(),
    ]
}

/// Build the inbound SIP-side RTP PeerConnection from the INVITE's offer
/// SDP, producing an SDP answer suitable for the 200 OK.
///
/// Returns the configured PC, the answer SDP string, the
/// [`AudioCapability`] that the SIP-side negotiation actually settled on
/// (the audio voice codec — drives the transcoder), and the
/// telephone-event payload type negotiated for RFC 2833 DTMF
/// pass-through (`None` if the carrier didn't offer telephone-event).
pub(crate) async fn build_inbound_rtp_pc(
    invite_offer_sdp: &str,
    ctx: &DispatchContext,
) -> Result<(PeerConnection, String, AudioCapability, Option<u8>)> {
    let cfg = RtcConfiguration {
        transport_mode: rustrtc::TransportMode::Rtp,
        media_capabilities: Some(MediaCapabilities {
            audio: sip_side_audio_offer(),
            video: Vec::<VideoCapability>::new(),
            application: None,
        }),
        sdp_compatibility: SdpCompatibilityMode::Standard,
        // Advertise the operator's public IP in the answer SDP instead of
        // the local bind interface. Without this, a NAT'd cloud VM answers
        // with its private address (e.g. Oracle `10.0.0.x`) and the SIP
        // caller's RTP is sent to an unroutable destination → no inbound
        // audio. Mirrors what the legacy `proxy_call::sip_session` path
        // already does via `rtp_config.external_ip`.
        external_ip: ctx.external_ip.clone(),
        bind_ip: ctx.bind_ip.clone(),
        rtp_start_port: ctx.rtp_start_port,
        rtp_end_port: ctx.rtp_end_port,
        // Symmetric RTP / latching: learn the caller's real RTP source
        // address from the first inbound packet rather than trusting the
        // SDP `c=` line. SIP softphones behind NAT (e.g. Linphone
        // advertising a `192.168.x` LAN address) require this for the
        // return path to reach them.
        enable_latching: true,
        ..Default::default()
    };
    let pc = PeerConnection::new(cfg);
    let offer = SessionDescription::parse(SdpType::Offer, invite_offer_sdp)
        .map_err(|e| anyhow!("failed to parse INVITE offer SDP: {e:?}"))?;
    pc.set_remote_description(offer)
        .await
        .map_err(|e| anyhow!("set_remote_description failed on RTP leg: {e}"))?;
    let answer = pc
        .create_answer()
        .await
        .map_err(|e| anyhow!("create_answer failed on RTP leg: {e}"))?;
    pc.set_local_description(answer)
        .map_err(|e| anyhow!("set_local_description failed on RTP leg: {e}"))?;
    let local_desc = pc
        .local_description()
        .ok_or_else(|| anyhow!("RTP leg has no local description after set_local_description"))?;
    // rustrtc emits a=sendonly in RTP-mode answers because no local track is
    // attached at this point — but the bridge will pump RTP from the WebRTC
    // leg into this socket once media starts flowing, and we also need the
    // SIP peer to send us its audio. Force sendrecv so the caller actually
    // transmits.
    let answer_sdp = local_desc
        .to_sdp_string()
        .replace("a=sendonly", "a=sendrecv")
        .replace("a=recvonly", "a=sendrecv");

    // Compute the negotiated codec by intersecting the carrier's offer
    // (remote_description) with our supported set. In RFC 3264 terms the
    // answerer picks the first codec in the offer that it also supports;
    // we mirror that here against `sip_side_audio_offer()`.
    //
    // We can't rely on `local_desc.first_audio_section().to_audio_capabilities()`
    // because rustrtc's Rtp-mode "answer" enumerates *our* full offer set
    // (not the single chosen codec), which would make every call look
    // like Opus regardless of what the carrier actually wanted.
    let offer_parsed = SessionDescription::parse(SdpType::Offer, invite_offer_sdp)
        .map_err(|e| anyhow!("failed to re-parse INVITE offer for codec negotiation: {e:?}"))?;
    let supported = sip_side_audio_offer();
    let offered: Vec<AudioCapability> = offer_parsed
        .audio_sections()
        .flat_map(|sec| sec.to_audio_capabilities())
        .collect();
    let is_dtmf = |c: &AudioCapability| c.codec_name.eq_ignore_ascii_case("telephone-event");
    // Voice codec: first offered audio codec that's in our supported set,
    // excluding telephone-event (which is signaling, not voice).
    let negotiated = offered
        .iter()
        .find(|c| {
            !is_dtmf(c)
                && supported
                    .iter()
                    .any(|s| s.codec_name.eq_ignore_ascii_case(&c.codec_name))
        })
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "carrier offered no voice codecs the bridge supports — supported: {:?}, \
                 offered: {:?}",
                supported
                    .iter()
                    .filter(|c| !is_dtmf(c))
                    .map(|c| c.codec_name.as_str())
                    .collect::<Vec<_>>(),
                offered.iter().map(|c| c.codec_name.clone()).collect::<Vec<_>>()
            )
        })?;
    // Telephone-event (RFC 2833 DTMF) PT: the carrier picks the dynamic PT
    // for it (often 101 but can be anything ≥ 96). `None` when not offered.
    let dtmf_pt = offered.iter().find(|c| is_dtmf(c)).map(|c| c.payload_type);
    Ok((pc, answer_sdp, negotiated, dtmf_pt))
}

/// Fetch the row for an external-bridge trunk kind, validating it's
/// active and that its `kind` column matches `expected_kind`.
pub async fn fetch_external_trunk(
    db: &DatabaseConnection,
    trunk_name: &str,
    expected_kind: BridgeKind,
) -> Result<trunk::Model> {
    let row = trunk::Entity::find()
        .filter(trunk::Column::Name.eq(trunk_name))
        .one(db)
        .await
        .map_err(|e| anyhow!("db error looking up trunk '{}': {}", trunk_name, e))?
        .ok_or_else(|| anyhow!("trunk '{}' not found", trunk_name))?;
    if !row.is_active {
        return Err(anyhow!("trunk '{}' is disabled", trunk_name));
    }
    if row.kind != expected_kind.as_str() {
        return Err(anyhow!(
            "trunk '{}' has kind '{}', expected '{}'",
            trunk_name,
            row.kind,
            expected_kind.as_str()
        ));
    }
    Ok(row)
}

// ── Remote-header exposure filter (control-plane) ────────────────────────────
//
// A single allow-list filter over the carrier INVITE's headers, shared by three
// consumers: `GET /calls/{id}` (bridge entries), the `bridge.call.incoming`
// webhook payload, and — behind the per-trunk `expose_headers_to_bot` flag — the
// signaling adapter request context. Only untyped (`Header::Other`) headers are
// considered: From/To/Contact are typed and the bot already receives them via
// call_id/from/to. Deny wins over allow so credentials/topology never leak even
// if a name also matches the allow-list.

fn header_denied(lower_name: &str) -> bool {
    lower_name.starts_with("content-")
        || matches!(
            lower_name,
            "via" | "route"
                | "record-route"
                | "cseq"
                | "max-forwards"
                | "authorization"
                | "proxy-authorization"
                | "www-authenticate"
                | "proxy-authenticate"
                | "session-expires"
                | "min-se"
        )
}

fn header_allowed(lower_name: &str) -> bool {
    lower_name.starts_with("x-")
        || matches!(
            lower_name,
            "diversion"
                | "p-asserted-identity"
                | "p-preferred-identity"
                | "remote-party-id"
                | "history-info"
                | "referred-by"
                | "user-to-user"
        )
}

/// Filter carrier INVITE headers down to the safe, bot-useful set (identity +
/// `X-*`), dropping sensitive/topology headers. Deny-list wins over allow-list.
pub fn filter_remote_headers<'a>(
    headers: impl Iterator<Item = &'a rsipstack::sip::Header>,
) -> Vec<(String, String)> {
    headers
        .filter_map(|h| match h {
            rsipstack::sip::Header::Other(name, value) => {
                let lower = name.to_ascii_lowercase();
                if header_denied(&lower) {
                    None
                } else if header_allowed(&lower) {
                    Some((name.clone(), value.clone()))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}

/// Convenience over [`filter_remote_headers`] taking a whole request.
pub fn filtered_remote_headers(req: &rsipstack::sip::Request) -> Vec<(String, String)> {
    filter_remote_headers(req.headers.iter())
}

#[cfg(test)]
mod header_filter_tests {
    use super::*;
    use rsipstack::sip::Header;

    #[test]
    fn passes_identity_and_x_headers_denies_sensitive() {
        let headers = vec![
            Header::Other(
                "P-Asserted-Identity".into(),
                "<sip:+15551234567@carrier>".into(),
            ),
            Header::Other("Diversion".into(), "<sip:+15559999999@carrier>".into()),
            Header::Other("X-Custom-Tag".into(), "abc".into()),
            Header::Other("Proxy-Authorization".into(), "Digest realm=...".into()),
            Header::Other("Content-Type".into(), "application/sdp".into()),
            Header::Other("Session-Expires".into(), "1800".into()),
            Header::Other("Route".into(), "<sip:proxy>".into()),
        ];
        let out = filter_remote_headers(headers.iter());
        let names: Vec<String> = out.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();

        // Identity + X-* survive.
        assert!(names.contains(&"p-asserted-identity".to_string()));
        assert!(names.contains(&"diversion".to_string()));
        assert!(names.contains(&"x-custom-tag".to_string()));
        // Sensitive/topology denied (deny wins).
        assert!(!names.iter().any(|n| n == "proxy-authorization"));
        assert!(!names.iter().any(|n| n == "content-type"));
        assert!(!names.iter().any(|n| n == "session-expires"));
        assert!(!names.iter().any(|n| n == "route"));
        assert_eq!(out.len(), 3, "exactly the 3 allowed headers pass");
    }
}
