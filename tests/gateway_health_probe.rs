//! Integration tests for `probe_trunk`.
//!
//! Covers two cases:
//!
//! 1. **Happy path** — a fake UDP SIP peer that echoes a `SIP/2.0 200 OK`
//!    response to any request with the first 7 bytes `OPTIONS`. The probe
//!    should return `ok=true`.
//! 2. **Timeout** — a UDP socket that reads the request but never replies.
//!    The probe should return `ok=false` with `detail == "timeout"`.
//!
//! Both tests build a standalone rsipstack `Endpoint` bound to an ephemeral
//! UDP port (no database, no AppState), and drive `endpoint.serve()` in a
//! background task for the duration of the probe.

use std::time::Duration;

use rsipstack::{
    transaction::endpoint::EndpointBuilder,
    transport::{udp::UdpConnection, TransportLayer},
};
use rustpbx::{
    models::sip_trunk::{Model as TrunkModel, SipTrunkDirection, SipTrunkStatus, SipTransport},
    proxy::gateway_health::probe_trunk,
};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

/// Build a `sip_trunk::Model` pointing at `target` (e.g. "sip:127.0.0.1:5080").
fn make_trunk(target: &str) -> TrunkModel {
    let now = chrono::Utc::now();
    TrunkModel {
        id: 1,
        name: "probe-test".into(),
        status: SipTrunkStatus::Healthy,
        direction: SipTrunkDirection::Outbound,
        sip_server: Some(target.to_string()),
        sip_transport: SipTransport::Udp,
        is_active: true,
        register_enabled: false,
        rewrite_hostport: false,
        created_at: now,
        updated_at: now,
        consecutive_failures: 0,
        consecutive_successes: 0,
        ..Default::default()
    }
}

/// Spin up an rsipstack `Endpoint` on 127.0.0.1:0 and return its
/// `EndpointInnerRef` + a cancel token that can be dropped to clean up.
async fn build_probe_endpoint()
-> (rsipstack::transaction::endpoint::EndpointInnerRef, CancellationToken)
{
    let token = CancellationToken::new();
    let transport_layer = TransportLayer::new(token.clone());
    let conn = UdpConnection::create_connection(
        "127.0.0.1:0".parse().unwrap(),
        None,
        Some(token.child_token()),
    )
    .await
    .expect("bind probe udp");
    transport_layer.add_transport(conn.into());

    let endpoint = EndpointBuilder::new()
        .with_cancel_token(token.clone())
        .with_transport_layer(transport_layer)
        .build();

    let inner = endpoint.inner.clone();
    // Drive the endpoint loop in the background so client transactions can
    // send/receive messages.
    tokio::spawn(async move {
        let _ = endpoint.serve().await;
    });

    (inner, token)
}

/// Build a SIP 200 OK response for a raw OPTIONS request by replacing the
/// first line (`OPTIONS ... SIP/2.0`) with `SIP/2.0 200 OK` and preserving
/// every header (Via/From/To/Call-ID/CSeq/Max-Forwards) verbatim. This is
/// the minimum the client transaction needs to match the response to the
/// pending transaction.
fn synthesise_200_ok(req_bytes: &[u8]) -> Vec<u8> {
    let text = std::str::from_utf8(req_bytes).expect("utf8 sip request");
    // Split off the request line.
    let (_request_line, rest) = text.split_once("\r\n").expect("request line");
    let mut out = String::new();
    out.push_str("SIP/2.0 200 OK\r\n");
    out.push_str(rest);
    // If the original had no Content-Length, make sure we have one so the
    // receiver knows the body is empty. Most stacks still accept the response
    // without it, but belt-and-braces.
    if !rest.to_ascii_lowercase().contains("content-length") {
        // `rest` ends with "\r\n\r\n"; rewind past the terminator so we can
        // insert the header before it.
        let trimmed = out.trim_end_matches("\r\n");
        let trimmed = trimmed.trim_end_matches("\r\n");
        out = format!("{}\r\nContent-Length: 0\r\n\r\n", trimmed);
    }
    out.into_bytes()
}

#[tokio::test]
async fn probe_trunk_happy_path_returns_ok_true() {
    // 1. Fake SIP peer socket on 127.0.0.1:0.
    let peer = UdpSocket::bind("127.0.0.1:0").await.expect("bind peer");
    let peer_addr = peer.local_addr().expect("peer addr");

    let peer_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        let (n, src) = peer.recv_from(&mut buf).await.expect("recv options");
        let req = &buf[..n];
        assert!(
            req.len() >= 7 && &req[..7] == b"OPTIONS",
            "expected OPTIONS request, got: {:?}",
            String::from_utf8_lossy(&req[..req.len().min(80)])
        );
        let resp = synthesise_200_ok(req);
        peer.send_to(&resp, src).await.expect("send 200");
    });

    // 2. Build a real rsipstack endpoint to drive the client transaction.
    let (endpoint_inner, cancel) = build_probe_endpoint().await;

    // 3. Trunk model pointing at the fake peer.
    let target = format!("sip:{}", peer_addr);
    let trunk = make_trunk(&target);

    let outcome = probe_trunk(&endpoint_inner, &trunk, Duration::from_secs(2)).await;
    assert!(
        outcome.ok,
        "expected probe to succeed, got ok=false detail={}",
        outcome.detail
    );
    assert!(
        outcome.latency_ms < 2_000,
        "latency {} should be well under 2s",
        outcome.latency_ms
    );

    peer_task.await.ok();
    cancel.cancel();
}

#[tokio::test]
async fn probe_trunk_times_out_when_peer_silent() {
    // Peer socket that reads the OPTIONS but never responds.
    let peer = UdpSocket::bind("127.0.0.1:0").await.expect("bind peer");
    let peer_addr = peer.local_addr().expect("peer addr");

    let peer_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        // Consume one datagram so the request is actually "received" — then
        // do nothing (silent drop).
        let _ = peer.recv_from(&mut buf).await;
    });

    let (endpoint_inner, cancel) = build_probe_endpoint().await;

    let target = format!("sip:{}", peer_addr);
    let trunk = make_trunk(&target);

    // Short timeout so the test is fast.
    let outcome = probe_trunk(&endpoint_inner, &trunk, Duration::from_millis(300)).await;

    assert!(
        !outcome.ok,
        "expected probe to fail on silent peer, got ok=true detail={}",
        outcome.detail
    );
    assert!(
        outcome.detail.contains("timeout"),
        "expected detail to contain 'timeout', got: {}",
        outcome.detail
    );

    peer_task.abort();
    cancel.cancel();
}
