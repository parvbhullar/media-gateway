//! Eviction of dead pooled stream connections on send failure.
//!
//! The pool (`transport_layer.connections`) is keyed by the connection's own
//! remote address. When a send over a pooled TCP/TLS connection fails with a
//! stale-socket error (EPIPE/RST), `Transaction::evict_if_stale` must remove
//! that entry so the next lookup dials fresh — and must NOT fire for
//! non-stale errors or unreliable (UDP) connections, whose pool entry is the
//! shared listener socket.

use crate::sip::headers::*;
use crate::transaction::key::{TransactionKey, TransactionRole};
use crate::transaction::transaction::Transaction;
use crate::transport::udp::UdpConnection;
use crate::transport::{tcp::TcpConnection, SipAddr, SipConnection};
use crate::{Error, Result};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::sleep;

fn make_register_request(
    host_with_port: crate::sip::HostWithPort,
    branch: &str,
) -> crate::sip::Request {
    crate::sip::Request {
        method: crate::sip::Method::Register,
        uri: crate::sip::Uri {
            scheme: Some(crate::sip::Scheme::Sip),
            host_with_port,
            ..Default::default()
        },
        headers: vec![
            Via::new(format!("SIP/2.0/TCP 127.0.0.1:5060;branch=z9hG4bK{}", branch)).into(),
            CSeq::new("1 REGISTER").into(),
            From::new("<sip:alice@example.com>;tag=stale-test").into(),
            CallId::new(format!("stale-eviction-{}@example.com", branch)).into(),
        ]
        .into(),
        version: crate::sip::Version::V2,
        body: vec![],
    }
}

fn make_client_tx(
    endpoint: &super::Endpoint,
    host_with_port: crate::sip::HostWithPort,
    branch: &str,
) -> Result<Transaction> {
    let request = make_register_request(host_with_port, branch);
    let key = TransactionKey::from_request(&request, TransactionRole::Client)?;
    Ok(Transaction::new_client(
        key,
        request,
        endpoint.inner.clone(),
        None,
    ))
}

/// Pool a real TCP connection, kill the peer with an RST, and verify that a
/// transaction send over the dead socket both errors and evicts the pool
/// entry (observable: the next lookup has to dial, and dialing the closed
/// listener fails).
#[tokio::test]
async fn test_stale_send_evicts_pooled_tcp_connection() -> Result<()> {
    let endpoint = super::create_test_endpoint(None).await?;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let listener_addr = listener.local_addr().expect("local addr");
    let sip_addr = SipAddr {
        r#type: Some(crate::sip::transport::Transport::Tcp),
        addr: listener_addr.into(),
    };

    let (client_conn, accepted) =
        tokio::join!(TcpConnection::connect(&sip_addr, None), listener.accept());
    let client_conn = client_conn?;
    let (accepted_stream, _) = accepted.expect("accept");

    let pooled: SipConnection = client_conn.into();
    endpoint
        .inner
        .transport_layer
        .add_connection(pooled.clone());

    // RST the socket (linger 0 discards the FIN handshake) and close the
    // listener so any post-eviction dial attempt observably fails.
    // Deprecated because linger blocks drop — a zero linger has nothing to wait for.
    #[allow(deprecated)]
    accepted_stream
        .set_linger(Some(Duration::from_secs(0)))
        .expect("set linger");
    drop(accepted_stream);
    drop(listener);

    // Writes only start failing once the RST has been processed locally;
    // probe with raw sends (these bypass the transaction layer and must NOT
    // evict) until the socket is observably dead.
    let probe = make_register_request(listener_addr.into(), "probe");
    let mut socket_dead = false;
    for _ in 0..100 {
        if pooled.send(probe.clone().into(), None).await.is_err() {
            socket_dead = true;
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert!(socket_dead, "peer RST never surfaced on the client socket");

    // The dead connection is still pooled (raw sends don't evict).
    let (still_pooled, _) = endpoint
        .inner
        .transport_layer
        .lookup(&sip_addr, None)
        .await
        .expect("dead connection must still be pooled before the tx send");
    assert_eq!(still_pooled.get_addr(), pooled.get_addr());

    // A transaction send over the dead pooled socket must error...
    let mut tx = make_client_tx(&endpoint, listener_addr.into(), "txdead")?;
    tx.destination = Some(sip_addr.clone());
    tx.send()
        .await
        .expect_err("send over a dead pooled socket must fail");

    // ...and must have evicted the pool entry: the next lookup dials fresh,
    // which fails because the listener is gone. A surviving pool entry would
    // make this lookup succeed without dialing.
    assert!(
        endpoint
            .inner
            .transport_layer
            .lookup(&sip_addr, None)
            .await
            .is_err(),
        "pool entry must be evicted after a stale send failure"
    );
    Ok(())
}

/// Non-stale IO errors must leave the pool entry alone.
#[tokio::test]
async fn test_non_stale_error_does_not_evict() -> Result<()> {
    let endpoint = super::create_test_endpoint(None).await?;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let listener_addr = listener.local_addr().expect("local addr");
    let sip_addr = SipAddr {
        r#type: Some(crate::sip::transport::Transport::Tcp),
        addr: listener_addr.into(),
    };
    let (client_conn, accepted) =
        tokio::join!(TcpConnection::connect(&sip_addr, None), listener.accept());
    let pooled: SipConnection = client_conn?.into();
    endpoint
        .inner
        .transport_layer
        .add_connection(pooled.clone());
    drop(accepted);
    drop(listener);

    let tx = make_client_tx(&endpoint, listener_addr.into(), "timeout")?;
    tx.evict_if_stale(
        &pooled,
        &Error::IoError(std::io::Error::new(std::io::ErrorKind::TimedOut, "slow")),
    );

    // Listener is gone, so only a surviving pool entry can satisfy lookup.
    endpoint
        .inner
        .transport_layer
        .lookup(&sip_addr, None)
        .await
        .expect("non-stale error must not evict the pooled connection");
    Ok(())
}

/// Unreliable (UDP) connections are exempt: their pool entry is the shared
/// listener socket and must survive per-peer resets.
#[tokio::test]
async fn test_udp_connection_is_never_evicted() -> Result<()> {
    let endpoint = super::create_test_endpoint(None).await?;

    let udp = UdpConnection::create_connection("127.0.0.1:0".parse()?, None, None).await?;
    let pooled: SipConnection = udp.into();
    let udp_addr = pooled.get_addr().clone();
    endpoint
        .inner
        .transport_layer
        .add_connection(pooled.clone());

    let tx = make_client_tx(&endpoint, udp_addr.addr.clone(), "udp")?;
    tx.evict_if_stale(
        &pooled,
        &Error::IoError(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "icmp port unreachable",
        )),
    );

    let (got, _) = endpoint
        .inner
        .transport_layer
        .lookup(&udp_addr, None)
        .await
        .expect("udp pool entry must survive stale-looking errors");
    assert_eq!(got.get_addr(), &udp_addr);
    Ok(())
}
