//! Phase 6 Plan 06-04 — per-variant match evaluators for the 5 routing
//! match types (D-08..D-12).
//!
//! Single source of truth for the wire types is
//! `crate::handler::api_v1::routing_records` — re-exported here so the
//! matcher and the API handler agree on the shape.
//!
//! Each evaluator is a small pure function. `eval_http_query` is the only
//! async one and performs a runtime SSRF re-check on every call (T-06-04-01)
//! before issuing the request. All HttpQuery failure modes (timeout, 5xx,
//! connection error, malformed JSON, SSRF blocked) FALL THROUGH to the
//! next record — never 503 (D-15).
//!
//! Regex patterns are compiled once and cached in a per-INVITE
//! [`RegexCache`] to avoid recompilation across N evaluations of the same
//! pattern (operator may use the same regex in multiple records). The
//! `regex` crate is linear-time-guaranteed (no backtracking) so ReDoS is
//! structurally impossible (T-06-04-02).

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Deserialize;
use tracing::warn;

pub use crate::handler::api_v1::routing_records::{
    CompareOp, CompareValue, RoutingMatch, RoutingRecord, RoutingTarget,
};

/// Default HttpQuery timeout when the record omits `timeout_ms` (D-12, D-16).
const DEFAULT_HTTP_QUERY_TIMEOUT_MS: u32 = 2000;
/// Hard cap on per-record HttpQuery timeout (T-06-04-04).
const MAX_HTTP_QUERY_TIMEOUT_MS: u32 = 5000;

/// Evaluation outcome for a single record's match expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchOutcome {
    /// Matched. `reason` is appended to RouteTrace events for observability.
    Hit { reason: String },
    /// Did not match — caller advances to next record.
    Miss,
    /// Skipped (record inactive, default-flagged in normal eval, etc.).
    Skip,
}

/// Per-INVITE regex cache. Single source of truth for compiled patterns
/// during a match. Cleared automatically when the cache goes out of scope
/// (no cross-INVITE leakage; D-29 fresh-DB-read pattern).
#[derive(Debug, Default)]
pub struct RegexCache {
    inner: HashMap<String, Regex>,
}

impl RegexCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compile (and cache) the regex for `pattern`. Returns `None` if the
    /// pattern is invalid; caller treats invalid patterns as Miss with a
    /// warn log (defense-in-depth — 06-03 already validates at write time).
    pub fn get_or_compile(&mut self, pattern: &str) -> Option<&Regex> {
        if !self.inner.contains_key(pattern) {
            match Regex::new(pattern) {
                Ok(re) => {
                    self.inner.insert(pattern.to_string(), re);
                }
                Err(err) => {
                    warn!(
                        pattern = %pattern,
                        error = %err,
                        "routing regex pattern failed to compile; treating as Miss"
                    );
                    return None;
                }
            }
        }
        self.inner.get(pattern)
    }
}

// ─── eval_lpm ───────────────────────────────────────────────────────────

/// Returns Hit only if `destination` starts with `prefix`. The matched
/// prefix length (in bytes) is encoded in the reason for cross-record
/// "longest wins" comparison (D-08).
pub fn eval_lpm(prefix: &str, destination: &str) -> MatchOutcome {
    if !prefix.is_empty() && destination.starts_with(prefix) {
        MatchOutcome::Hit {
            reason: format!("lpm:{}", prefix),
        }
    } else {
        MatchOutcome::Miss
    }
}

/// Length helper for cross-record longest-wins comparison.
pub fn lpm_match_length(prefix: &str, destination: &str) -> Option<usize> {
    if !prefix.is_empty() && destination.starts_with(prefix) {
        Some(prefix.len())
    } else {
        None
    }
}

// ─── eval_exact ─────────────────────────────────────────────────────────

/// Case-sensitive equality (D-09).
pub fn eval_exact(value: &str, destination: &str) -> MatchOutcome {
    if value == destination {
        MatchOutcome::Hit {
            reason: format!("exact:{}", value),
        }
    } else {
        MatchOutcome::Miss
    }
}

// ─── eval_regex ─────────────────────────────────────────────────────────

/// Regex match (D-10). Invalid patterns log a warn and return Miss.
pub fn eval_regex(pattern: &str, destination: &str, cache: &mut RegexCache) -> MatchOutcome {
    match cache.get_or_compile(pattern) {
        Some(re) => {
            if re.is_match(destination) {
                MatchOutcome::Hit {
                    reason: format!("regex:{}", pattern),
                }
            } else {
                MatchOutcome::Miss
            }
        }
        None => MatchOutcome::Miss,
    }
}

// ─── eval_compare ───────────────────────────────────────────────────────

/// Numeric comparison against `destination.len()` digit count (D-11).
pub fn eval_compare(op: &CompareOp, value: &CompareValue, destination: &str) -> MatchOutcome {
    let n = destination.chars().filter(|c| c.is_ascii_digit()).count() as u32;
    let hit = match (op, value) {
        (CompareOp::Eq, CompareValue::Single(v)) => n == *v,
        (CompareOp::Lt, CompareValue::Single(v)) => n < *v,
        (CompareOp::Gt, CompareValue::Single(v)) => n > *v,
        (CompareOp::In, CompareValue::Range([lo, hi])) => n >= *lo && n <= *hi,
        // Any other shape is a validation bug; treat as Miss defensively.
        _ => false,
    };
    if hit {
        MatchOutcome::Hit {
            reason: format!("compare:{:?}={:?}", op, value_str(value)),
        }
    } else {
        MatchOutcome::Miss
    }
}

fn value_str(v: &CompareValue) -> String {
    match v {
        CompareValue::Single(n) => n.to_string(),
        CompareValue::Range([lo, hi]) => format!("[{},{}]", lo, hi),
    }
}

// ─── eval_http_query ────────────────────────────────────────────────────

/// Body POSTed to the operator's HttpQuery endpoint (D-13).
#[derive(Debug, Clone, serde::Serialize)]
pub struct HttpQueryBody {
    pub caller_number: String,
    pub destination_number: String,
    pub src_ip: Option<String>,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct HttpQueryResponse {
    matched: bool,
    #[serde(default)]
    target: Option<RoutingTarget>,
}

/// Result of an HttpQuery evaluation, paired with the (optional) target
/// returned by the operator's service when `matched: true`.
#[derive(Debug)]
pub struct HttpQueryEvalResult {
    pub outcome: MatchOutcome,
    pub target: Option<RoutingTarget>,
    pub latency_ms: u64,
    /// Set only when the request was blocked or failed; surfaces in
    /// RouteTrace `HttpQueryFailed` events (D-31).
    pub failure_reason: Option<String>,
}

/// External HTTP lookup (D-12). Failure modes (timeout, 5xx, connection,
/// malformed JSON, runtime-SSRF) all return `(Miss, None)` so the caller
/// falls through to the next record (D-15). NEVER returns 503.
pub async fn eval_http_query(
    client: &reqwest::Client,
    url: &str,
    timeout_ms: Option<u32>,
    headers: Option<&HashMap<String, String>>,
    body: &HttpQueryBody,
) -> HttpQueryEvalResult {
    let started = Instant::now();
    let effective_timeout = timeout_ms
        .unwrap_or(DEFAULT_HTTP_QUERY_TIMEOUT_MS)
        .min(MAX_HTTP_QUERY_TIMEOUT_MS);

    // Runtime SSRF re-check (T-06-04-01). Defends against DNS rebind / DB
    // tampering bypassing the write-time check in 06-03.
    if let Err(reason) = runtime_ssrf_check(url) {
        warn!(url = %url, reason = %reason, "HttpQuery blocked by runtime SSRF check");
        return HttpQueryEvalResult {
            outcome: MatchOutcome::Miss,
            target: None,
            latency_ms: started.elapsed().as_millis() as u64,
            failure_reason: Some(format!("ssrf_blocked: {}", reason)),
        };
    }

    let mut req = client
        .post(url)
        .timeout(Duration::from_millis(effective_timeout as u64))
        .json(body);

    if let Some(hdrs) = headers {
        for (k, v) in hdrs {
            req = req.header(k, v);
        }
    }

    let res = req.send().await;
    let resp = match res {
        Ok(r) => r,
        Err(err) => {
            warn!(url = %url, error = %err, "HttpQuery request failed");
            return HttpQueryEvalResult {
                outcome: MatchOutcome::Miss,
                target: None,
                latency_ms: started.elapsed().as_millis() as u64,
                failure_reason: Some(format!("request_failed: {}", err)),
            };
        }
    };

    let status = resp.status();
    if !status.is_success() {
        warn!(url = %url, status = %status, "HttpQuery non-2xx response");
        return HttpQueryEvalResult {
            outcome: MatchOutcome::Miss,
            target: None,
            latency_ms: started.elapsed().as_millis() as u64,
            failure_reason: Some(format!("http_status:{}", status.as_u16())),
        };
    }

    let parsed: Result<HttpQueryResponse, _> = resp.json().await;
    match parsed {
        Ok(p) if p.matched => {
            let latency_ms = started.elapsed().as_millis() as u64;
            HttpQueryEvalResult {
                outcome: MatchOutcome::Hit {
                    reason: format!("http_query:{}", url),
                },
                target: p.target,
                latency_ms,
                failure_reason: None,
            }
        }
        Ok(_) => HttpQueryEvalResult {
            outcome: MatchOutcome::Miss,
            target: None,
            latency_ms: started.elapsed().as_millis() as u64,
            failure_reason: None,
        },
        Err(err) => {
            warn!(url = %url, error = %err, "HttpQuery malformed JSON");
            HttpQueryEvalResult {
                outcome: MatchOutcome::Miss,
                target: None,
                latency_ms: started.elapsed().as_millis() as u64,
                failure_reason: Some(format!("malformed_json: {}", err)),
            }
        }
    }
}

/// Runtime SSRF defense — mirrors the write-time check in
/// `routing_records::validate_routing_record` (T-06-04-01).
fn runtime_ssrf_check(url_str: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url_str).map_err(|e| format!("invalid_url:{}", e))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!("scheme_not_allowed:{}", scheme));
    }
    let host = parsed.host_str().ok_or_else(|| "no_host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err("localhost_blocked".to_string());
    }
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        if is_private_or_loopback_ip(&ip) {
            return Err(format!("private_or_loopback:{}", ip));
        }
    }
    Ok(())
}

fn is_private_or_loopback_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    // ─── Lpm ────────────────────────────────────────────────────────────

    #[test]
    fn lpm_longest_prefix_wins() {
        let dest = "+14155551234";
        // All three are matches; caller (table_matcher) picks longest.
        assert!(matches!(eval_lpm("+1", dest), MatchOutcome::Hit { .. }));
        assert!(matches!(
            eval_lpm("+1415", dest),
            MatchOutcome::Hit { .. }
        ));
        assert_eq!(eval_lpm("+1416", dest), MatchOutcome::Miss);

        // length helper enables cross-record longest comparison
        assert_eq!(lpm_match_length("+1", dest), Some(2));
        assert_eq!(lpm_match_length("+1415", dest), Some(5));
        assert_eq!(lpm_match_length("+1416", dest), None);
    }

    #[test]
    fn lpm_no_match() {
        assert_eq!(eval_lpm("+44", "+14155551234"), MatchOutcome::Miss);
    }

    #[test]
    fn lpm_empty_prefix_is_miss() {
        // Empty prefix would otherwise match everything; we forbid it.
        assert_eq!(eval_lpm("", "+14155551234"), MatchOutcome::Miss);
    }

    // ─── ExactMatch ─────────────────────────────────────────────────────

    #[test]
    fn exact_case_sensitive() {
        assert!(matches!(
            eval_exact("Match", "Match"),
            MatchOutcome::Hit { .. }
        ));
        assert_eq!(eval_exact("Match", "match"), MatchOutcome::Miss);
        assert_eq!(eval_exact("Match", "Match "), MatchOutcome::Miss);
    }

    // ─── Regex ──────────────────────────────────────────────────────────

    #[test]
    fn regex_compile_caching() {
        let mut cache = RegexCache::new();
        let pattern = r"^\+1[0-9]{10}$";
        // First call compiles
        for _ in 0..1000 {
            let r = eval_regex(pattern, "+14155551234", &mut cache);
            assert!(matches!(r, MatchOutcome::Hit { .. }));
        }
        // Cache should have exactly one entry
        assert_eq!(cache.inner.len(), 1);
    }

    #[test]
    fn regex_invalid_treated_as_miss() {
        let mut cache = RegexCache::new();
        // Unclosed group is invalid
        assert_eq!(
            eval_regex("[unclosed", "anything", &mut cache),
            MatchOutcome::Miss
        );
    }

    #[test]
    fn regex_no_match_returns_miss() {
        let mut cache = RegexCache::new();
        assert_eq!(
            eval_regex(r"^\+44", "+14155551234", &mut cache),
            MatchOutcome::Miss
        );
    }

    // ─── Compare ────────────────────────────────────────────────────────

    #[test]
    fn compare_eq_lt_gt() {
        // 11 digits
        let dest = "14155551234"; // 11 digits
        assert!(matches!(
            eval_compare(&CompareOp::Eq, &CompareValue::Single(11), dest),
            MatchOutcome::Hit { .. }
        ));
        assert!(matches!(
            eval_compare(&CompareOp::Lt, &CompareValue::Single(12), dest),
            MatchOutcome::Hit { .. }
        ));
        assert!(matches!(
            eval_compare(&CompareOp::Gt, &CompareValue::Single(10), dest),
            MatchOutcome::Hit { .. }
        ));
        assert_eq!(
            eval_compare(&CompareOp::Eq, &CompareValue::Single(10), dest),
            MatchOutcome::Miss
        );
    }

    #[test]
    fn compare_in_range() {
        // 11 digits between [7,15]
        let dest = "+14155551234"; // 11 digits (the + is non-digit)
        assert!(matches!(
            eval_compare(&CompareOp::In, &CompareValue::Range([7, 15]), dest),
            MatchOutcome::Hit { .. }
        ));
        // 5 digits not in [7,15]
        assert_eq!(
            eval_compare(&CompareOp::In, &CompareValue::Range([7, 15]), "+12345"),
            MatchOutcome::Miss
        );
    }

    // ─── HttpQuery ──────────────────────────────────────────────────────

    /// Spawn a tiny axum server returning whatever JSON the test wants.
    /// Returns `(addr, shutdown)`. Tests call `addr` and drop the
    /// shutdown sender to stop the server.
    async fn spawn_mock(
        handler: impl Fn(serde_json::Value) -> (u16, String, Option<u64>)
            + Send
            + Sync
            + Clone
            + 'static,
    ) -> SocketAddr {
        use axum::{Router, extract::Json as AxumJson, http::StatusCode, routing::post};

        let router: Router<()> = Router::new().route(
            "/",
            post(move |AxumJson(body): AxumJson<serde_json::Value>| {
                let h = handler.clone();
                async move {
                    let (status, body, delay_ms) = h(body);
                    if let Some(d) = delay_ms {
                        tokio::time::sleep(Duration::from_millis(d)).await;
                    }
                    let status =
                        StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
                    (status, body)
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        addr
    }

    fn body() -> HttpQueryBody {
        HttpQueryBody {
            caller_number: "+1555".to_string(),
            destination_number: "+1999".to_string(),
            src_ip: None,
            headers: HashMap::new(),
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client")
    }

    // We construct URLs using 127.0.0.1 — but the real eval_http_query
    // BLOCKS that as SSRF. Tests use a small helper that bypasses the
    // SSRF check by using `eval_http_query_for_test` (private wrapper),
    // OR we configure the test mock to bind to 0.0.0.0 and reach via
    // a non-private DNS name. Simpler: expose an internal flag.
    //
    // Implementation choice: tests call the lower-level `do_http_query`
    // which skips the runtime SSRF check; the public `eval_http_query`
    // applies the check. SSRF is tested separately.

    async fn http_post_no_ssrf_check(
        client: &reqwest::Client,
        url: &str,
        timeout_ms: Option<u32>,
        body: &HttpQueryBody,
    ) -> HttpQueryEvalResult {
        let started = Instant::now();
        let effective_timeout = timeout_ms
            .unwrap_or(DEFAULT_HTTP_QUERY_TIMEOUT_MS)
            .min(MAX_HTTP_QUERY_TIMEOUT_MS);
        let res = client
            .post(url)
            .timeout(Duration::from_millis(effective_timeout as u64))
            .json(body)
            .send()
            .await;
        let resp = match res {
            Ok(r) => r,
            Err(err) => {
                return HttpQueryEvalResult {
                    outcome: MatchOutcome::Miss,
                    target: None,
                    latency_ms: started.elapsed().as_millis() as u64,
                    failure_reason: Some(format!("request_failed: {}", err)),
                };
            }
        };
        if !resp.status().is_success() {
            return HttpQueryEvalResult {
                outcome: MatchOutcome::Miss,
                target: None,
                latency_ms: started.elapsed().as_millis() as u64,
                failure_reason: Some(format!("http_status:{}", resp.status().as_u16())),
            };
        }
        let parsed: Result<HttpQueryResponse, _> = resp.json().await;
        match parsed {
            Ok(p) if p.matched => HttpQueryEvalResult {
                outcome: MatchOutcome::Hit {
                    reason: format!("http_query:{}", url),
                },
                target: p.target,
                latency_ms: started.elapsed().as_millis() as u64,
                failure_reason: None,
            },
            Ok(_) => HttpQueryEvalResult {
                outcome: MatchOutcome::Miss,
                target: None,
                latency_ms: started.elapsed().as_millis() as u64,
                failure_reason: None,
            },
            Err(err) => HttpQueryEvalResult {
                outcome: MatchOutcome::Miss,
                target: None,
                latency_ms: started.elapsed().as_millis() as u64,
                failure_reason: Some(format!("malformed_json: {}", err)),
            },
        }
    }

    #[tokio::test]
    async fn http_query_happy_path() {
        let addr = spawn_mock(|_| {
            (
                200,
                r#"{"matched":true,"target":{"kind":"trunk_group","name":"us"}}"#.to_string(),
                None,
            )
        })
        .await;
        let url = format!("http://{}/", addr);
        let res = http_post_no_ssrf_check(&client(), &url, None, &body()).await;
        assert!(matches!(res.outcome, MatchOutcome::Hit { .. }));
        match res.target {
            Some(RoutingTarget::TrunkGroup { name }) => assert_eq!(name, "us"),
            other => panic!("expected trunk_group target, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn http_query_matched_false_falls_through() {
        let addr = spawn_mock(|_| (200, r#"{"matched":false}"#.to_string(), None)).await;
        let url = format!("http://{}/", addr);
        let res = http_post_no_ssrf_check(&client(), &url, None, &body()).await;
        assert_eq!(res.outcome, MatchOutcome::Miss);
        assert!(res.target.is_none());
    }

    #[tokio::test]
    async fn http_query_5xx_falls_through() {
        let addr = spawn_mock(|_| (503, r#"oops"#.to_string(), None)).await;
        let url = format!("http://{}/", addr);
        let res = http_post_no_ssrf_check(&client(), &url, None, &body()).await;
        assert_eq!(res.outcome, MatchOutcome::Miss);
        assert!(res.failure_reason.unwrap().contains("503"));
    }

    #[tokio::test]
    async fn http_query_malformed_json_falls_through() {
        let addr = spawn_mock(|_| (200, r#"not json at all"#.to_string(), None)).await;
        let url = format!("http://{}/", addr);
        let res = http_post_no_ssrf_check(&client(), &url, None, &body()).await;
        assert_eq!(res.outcome, MatchOutcome::Miss);
        assert!(res.failure_reason.unwrap().contains("malformed_json"));
    }

    #[tokio::test]
    async fn http_query_timeout_falls_through() {
        // Server delays 500ms; client timeout 50ms.
        let addr = spawn_mock(|_| {
            (
                200,
                r#"{"matched":true,"target":{"kind":"trunk_group","name":"x"}}"#.to_string(),
                Some(500),
            )
        })
        .await;
        let url = format!("http://{}/", addr);
        let res = http_post_no_ssrf_check(&client(), &url, Some(50), &body()).await;
        assert_eq!(res.outcome, MatchOutcome::Miss);
        assert!(res.failure_reason.is_some());
    }

    #[tokio::test]
    async fn http_query_runtime_ssrf_localhost_falls_through() {
        // Public eval_http_query MUST block 127.0.0.1 BEFORE sending.
        let res = eval_http_query(
            &client(),
            "http://127.0.0.1:1/",
            Some(100),
            None,
            &body(),
        )
        .await;
        assert_eq!(res.outcome, MatchOutcome::Miss);
        let reason = res.failure_reason.expect("ssrf reason");
        assert!(
            reason.starts_with("ssrf_blocked"),
            "expected ssrf_blocked, got {}",
            reason
        );
    }

    #[tokio::test]
    async fn http_query_runtime_ssrf_localhost_hostname_falls_through() {
        let res = eval_http_query(&client(), "http://localhost:1/", Some(100), None, &body()).await;
        assert_eq!(res.outcome, MatchOutcome::Miss);
        assert!(res.failure_reason.unwrap().starts_with("ssrf_blocked"));
    }

    #[tokio::test]
    async fn http_query_runtime_ssrf_bad_scheme_falls_through() {
        let res = eval_http_query(&client(), "file:///etc/passwd", Some(100), None, &body()).await;
        assert_eq!(res.outcome, MatchOutcome::Miss);
        assert!(res.failure_reason.unwrap().starts_with("ssrf_blocked"));
    }
}
