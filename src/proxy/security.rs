//! Phase 10 Plan 10-02 — `SecurityModule` ProxyModule (SEC-01..SEC-03).
//!
//! Hot-path enforcement for the security suite. Registered FIRST in the
//! module chain (before `acl` and `auth`) so firewall deny rules, block
//! list lookups, and flood thresholds short-circuit untrusted traffic
//! before any heavier processing.
//!
//! Order on `on_transaction_begin` (CONTEXT.md D-15, RESEARCH.md RISK-02):
//!   1. Extract source IP from the topmost Via header. The exact 3-line
//!      pattern is copied verbatim from `acl.rs` so all modules agree on
//!      what "source IP" means (RISK-01).
//!   2. Firewall CIDR evaluation against `firewall_rules_snapshot()` in
//!      position order. First match wins. `deny` → reply 403 + Abort;
//!      `allow` → break and continue. Default policy when no rule matches
//!      is "continue" (matches typical firewall semantics — explicit deny
//!      lists win, everything else falls through to subsequent modules).
//!   3. Block-list lookup via `is_ip_blocked`. Hit → 403 + Abort.
//!   4. Flood threshold via `record_message`. Breach → 503 + Abort.
//!   5. Otherwise → Continue.
//!
//! Spam markers (`mark_as_spam`) are set so downstream code can short-
//! circuit and so call records reflect the rejection reason.

use super::{ProxyAction, ProxyModule, server::SipServerRef};
use crate::call::TransactionCookie;
use crate::call::cookie::SpamResult;
use crate::config::ProxyConfig;
use anyhow::Result;
use async_trait::async_trait;
use rsipstack::sip::prelude::HeadersExt;
use rsipstack::{transaction::transaction::Transaction, transport::SipConnection};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

/// In-memory CIDR matcher used for firewall rule evaluation. Mirrors the
/// `IpNetwork` struct in `acl.rs` (kept private to that module). We
/// duplicate the small struct here rather than re-exporting to keep the
/// two modules independently evolvable.
#[derive(Debug, Clone)]
struct IpNetwork {
    network: IpAddr,
    prefix_len: u8,
}

impl IpNetwork {
    fn contains(&self, ip: &IpAddr) -> bool {
        match (self.network, ip) {
            (IpAddr::V4(network), IpAddr::V4(ip)) => {
                let mask = if self.prefix_len == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix_len)
                };
                (u32::from(network) & mask) == (u32::from(*ip) & mask)
            }
            (IpAddr::V6(network), IpAddr::V6(ip)) => {
                let network_segments = network.segments();
                let ip_segments = ip.segments();
                let mut remaining_bits = self.prefix_len;
                for i in 0..8 {
                    if remaining_bits == 0 {
                        return true;
                    }
                    let bits = std::cmp::min(remaining_bits, 16);
                    let mask = if bits == 16 {
                        0xFFFF
                    } else {
                        0xFFFF << (16 - bits)
                    };
                    if (network_segments[i] & mask) != (ip_segments[i] & mask) {
                        return false;
                    }
                    if remaining_bits >= 16 {
                        remaining_bits -= 16;
                    } else {
                        break;
                    }
                }
                true
            }
            _ => false,
        }
    }
}

/// Parse a firewall rule's `cidr` field. Accepts either a bare IP literal
/// (treated as a /32 or /128) or a CIDR string. Returns `None` on parse
/// failure so a malformed rule is skipped rather than aborting traffic.
fn parse_cidr(cidr: &str) -> Option<IpNetwork> {
    let cidr = cidr.trim();
    if let Some((ip_str, prefix_str)) = cidr.split_once('/') {
        let ip = IpAddr::from_str(ip_str).ok()?;
        let prefix_len: u8 = prefix_str.parse().ok()?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if prefix_len > max {
            return None;
        }
        Some(IpNetwork {
            network: ip,
            prefix_len,
        })
    } else {
        let ip = IpAddr::from_str(cidr).ok()?;
        let prefix_len = if ip.is_ipv4() { 32 } else { 128 };
        Some(IpNetwork {
            network: ip,
            prefix_len,
        })
    }
}

/// SecurityModule wires the hot path to `SecurityState` (Wave 1).
pub struct SecurityModule {
    server: SipServerRef,
}

impl SecurityModule {
    /// ProxyModule factory used by `SipServerBuilder::register_module`.
    pub fn create(
        server: SipServerRef,
        _config: Arc<ProxyConfig>,
    ) -> Result<Box<dyn ProxyModule>> {
        Ok(Box::new(Self { server }))
    }
}

#[async_trait]
impl ProxyModule for SecurityModule {
    fn name(&self) -> &str {
        "security"
    }

    async fn on_start(&mut self) -> Result<()> {
        debug!("security module started");
        Ok(())
    }

    async fn on_stop(&self) -> Result<()> {
        debug!("security module stopped");
        Ok(())
    }

    async fn on_transaction_begin(
        &self,
        _token: CancellationToken,
        tx: &mut Transaction,
        cookie: TransactionCookie,
    ) -> Result<ProxyAction> {
        // (1) Extract source IP — verbatim acl.rs pattern (RISK-01).
        let via = tx.original.via_header()?;
        let (_, target) = SipConnection::parse_target_from_via(via)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let from_addr: IpAddr = target.host.try_into()?;

        // (2) Firewall CIDR evaluation. First match wins.
        let rules = self.server.security_state.firewall_rules_snapshot();
        for rule in &rules {
            let Some(network) = parse_cidr(&rule.cidr) else {
                continue;
            };
            if !network.contains(&from_addr) {
                continue;
            }
            let action = rule.action.to_ascii_lowercase();
            if action == "deny" {
                info!(
                    method = tx.original.method().to_string(),
                    ip = %from_addr,
                    cidr = %rule.cidr,
                    "IP denied by firewall rule"
                );
                cookie.mark_as_spam(SpamResult::IpBlacklist);
                tx.reply(rsipstack::sip::StatusCode::Forbidden).await.ok();
                return Ok(ProxyAction::Abort);
            }
            // allow: stop evaluating further rules and fall through.
            break;
        }

        // (3) Block-list lookup.
        if self.server.security_state.is_ip_blocked(from_addr) {
            info!(
                method = tx.original.method().to_string(),
                ip = %from_addr,
                "IP rejected by security block list"
            );
            cookie.mark_as_spam(SpamResult::IpBlacklist);
            tx.reply(rsipstack::sip::StatusCode::Forbidden).await.ok();
            return Ok(ProxyAction::Abort);
        }

        // (4) Flood check (per-IP sliding window).
        if self.server.security_state.record_message(from_addr) {
            info!(
                method = tx.original.method().to_string(),
                ip = %from_addr,
                "flood threshold breached, rejecting"
            );
            cookie.mark_as_spam(SpamResult::Spam);
            tx.reply(rsipstack::sip::StatusCode::ServiceUnavailable)
                .await
                .ok();
            return Ok(ProxyAction::Abort);
        }

        Ok(ProxyAction::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cidr_bare_ipv4() {
        let n = parse_cidr("10.0.0.5").unwrap();
        assert_eq!(n.prefix_len, 32);
        assert!(n.contains(&"10.0.0.5".parse().unwrap()));
        assert!(!n.contains(&"10.0.0.6".parse().unwrap()));
    }

    #[test]
    fn parse_cidr_v4_subnet() {
        let n = parse_cidr("10.0.0.0/8").unwrap();
        assert!(n.contains(&"10.1.2.3".parse().unwrap()));
        assert!(!n.contains(&"11.0.0.1".parse().unwrap()));
    }

    #[test]
    fn parse_cidr_invalid_returns_none() {
        assert!(parse_cidr("not-an-ip").is_none());
        assert!(parse_cidr("10.0.0.0/40").is_none());
    }

    #[test]
    fn parse_cidr_v6_subnet() {
        let n = parse_cidr("2001:db8::/32").unwrap();
        assert!(n.contains(&"2001:db8::1".parse().unwrap()));
        assert!(!n.contains(&"2001:dead::1".parse().unwrap()));
    }
}
