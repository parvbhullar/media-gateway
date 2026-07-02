//! Crash-proof SIP domain resolver.
//!
//! rsipstack's `DefaultDomainResolver` panics at construction when the
//! host's DNS config can't be parsed by hickory — e.g. macOS writing a
//! scoped IPv6 entry like `nameserver fe80::…%en0` into /etc/resolv.conf.
//! An SBC must not die at startup because the local router advertised a
//! link-local DNS server.
//!
//! This drop-in replacement builds the resolver fallibly, degrading in
//! order:
//!   1. hickory with system DNS config (full RFC 3263 SRV + A/AAAA),
//!   2. hickory with its default public-DNS config (still full SRV),
//!   3. OS `getaddrinfo` via `tokio::net::lookup_host` (A/AAAA only, no
//!      SRV) — the same behavior as rsipstack's `srv_lookup`-disabled
//!      DummyResolver.

use async_trait::async_trait;
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{LookupIpStrategy, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RData;
use rsipstack::resolver::sip_resolver::{LookupSource, SrvRecord, resolve_logic};
use rsipstack::sip::{Host, HostWithPort, Transport};
use rsipstack::transport::SipAddr;
use rsipstack::transport::transport_layer::DomainResolver;
use std::net::IpAddr;
use std::sync::Arc;
use tracing::warn;

/// The one true way to build a `TransportLayer` in this codebase — always
/// carries the crash-proof resolver. Direct `TransportLayer::new` panics
/// on hosts with unparsable resolv.conf entries.
pub fn new_transport_layer(
    cancel: tokio_util::sync::CancellationToken,
) -> rsipstack::transport::TransportLayer {
    rsipstack::transport::TransportLayer::new_with_domain_resolver(
        cancel,
        Box::new(RobustDomainResolver::new()),
    )
}

pub struct RobustDomainResolver {
    source: Backend,
}

enum Backend {
    Hickory(Arc<TokioResolver>),
    /// getaddrinfo only — no SRV lookups.
    SystemOnly,
}

impl RobustDomainResolver {
    pub fn new() -> Self {
        let resolver = match TokioResolver::builder_tokio() {
            Ok(mut b) => {
                b.options_mut().ip_strategy = LookupIpStrategy::Ipv4thenIpv6;
                b.build().ok()
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "system DNS config unusable for SRV resolver; \
                     falling back to public DNS"
                );
                let mut b = TokioResolver::builder_with_config(
                    ResolverConfig::default(),
                    TokioRuntimeProvider::default(),
                );
                b.options_mut().ip_strategy = LookupIpStrategy::Ipv4thenIpv6;
                b.build().ok()
            }
        };
        let source = match resolver {
            Some(r) => Backend::Hickory(Arc::new(r)),
            None => {
                warn!(
                    "no usable hickory DNS resolver; SIP domain resolution \
                     degrades to getaddrinfo (no SRV records)"
                );
                Backend::SystemOnly
            }
        };
        Self { source }
    }
}

impl Default for RobustDomainResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LookupSource for Backend {
    async fn lookup_srv(&self, name: &str) -> Result<Vec<SrvRecord>, String> {
        match self {
            // Mirrors rsipstack's private HickorySource.
            Backend::Hickory(r) => match r.srv_lookup(name).await {
                Ok(records) => Ok(records
                    .message()
                    .all_sections()
                    .filter_map(|rec| match &rec.data {
                        RData::SRV(srv) => Some(SrvRecord {
                            target: srv
                                .target
                                .to_string()
                                .trim_end_matches('.')
                                .to_string(),
                            port: srv.port,
                            priority: srv.priority,
                            weight: srv.weight,
                        }),
                        _ => None,
                    })
                    .collect()),
                Err(e) => Err(e.to_string()),
            },
            // No SRV support — resolve_logic falls through to A/AAAA.
            Backend::SystemOnly => Ok(vec![]),
        }
    }

    async fn lookup_a_aaaa(&self, name: &str) -> Result<Vec<IpAddr>, String> {
        match self {
            Backend::Hickory(r) => match r.lookup_ip(name).await {
                Ok(records) => Ok(records.iter().collect()),
                Err(e) => Err(e.to_string()),
            },
            Backend::SystemOnly => {
                match tokio::net::lookup_host((name, 5060u16)).await {
                    Ok(addrs) => Ok(addrs.map(|a| a.ip()).collect()),
                    Err(e) => Err(e.to_string()),
                }
            }
        }
    }
}

#[async_trait]
impl DomainResolver for RobustDomainResolver {
    /// Mirrors `DefaultDomainResolver::resolve_with_lookup`: RFC 3263
    /// resolution of a domain target to a concrete transport address.
    async fn resolve(&self, target: &SipAddr) -> rsipstack::Result<SipAddr> {
        let domain = match &target.addr.host {
            Host::Domain(domain) => domain,
            _ => {
                return Err(rsipstack::Error::DnsResolutionError(
                    target.addr.to_string(),
                ));
            }
        };
        let secure = matches!(
            target.r#type,
            Some(Transport::Tls) | Some(Transport::Wss) | Some(Transport::TlsSctp)
        );
        let addrs = resolve_logic(
            &self.source,
            domain,
            target.addr.port,
            target.r#type,
            secure,
        )
        .await
        .map_err(|e| {
            rsipstack::Error::DnsResolutionError(format!("{}: {}", target.addr, e))
        })?;

        if let Some(first) = addrs.first() {
            return Ok(SipAddr {
                r#type: Some(first.transport),
                addr: HostWithPort {
                    host: Host::IpAddr(first.addr.ip()),
                    port: Some(first.addr.port().into()),
                },
            });
        }
        Err(rsipstack::Error::DnsResolutionError(target.addr.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of this module: construction must never panic,
    /// whatever state the host's resolv.conf is in.
    #[tokio::test]
    async fn construction_never_panics() {
        let _ = RobustDomainResolver::new();
    }

    /// IP-literal hosts resolve without any DNS at all.
    #[tokio::test]
    async fn ip_literal_resolves_without_dns() {
        let r = RobustDomainResolver::new();
        let target = SipAddr {
            r#type: Some(Transport::Udp),
            addr: HostWithPort {
                host: Host::Domain("192.0.2.10".into()),
                port: Some(5060.into()),
            },
        };
        let resolved = r.resolve(&target).await.expect("ip literal resolves");
        assert_eq!(
            resolved.addr.host,
            Host::IpAddr("192.0.2.10".parse().unwrap())
        );
    }
}
