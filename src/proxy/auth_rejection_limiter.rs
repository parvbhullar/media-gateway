//! Per-source-IP rate limiter for auth-rejection call-records.
//!
//! When an inbound INVITE presents credentials that fail authentication we
//! persist a minimal CDR (see `AuthModule::record_auth_rejection`). The record
//! is keyed on the SIP Call-ID, so retransmits of the *same* attempt de-dupe
//! via the upsert — but an attacker (or a badly-misconfigured peer) can send a
//! flood of INVITEs each with a *unique* Call-ID, which would otherwise write
//! one unbounded DB row per request.
//!
//! This limiter caps how many auth-rejection CDRs a single source IP can
//! produce within a sliding fixed window. The 407 challenge itself is never
//! suppressed — only the CDR write is throttled — so authentication behaviour
//! is unchanged. State is held in an LRU map so memory is bounded regardless of
//! how many distinct source IPs appear.

use lru::LruCache;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Default: at most this many auth-rejection CDRs per source IP per window.
/// Generous for a legitimately misconfigured peer (a few failed attempts per
/// call) while bounding a flood to a small, fixed write rate.
pub const DEFAULT_MAX_PER_WINDOW: u32 = 20;
/// Default sliding window length.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(60);
/// Default number of distinct source IPs to track before LRU eviction.
pub const DEFAULT_CAPACITY: usize = 10_000;

#[derive(Clone, Copy, Debug)]
struct Window {
    started_at: Instant,
    count: u32,
}

/// Sliding fixed-window rate limiter keyed by source IP.
#[derive(Clone)]
pub struct AuthRejectionRateLimiter {
    inner: Arc<Mutex<LruCache<IpAddr, Window>>>,
    max_per_window: u32,
    window: Duration,
}

impl AuthRejectionRateLimiter {
    pub fn new(max_per_window: u32, window: Duration, capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity)
            .unwrap_or_else(|| NonZeroUsize::new(DEFAULT_CAPACITY).unwrap());
        Self {
            inner: Arc::new(Mutex::new(LruCache::new(capacity))),
            max_per_window,
            window,
        }
    }

    /// Returns `true` if a CDR for `ip` may be written now, consuming one slot
    /// in the current window. Returns `false` once the per-window cap is hit.
    pub async fn allow(&self, ip: IpAddr) -> bool {
        self.allow_at(ip, Instant::now()).await
    }

    /// Time-injectable core, for testing.
    async fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut cache = self.inner.lock().await;
        match cache.get_mut(&ip) {
            Some(win) if now.duration_since(win.started_at) < self.window => {
                if win.count >= self.max_per_window {
                    false
                } else {
                    win.count += 1;
                    true
                }
            }
            // No entry, or the previous window has elapsed: start a fresh one.
            _ => {
                cache.put(
                    ip,
                    Window {
                        started_at: now,
                        count: 1,
                    },
                );
                true
            }
        }
    }
}

impl Default for AuthRejectionRateLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_PER_WINDOW, DEFAULT_WINDOW, DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    #[tokio::test]
    async fn allows_up_to_cap_then_blocks_within_window() {
        let limiter = AuthRejectionRateLimiter::new(3, Duration::from_secs(60), 100);
        let t0 = Instant::now();
        let addr = ip(1);

        // First 3 allowed, 4th+ blocked within the same window.
        assert!(limiter.allow_at(addr, t0).await);
        assert!(limiter.allow_at(addr, t0).await);
        assert!(limiter.allow_at(addr, t0).await);
        assert!(!limiter.allow_at(addr, t0).await);
        assert!(!limiter.allow_at(addr, t0).await);
    }

    #[tokio::test]
    async fn window_resets_after_elapsed() {
        let limiter = AuthRejectionRateLimiter::new(2, Duration::from_secs(60), 100);
        let t0 = Instant::now();
        let addr = ip(1);

        assert!(limiter.allow_at(addr, t0).await);
        assert!(limiter.allow_at(addr, t0).await);
        assert!(!limiter.allow_at(addr, t0).await);

        // After the window elapses, the budget is fresh again.
        let t1 = t0 + Duration::from_secs(61);
        assert!(limiter.allow_at(addr, t1).await);
        assert!(limiter.allow_at(addr, t1).await);
        assert!(!limiter.allow_at(addr, t1).await);
    }

    #[tokio::test]
    async fn limits_are_per_ip() {
        let limiter = AuthRejectionRateLimiter::new(1, Duration::from_secs(60), 100);
        let t0 = Instant::now();

        assert!(limiter.allow_at(ip(1), t0).await);
        assert!(!limiter.allow_at(ip(1), t0).await);
        // A different IP has its own independent budget.
        assert!(limiter.allow_at(ip(2), t0).await);
        assert!(!limiter.allow_at(ip(2), t0).await);
    }
}
