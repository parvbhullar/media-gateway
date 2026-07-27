//! dispatch_external_media — entry point parallel to dispatch_livekit.
//!
//! Flow:
//!   1. Build the inbound SIP-side RTP PC + SDP answer (shared helper).
//!   2. Bind a localhost UDP socket Q (the rustpbx side of the PCM pipe).
//!   3. Spawn the sidecar with `--call-id/--did/--caller/--port Q`. The
//!      sidecar binds its own ephemeral port, joins its far-end session
//!      (e.g. a LiveKit room), and sends a `READY` datagram to 127.0.0.1:Q.
//!   4. Wait (recv_from) for that first datagram up to `bot_join_timeout_ms`
//!      to learn the sidecar's address; `connect()` the socket to it.
//!   5. Build ExternalMediaBridge (UDP PCM Task A/B) + ExternalMediaTeardown.
//!   6. Return the generalized DispatchOutcome (answer SDP for the 200 OK).
//!
//! rustpbx owns all SIP-side codec/resample + external_ip/latching; the
//! sidecar owns the far-end media entirely. Wire format on the socket: raw
//! 16-bit LE PCM mono, 20 ms frames at the trunk's `pcm_sample_rate`
//! (default 48 kHz → 1920 bytes; 16/24 kHz also allowed), no header.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use rustrtc::IceServer;
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use crate::models::trunk;
use crate::proxy::bridge::common::{DispatchContext, build_inbound_rtp_pc};
use crate::proxy::bridge::session::{BridgeHangupCause, DispatchOutcome, MediaBridge};

use super::media::ExternalMediaBridge;
use super::teardown::ExternalMediaTeardown;

pub async fn dispatch_external_media(
    trunk: &trunk::Model,
    invite_offer_sdp: &str,
    _global_ice_servers: Option<&[IceServer]>,
    ctx: &DispatchContext,
) -> Result<DispatchOutcome> {
    let cfg = trunk.external_media()?;

    // 1. Inbound SIP-side RTP PC + answer.
    let (sip_pc, sip_sdp_answer, sip_codec, sip_dtmf_pt) =
        build_inbound_rtp_pc(invite_offer_sdp, ctx).await?;

    // 2. Bind the rustpbx side of the PCM pipe. Port 0 → kernel-assigned;
    //    no port-allocation race with the sidecar.
    let sock = UdpSocket::bind("127.0.0.1:0")
        .await
        .map_err(|e| anyhow!("failed to bind external_media PCM socket: {e}"))?;
    let q_port = sock
        .local_addr()
        .map_err(|e| anyhow!("PCM socket local_addr failed: {e}"))?
        .port();

    // 3. Spawn the sidecar. `command` is split on whitespace into
    //    program + base args; rustpbx appends the per-call flags.
    let mut parts = cfg.command.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("external_media command is empty"))?;
    let mut command = tokio::process::Command::new(program);
    for arg in parts {
        command.arg(arg);
    }
    // Use `--flag=value` form (single argv entry) rather than two separate
    // args. A SIP Call-ID can begin with '-' (Linphone generates e.g.
    // "-jLtLvcXCy"); passed as a separate arg, an argparse-style parser
    // mistakes it for another option and the sidecar dies with
    // "expected one argument". The `=` form is parsed literally.
    command
        .arg(format!("--call-id={}", ctx.call_id))
        .arg(format!("--did={}", ctx.to_user))
        .arg(format!("--caller={}", ctx.from_user))
        .arg(format!("--port={q_port}"));
    // Only pass the rate when the operator overrides the 48 kHz default —
    // opting in implies the sidecar understands the flag; existing sidecars
    // with strict argparse keep working untouched.
    if cfg.pcm_sample_rate != 48_000 {
        command.arg(format!("--sample-rate={}", cfg.pcm_sample_rate));
    }
    // Forward custom INVITE headers (X-*) as a JSON map in the environment;
    // the sidecar exposes each as a `sip.h.<Header>` participant attribute
    // (mirrors livekit/sip's HeadersToAttrs). Env (not argv) so arbitrary
    // header values need no shell/argparse escaping. The base sip.* attributes
    // (callID/phoneNumber/trunkPhoneNumber/callStatus) are derived sidecar-side
    // from --call-id/--caller/--did, so they aren't repeated here.
    if !ctx.sip_headers.is_empty() {
        let map: std::collections::BTreeMap<&str, &str> = ctx
            .sip_headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        match serde_json::to_string(&map) {
            Ok(json) => {
                command.env("SIP_SIDECAR_HEADERS", json);
            }
            Err(e) => tracing::warn!(
                trunk = %trunk.name,
                "failed to serialize sip_headers for sidecar: {e}; skipping"
            ),
        }
    }
    // Backstop: if teardown is never reached (e.g. dispatcher returns Err
    // below), dropping the Child kills the sidecar.
    command.kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|e| anyhow!("failed to spawn external_media sidecar '{}': {e}", cfg.command))?;
    tracing::info!(
        trunk = %trunk.name,
        call_id = %ctx.call_id,
        rustpbx_port = q_port,
        "external_media sidecar spawned; awaiting READY"
    );

    // 4. Wait for the sidecar's first datagram (READY) to learn its address.
    let timeout = Duration::from_millis(cfg.bot_join_timeout_ms.unwrap_or(15_000));
    let mut buf = [0u8; 64];
    let sidecar_addr = match tokio::time::timeout(timeout, sock.recv_from(&mut buf)).await {
        Ok(Ok((n, addr))) => {
            if &buf[..n] != b"READY" {
                tracing::warn!(
                    trunk = %trunk.name,
                    "external_media sidecar first datagram was not READY ({} bytes); proceeding anyway",
                    n
                );
            }
            addr
        }
        Ok(Err(e)) => {
            // child drops here → killed via kill_on_drop.
            return Err(anyhow!("external_media PCM socket recv_from failed: {e}"));
        }
        Err(_) => {
            let _ = child.start_kill();
            return Err(anyhow!(
                "external_media sidecar did not send READY within {}ms",
                timeout.as_millis()
            ));
        }
    };
    // After connect(), send()/recv() are pinned to the sidecar's address.
    sock.connect(sidecar_addr)
        .await
        .map_err(|e| anyhow!("failed to connect PCM socket to sidecar {sidecar_addr}: {e}"))?;
    tracing::info!(
        trunk = %trunk.name,
        call_id = %ctx.call_id,
        %sidecar_addr,
        "external_media sidecar READY; bridging"
    );

    let sock = Arc::new(sock);
    let cancel = CancellationToken::new();

    // 5. Media-plane bridge + signaling-plane teardown.
    let bridge = Arc::new(ExternalMediaBridge {
        sip_pc,
        sip_codec,
        sip_dtmf_pt,
        sock: sock.clone(),
        pcm_rate: cfg.pcm_sample_rate,
        cancel_token: cancel.clone(),
        trunk_name: trunk.name.clone(),
        disconnect_cause: Arc::new(parking_lot::Mutex::new(BridgeHangupCause::ByCallee)),
        recorder: parking_lot::RwLock::new(None),
    });

    let teardown = Box::new(ExternalMediaTeardown {
        child: parking_lot::Mutex::new(Some(child)),
        sock,
        trunk_name: trunk.name.clone(),
    });

    Ok(DispatchOutcome {
        sip_sdp_answer,
        bridge: bridge as Arc<dyn MediaBridge>,
        teardown,
    })
}
