//! Per-kind health-probe registry.
//!
//! Mirrors the `kind_schemas` validator-registry pattern: each trunk `kind`
//! registers a `KindHealthProber` that knows how to liveness-probe trunks of
//! that kind. The [`gateway_health`](crate::proxy::gateway_health) monitor
//! looks up the prober by `trunk.kind` at every tick; trunks whose kind has
//! no prober are skipped (with a one-time warn) instead of erroring.
//!
//! Built-in probers:
//! - `"sip"` — wraps [`crate::proxy::gateway_health::probe_trunk`] (SIP
//!   OPTIONS keepalive, unchanged behavior).
//! - `"webrtc"` — HTTP `GET` against the signaling endpoint configured on
//!   the trunk's `kind_config.endpoint_url`. Any 2xx/3xx counts as healthy;
//!   4xx/5xx/timeout count as a failure. This is a cheap probe that doesn't
//!   consume a real session slot on the remote bot. Operators who want
//!   stronger liveness can layer a canary POST later via the same registry.
//!
//! Both probers honour `probe_timeout`. Failure detail strings are
//! propagated into the existing tally/transition logs.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use async_trait::async_trait;

use crate::models::sip_trunk::Model as TrunkModel;
use crate::proxy::gateway_health::ProbeOutcome;

#[async_trait]
pub trait KindHealthProber: Send + Sync {
    /// Probe a single trunk. The implementation owns timeout enforcement —
    /// the caller passes `timeout` as a hint, the prober is expected to
    /// bound its own work within it.
    async fn probe(&self, trunk: &TrunkModel, timeout: Duration) -> ProbeOutcome;
}

pub type ProberHandle = Arc<dyn KindHealthProber>;

static REGISTRY: OnceLock<RwLock<HashMap<String, ProberHandle>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, ProberHandle>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register (or replace) the prober for `kind`. Idempotent.
pub fn register(kind: impl Into<String>, prober: ProberHandle) {
    let kind = kind.into();
    let mut guard = registry()
        .write()
        .expect("health_probers registry RwLock poisoned");
    guard.insert(kind, prober);
}

/// Look up the prober for `kind`. Returns `None` when no prober is
/// registered, which the monitor uses to skip that trunk.
pub fn lookup(kind: &str) -> Option<ProberHandle> {
    let guard = registry()
        .read()
        .expect("health_probers registry RwLock poisoned");
    guard.get(kind).cloned()
}

/// Snapshot of all currently-registered kind names. Order is unspecified.
pub fn registered_kinds() -> Vec<String> {
    let guard = registry()
        .read()
        .expect("health_probers registry RwLock poisoned");
    guard.keys().cloned().collect()
}

/// SIP prober: delegates to the existing OPTIONS-keepalive probe. Stateful
/// (carries an `EndpointInnerRef`); constructed once at app boot and
/// registered.
pub struct SipProber {
    endpoint: Option<rsipstack::transaction::endpoint::EndpointInnerRef>,
}

impl SipProber {
    pub fn new(endpoint: Option<rsipstack::transaction::endpoint::EndpointInnerRef>) -> Self {
        Self { endpoint }
    }
}

#[async_trait]
impl KindHealthProber for SipProber {
    async fn probe(&self, trunk: &TrunkModel, timeout: Duration) -> ProbeOutcome {
        match &self.endpoint {
            Some(ep) => crate::proxy::gateway_health::probe_trunk(ep, trunk, timeout).await,
            None => ProbeOutcome {
                ok: false,
                latency_ms: 0,
                detail: "no endpoint".into(),
            },
        }
    }
}

/// WebRTC prober for the `http_json` signaling adapter (currently the only
/// registered adapter, per `crate::proxy::bridge::signaling`). Sends a
/// plain HTTP `GET` to the endpoint URL extracted from `kind_config`.
/// `GET` (not `HEAD`) is used because most health endpoints in the wild
/// — Kubernetes liveness, load-balancer probes, FastAPI `@app.get` — only
/// declare GET; HEAD often returns 405. Health bodies are tiny so the
/// extra bytes don't matter.
///
/// We deliberately avoid:
/// - A canary `POST` of a synthetic SDP — that would consume a real
///   session slot on the bot for every probe (expensive, side-effects).
/// - Parsing the full `WebRtcTrunkConfig` schema and reading optional
///   `Authorization` headers — keeps the prober simple and unaffected by
///   any future schema drift. If the bot lives behind auth, the operator
///   should mark the trunk's status manually or front-end-cache it.
pub struct WebRtcHttpJsonProber {
    client: reqwest::Client,
}

impl WebRtcHttpJsonProber {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .build()
                .expect("reqwest::Client::builder should not fail with defaults"),
        }
    }
}

impl Default for WebRtcHttpJsonProber {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl KindHealthProber for WebRtcHttpJsonProber {
    async fn probe(&self, trunk: &TrunkModel, timeout: Duration) -> ProbeOutcome {
        let start = std::time::Instant::now();

        // Pick the URL to probe. Precedence:
        //   1. `kind_config.health_check_url` if present (operator
        //      override — point at /healthz or / when the signaling
        //      endpoint doesn't implement GET).
        //   2. `kind_config.endpoint_url` (default — same URL the
        //      signaling adapter POSTs to for real offers).
        // Read both defensively (raw JSON, not typed view) so future
        // schema drift won't break probing.
        let url_opt = trunk
            .kind_config
            .get("health_check_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                trunk
                    .kind_config
                    .get("endpoint_url")
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string());
        let Some(url) = url_opt else {
            return ProbeOutcome {
                ok: false,
                latency_ms: 0,
                detail: "kind_config has neither health_check_url nor endpoint_url (or both empty)".into(),
            };
        };

        let result = self
            .client
            .get(&url)
            .timeout(timeout)
            .send()
            .await;

        let latency_ms = start.elapsed().as_millis() as u64;
        match result {
            Ok(resp) => {
                let status = resp.status();
                let detail = format!(
                    "GET {} {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("")
                );
                ProbeOutcome {
                    ok: status.is_success() || status.is_redirection(),
                    latency_ms,
                    detail,
                }
            }
            Err(e) if e.is_timeout() => ProbeOutcome {
                ok: false,
                latency_ms,
                detail: "timeout".into(),
            },
            Err(e) => ProbeOutcome {
                ok: false,
                latency_ms,
                detail: format!("http error: {e}"),
            },
        }
    }
}

/// LiveKit prober. Two probe strategies, in precedence order:
///
/// 1. If `kind_config.health_check_url` is set, send an HTTP `GET` to it
///    and treat any 2xx/3xx as healthy. This is the same shape as the
///    WebRTC prober and lets operators point at an `/healthz` endpoint on
///    a sidecar.
/// 2. Otherwise, parse `kind_config.server_url` (`ws://` or `wss://`) into
///    host:port and run a TCP connect with the configured
///    `signaling_timeout_ms` (defaulting to 5s). A successful connect
///    counts as healthy. This is cheap and avoids consuming a LiveKit
///    session slot for every probe; it doesn't validate that LiveKit
///    itself is responding, only that the signaling TCP socket is open.
pub struct LiveKitProber {
    client: reqwest::Client,
}

impl LiveKitProber {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .build()
                .expect("reqwest::Client::builder should not fail with defaults"),
        }
    }
}

impl Default for LiveKitProber {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a `ws://host[:port]/...` or `wss://host[:port]/...` URL into
/// `host:port`, defaulting the port to 80 (ws) or 443 (wss) when omitted.
/// Returns `Err(detail)` on a malformed URL.
fn ws_url_to_host_port(url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid server_url: {e}"))?;
    let scheme = parsed.scheme();
    let default_port = match scheme {
        "ws" => 80u16,
        "wss" => 443u16,
        other => return Err(format!("server_url scheme '{other}' is not ws/wss")),
    };
    let host = parsed
        .host_str()
        .ok_or_else(|| "server_url has no host".to_string())?;
    let port = parsed.port().unwrap_or(default_port);
    Ok(format!("{host}:{port}"))
}

#[async_trait]
impl KindHealthProber for LiveKitProber {
    async fn probe(&self, trunk: &TrunkModel, timeout: Duration) -> ProbeOutcome {
        let start = std::time::Instant::now();

        // Precedence 1: explicit health_check_url → HTTP GET.
        if let Some(url) = trunk
            .kind_config
            .get("health_check_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let result = self.client.get(url).timeout(timeout).send().await;
            let latency_ms = start.elapsed().as_millis() as u64;
            return match result {
                Ok(resp) => {
                    let status = resp.status();
                    let detail = format!(
                        "GET {} {}",
                        status.as_u16(),
                        status.canonical_reason().unwrap_or("")
                    );
                    let ok = status.is_success() || status.is_redirection();
                    ProbeOutcome { ok, latency_ms, detail }
                }
                Err(e) if e.is_timeout() => ProbeOutcome {
                    ok: false,
                    latency_ms,
                    detail: "timeout".into(),
                },
                Err(e) => ProbeOutcome {
                    ok: false,
                    latency_ms,
                    detail: format!("http error: {e}"),
                },
            };
        }

        // Precedence 2: TCP connect against server_url's host:port.
        let server_url = match trunk
            .kind_config
            .get("server_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            Some(s) => s,
            None => {
                return ProbeOutcome {
                    ok: false,
                    latency_ms: 0,
                    detail: "kind_config has neither health_check_url nor server_url".into(),
                };
            }
        };
        let host_port = match ws_url_to_host_port(server_url) {
            Ok(hp) => hp,
            Err(detail) => {
                return ProbeOutcome { ok: false, latency_ms: 0, detail };
            }
        };
        let connect_fut = tokio::net::TcpStream::connect(&host_port);
        let result = tokio::time::timeout(timeout, connect_fut).await;
        let latency_ms = start.elapsed().as_millis() as u64;
        match result {
            Ok(Ok(_stream)) => ProbeOutcome {
                ok: true,
                latency_ms,
                detail: format!("tcp connect ok ({host_port})"),
            },
            Ok(Err(e)) => ProbeOutcome {
                ok: false,
                latency_ms,
                detail: format!("tcp connect error: {e}"),
            },
            Err(_elapsed) => ProbeOutcome {
                ok: false,
                latency_ms,
                detail: "timeout".into(),
            },
        }
    }
}

/// Register the built-in probers. Call once at boot. The SIP endpoint must
/// be known by then (it's required for SIP probing).
pub fn register_builtins(
    sip_endpoint: Option<rsipstack::transaction::endpoint::EndpointInnerRef>,
) {
    register("sip", Arc::new(SipProber::new(sip_endpoint)));
    register("webrtc", Arc::new(WebRtcHttpJsonProber::new()));
    register("livekit", Arc::new(LiveKitProber::new()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_trunk(kind: &str, kind_config: serde_json::Value) -> TrunkModel {
        TrunkModel {
            id: 1,
            name: "t".into(),
            kind: kind.into(),
            kind_config,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn webrtc_prober_reports_missing_endpoint_url() {
        let p = WebRtcHttpJsonProber::new();
        let t = make_trunk("webrtc", json!({}));
        let out = p.probe(&t, Duration::from_millis(500)).await;
        assert!(!out.ok);
        assert!(out.detail.contains("endpoint_url"));
    }

    #[tokio::test]
    async fn webrtc_prober_treats_non_2xx_as_failure() {
        // 0.0.0.0:1 is a deliberately bogus address; expect a connect-time
        // error well within the timeout.
        let p = WebRtcHttpJsonProber::new();
        let t = make_trunk("webrtc", json!({"endpoint_url": "http://0.0.0.0:1/health"}));
        let out = p.probe(&t, Duration::from_millis(300)).await;
        assert!(!out.ok);
    }

    #[tokio::test]
    async fn webrtc_prober_prefers_health_check_url_over_endpoint_url() {
        // Stand up a tiny HTTP server that returns 200 on /ok and 404 on
        // every other path. Point endpoint_url at the 404 path and
        // health_check_url at the 200 path: a probe must report ok=true,
        // proving the override took precedence over endpoint_url.
        use std::net::SocketAddr;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 256];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let body = if req.contains("/ok") {
                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    } else {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    };
                    let _ = sock.write_all(body.as_bytes()).await;
                });
            }
        });

        let p = WebRtcHttpJsonProber::new();
        let kind_config = json!({
            "endpoint_url":     format!("http://{}/offer", addr),
            "health_check_url": format!("http://{}/ok",    addr),
        });
        let t = make_trunk("webrtc", kind_config);
        let out = p.probe(&t, Duration::from_millis(2_000)).await;
        assert!(out.ok, "expected ok=true via health_check_url override, got {:?}", out);
        assert!(out.detail.contains("200"));
    }

    #[test]
    fn register_and_lookup_roundtrips() {
        register_builtins(None);
        assert!(lookup("sip").is_some());
        assert!(lookup("webrtc").is_some());
        assert!(lookup("livekit").is_some());
        assert!(lookup("nonexistent_kind").is_none());
    }

    #[tokio::test]
    async fn livekit_prober_reports_missing_urls() {
        let p = LiveKitProber::new();
        let t = make_trunk("livekit", json!({}));
        let out = p.probe(&t, Duration::from_millis(500)).await;
        assert!(!out.ok);
        assert!(out.detail.contains("server_url"));
    }

    #[tokio::test]
    async fn livekit_prober_rejects_non_ws_scheme() {
        let p = LiveKitProber::new();
        let t = make_trunk("livekit", json!({"server_url": "http://example.com"}));
        let out = p.probe(&t, Duration::from_millis(500)).await;
        assert!(!out.ok);
        assert!(out.detail.contains("ws/wss"));
    }

    #[tokio::test]
    async fn livekit_prober_tcp_connect_to_bogus_addr_fails() {
        // 0.0.0.0:1 — connect should fail well within the timeout.
        let p = LiveKitProber::new();
        let t = make_trunk("livekit", json!({"server_url": "ws://0.0.0.0:1"}));
        let out = p.probe(&t, Duration::from_millis(500)).await;
        assert!(!out.ok);
    }
}
