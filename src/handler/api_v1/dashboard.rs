//! `/api/v1/dashboard` — JSON dashboard summary (metrics, call direction,
//! active calls, timeline) that mirrors the console `/console/dashboard/data`
//! payload exactly. Both endpoints call
//! `console::handlers::dashboard::fetch_dashboard_payload` so the wire
//! shape stays identical.
//!
//! `/api/v1/dashboard/summary` — a filtered call list plus a rich aggregate
//! summary (ASR, talk time, outcome breakdown, per-number breakdown). This is
//! the native port of `scripts/qa_call_records_csv.py`: same outcome
//! classification (it reuses `classify_error_reason`) and the same timeline
//! duration math (start → ring → answer → end), exposed as a JSON API.
//!
//! Available only when the `console` feature/state is enabled — the
//! payload depends on `ConsoleState` for display timezone and the SIP
//! server handle. If `state.console` is `None`, returns 503.

use std::collections::BTreeMap;

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};

// Used by the `/dashboard/summary` helpers below (console-only).
#[cfg(feature = "console")]
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
#[cfg(feature = "console")]
use chrono_tz::Tz;

#[derive(Debug, Deserialize)]
pub struct DashboardQuery {
    pub range: Option<String>,
    /// Substring match against from_number OR to_number on call records,
    /// and caller OR callee on the active-calls preview.
    pub number: Option<String>,
    /// Exact direction filter (case-insensitive): inbound | outbound | internal.
    pub direction: Option<String>,
}

/// Query for `/api/v1/dashboard/summary`. Mirrors the filters the Python
/// exporter accepted (number + date range + direction) plus prefix filters on
/// either number side. Dates accept a bare `YYYY-MM-DD` (interpreted in `tz` →
/// console display timezone → IST) or a full RFC3339 timestamp.
#[derive(Debug, Deserialize)]
pub struct DashboardSummaryQuery {
    /// Substring match against from_number OR to_number.
    #[serde(default)]
    pub number: Option<String>,
    /// Prefix match on the caller number.
    #[serde(default)]
    pub from_number: Option<String>,
    /// Prefix match on the callee number.
    #[serde(default)]
    pub to_number: Option<String>,
    /// Exact direction filter (values are stored lowercase: inbound | outbound
    /// | internal). The sentinel "any" (case-insensitive) is ignored.
    #[serde(default)]
    pub direction: Option<String>,
    /// Exact status filter (stored lowercase, e.g. completed | failed | missed).
    /// The sentinel "any" (case-insensitive) is ignored.
    #[serde(default)]
    pub status: Option<String>,
    /// Filter by final SIP response code (e.g. 486, 503).
    #[serde(default)]
    pub status_code: Option<i16>,
    /// IANA timezone name (e.g. `Asia/Kolkata`, `UTC`, `America/New_York`) used
    /// to interpret bare `YYYY-MM-DD` date filters. Overrides the console's
    /// configured display timezone for this request; falls back to that (then
    /// IST) when omitted or unparseable.
    #[serde(default)]
    pub tz: Option<String>,
    /// Single-day shortcut (`YYYY-MM-DD`): sets both `date_from` (start of day)
    /// and `date_to` (end of day) automatically, overriding either if also set.
    /// Interpreted in the console display timezone (default IST).
    #[serde(default)]
    pub date: Option<String>,
    /// Lower bound on started_at. A bare `YYYY-MM-DD` is the start of that day
    /// in the console display timezone (default IST); RFC3339 is honored as
    /// written. Defaults to the last 30 days when omitted, to bound an
    /// otherwise unbounded table scan.
    #[serde(default)]
    pub date_from: Option<String>,
    /// Upper bound on started_at. A bare `YYYY-MM-DD` is end-of-day in the
    /// console display timezone (default IST); RFC3339 is honored as written.
    #[serde(default)]
    pub date_to: Option<String>,
}

/// `{ call_list, summary }` — the full filtered list and its aggregates.
#[derive(Debug, Serialize)]
pub struct DashboardSummaryResponse {
    pub call_list: Vec<CallListItem>,
    pub summary: SummaryPayload,
}

/// One row of the call list. Timestamps are RFC3339 UTC; durations are whole
/// seconds (null when the relevant timestamps are missing).
#[derive(Debug, Serialize)]
pub struct CallListItem {
    pub started_at: String,
    pub direction: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub status: String,
    pub status_code: Option<i16>,
    /// Ringing seconds: answer − (ring or start) when answered, else end − ring.
    pub ring_secs: Option<i64>,
    /// Talk seconds: end − answer when answered, else 0.
    pub talk_secs: Option<i64>,
    /// Wall-clock seconds: end − start.
    pub total_secs: Option<i64>,
    /// Outcome key from `classify_error_reason`; null for an answered call.
    pub error_reason: Option<String>,
    pub error_reason_detail: Option<String>,
}

/// Aggregates over the entire filtered window (not just one page).
#[derive(Debug, Serialize)]
pub struct SummaryPayload {
    pub total: i64,
    pub answered: i64,
    pub failed: i64,
    /// Answer-seizure ratio (answered / total) as a percentage, 1 decimal.
    pub asr: f64,
    pub inbound: i64,
    pub outbound: i64,
    pub total_talk_minutes: f64,
    pub avg_talk_secs: f64,
    /// outcome key → count (answered calls bucketed under "answered").
    pub outcome_breakdown: BTreeMap<String, i64>,
    /// Per caller-number stats, sorted by call volume descending.
    pub per_number: Vec<PerNumberStat>,
}

#[derive(Debug, Serialize)]
pub struct PerNumberStat {
    pub number: String,
    pub total: i64,
    pub answered: i64,
    pub rang_no_answer: i64,
    pub failed: i64,
    pub asr: f64,
    pub talk_minutes: f64,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/dashboard", get(handle_dashboard))
        .route("/dashboard/summary", get(handle_dashboard_summary))
}

/// Resolve the timezone used to interpret bare `YYYY-MM-DD` date filters:
/// `tz` query param → console display timezone → Asia/Kolkata (IST).
#[cfg(feature = "console")]
fn resolve_summary_tz(tz_param: Option<&str>, console_tz: Option<String>) -> Tz {
    tz_param
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<Tz>().ok())
        .or_else(|| console_tz.and_then(|s| s.parse::<Tz>().ok()))
        .unwrap_or(chrono_tz::Asia::Kolkata)
}

/// Accept either a full RFC3339 timestamp (honored as written) or a bare
/// `YYYY-MM-DD` date pinned to the start (or end) of that day in `tz`, then
/// converted to UTC for the stored-UTC comparison.
#[cfg(feature = "console")]
fn parse_bound(raw: &Option<String>, end_of_day: bool, tz: Tz) -> Option<DateTime<Utc>> {
    let value = raw.as_ref()?.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(&value) {
        return Some(dt.with_timezone(&Utc));
    }
    let date = NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok()?;
    let naive = if end_of_day {
        date.and_hms_opt(23, 59, 59)?
    } else {
        date.and_hms_opt(0, 0, 0)?
    };
    // earliest/latest disambiguates DST folds; for IST (no DST) it's exact.
    let local = tz.from_local_datetime(&naive);
    let dt = if end_of_day {
        local.latest()
    } else {
        local.earliest()
    }?;
    Some(dt.with_timezone(&Utc))
}

/// Resolve the effective lower bound on `started_at`. Without an explicit
/// `date_from` this defaults to a 30-day window, anchored to `date_to` when
/// given (so a lone backdated `date_to` yields the 30 days ending there, not an
/// impossible range), otherwise to `now`.
#[cfg(feature = "console")]
fn resolve_effective_from(
    date_from: Option<DateTime<Utc>>,
    date_to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    date_from.unwrap_or_else(|| date_to.unwrap_or(now) - Duration::days(30))
}

/// Aggregate already-filtered call records into the `{ call_list, summary }`
/// payload. Pure (no DB or clock access), so it is unit-testable in isolation.
/// Reuses `classify_error_reason` so outcome keys match the console + CSV; the
/// timeline duration math mirrors `scripts/qa_call_records_csv.py`.
#[cfg(feature = "console")]
fn build_summary_response(rows: Vec<crate::models::call_record::Model>) -> DashboardSummaryResponse {
    use crate::console::handlers::call_record::classify_error_reason;

    // Whole seconds between two optional timestamps; None if either is missing.
    let delta = |a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>| -> Option<i64> {
        match (a, b) {
            (Some(a), Some(b)) => Some((b - a).num_seconds()),
            _ => None,
        }
    };

    // Per-number accumulator.
    struct Acc {
        total: i64,
        answered: i64,
        rang_no_answer: i64,
        talk_secs: i64,
    }

    let mut call_list = Vec::with_capacity(rows.len());
    let mut outcome_breakdown: BTreeMap<String, i64> = BTreeMap::new();
    let mut per_number: BTreeMap<String, Acc> = BTreeMap::new();
    let mut total = 0i64;
    let mut answered = 0i64;
    let mut inbound = 0i64;
    let mut outbound = 0i64;
    let mut total_talk_secs = 0i64;
    let mut answered_with_talk = 0i64;

    for m in rows {
        let start = Some(m.started_at);
        let ring = m.ring_time;
        let answer = m.answer_time;
        let end = m.ended_at;

        let total_secs = delta(start, end);
        let (ring_secs, talk_secs) = if answer.is_some() {
            (delta(ring.or(start), answer), delta(answer, end))
        } else {
            (delta(ring, end), Some(0))
        };

        // "rang" for classification parity with the console: ring OR answer OR
        // elapsed > 3s (legacy rows have NULL ring_time).
        let rang = ring.is_some()
            || answer.is_some()
            || end
                .map(|e| (e - m.started_at).num_seconds() > 3)
                .unwrap_or(false);

        let hangup_messages: Vec<serde_json::Value> = m
            .metadata
            .as_ref()
            .and_then(|md| md.get("hangup_messages"))
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();

        let reason = classify_error_reason(&m.status, m.status_code, rang, &hangup_messages);
        let error_reason = reason.as_ref().map(|r| r.key.to_string());
        let error_reason_detail = reason.and_then(|r| r.detail);
        let is_answered = error_reason.is_none();

        total += 1;
        if is_answered {
            answered += 1;
        }
        match m.direction.as_str() {
            "inbound" => inbound += 1,
            "outbound" => outbound += 1,
            _ => {}
        }
        let outcome_key = error_reason
            .clone()
            .unwrap_or_else(|| "answered".to_string());
        *outcome_breakdown.entry(outcome_key).or_insert(0) += 1;

        if is_answered {
            if let Some(t) = talk_secs {
                total_talk_secs += t;
                answered_with_talk += 1;
            }
        }

        let num_key = m
            .from_number
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(unknown)".to_string());
        let acc = per_number.entry(num_key).or_insert(Acc {
            total: 0,
            answered: 0,
            rang_no_answer: 0,
            talk_secs: 0,
        });
        acc.total += 1;
        if is_answered {
            acc.answered += 1;
            if let Some(t) = talk_secs {
                acc.talk_secs += t;
            }
        }
        if error_reason.as_deref() == Some("rang_no_answer") {
            acc.rang_no_answer += 1;
        }

        call_list.push(CallListItem {
            started_at: m.started_at.to_rfc3339(),
            direction: m.direction,
            from: m.from_number,
            to: m.to_number,
            status: m.status,
            status_code: m.status_code,
            ring_secs,
            talk_secs,
            total_secs,
            error_reason,
            error_reason_detail,
        });
    }

    let round1 = |x: f64| (x * 10.0).round() / 10.0;
    let asr = if total > 0 {
        round1(answered as f64 / total as f64 * 100.0)
    } else {
        0.0
    };
    let total_talk_minutes = round1(total_talk_secs as f64 / 60.0);
    let avg_talk_secs = if answered_with_talk > 0 {
        round1(total_talk_secs as f64 / answered_with_talk as f64)
    } else {
        0.0
    };

    let mut per_number_vec: Vec<PerNumberStat> = per_number
        .into_iter()
        .map(|(number, a)| PerNumberStat {
            number,
            total: a.total,
            answered: a.answered,
            rang_no_answer: a.rang_no_answer,
            failed: a.total - a.answered,
            asr: if a.total > 0 {
                round1(a.answered as f64 / a.total as f64 * 100.0)
            } else {
                0.0
            },
            talk_minutes: round1(a.talk_secs as f64 / 60.0),
        })
        .collect();
    per_number_vec.sort_by(|x, y| y.total.cmp(&x.total));

    let summary = SummaryPayload {
        total,
        answered,
        failed: total - answered,
        asr,
        inbound,
        outbound,
        total_talk_minutes,
        avg_talk_secs,
        outcome_breakdown,
        per_number: per_number_vec,
    };

    DashboardSummaryResponse { call_list, summary }
}

#[cfg(feature = "console")]
async fn handle_dashboard(
    State(state): State<AppState>,
    Query(query): Query<DashboardQuery>,
) -> ApiResult<Json<crate::console::handlers::dashboard::DashboardPayload>> {
    let console = state
        .console
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("console state not initialized"))?;
    let filters = crate::console::handlers::dashboard::DashboardFilters {
        number: query.number,
        direction: query.direction,
    };
    let payload = crate::console::handlers::dashboard::fetch_dashboard_payload(
        console,
        query.range.as_deref(),
        filters,
    )
    .await;
    Ok(Json(payload))
}

#[cfg(not(feature = "console"))]
async fn handle_dashboard(
    State(_state): State<AppState>,
    Query(_query): Query<DashboardQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    Err(ApiError::unavailable(
        "dashboard requires the `console` feature",
    ))
}

#[cfg(feature = "console")]
async fn handle_dashboard_summary(
    State(state): State<AppState>,
    Query(query): Query<DashboardSummaryQuery>,
) -> ApiResult<Json<DashboardSummaryResponse>> {
    use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder};

    use crate::models::call_record::{Column, Entity};

    let db = state.db();

    // Bare `YYYY-MM-DD` dates are interpreted in a timezone resolved by
    // precedence: the `tz` query param → the console display timezone →
    // Asia/Kolkata (IST). RFC3339 inputs carry their own offset.
    let display_tz = resolve_summary_tz(
        query.tz.as_deref(),
        state.console.as_ref().map(|c| c.display_timezone()),
    );

    // `date` is a single-day shortcut: it feeds both bounds (start- and
    // end-of-day), overriding date_from/date_to when present.
    let date_from_raw = query.date.clone().or_else(|| query.date_from.clone());
    let date_to_raw = query.date.clone().or_else(|| query.date_to.clone());
    let date_from = parse_bound(&date_from_raw, false, display_tz);
    let date_to = parse_bound(&date_to_raw, true, display_tz);

    let mut conds = Condition::all();
    if let Some(v) = query
        .direction
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("any"))
    {
        conds = conds.add(Column::Direction.eq(v));
    }
    if let Some(v) = query
        .status
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("any"))
    {
        conds = conds.add(Column::Status.eq(v));
    }
    if let Some(v) = query.status_code {
        conds = conds.add(Column::StatusCode.eq(v));
    }
    if let Some(v) = query
        .from_number
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        conds = conds.add(Column::FromNumber.like(format!("{}%", v)));
    }
    if let Some(v) = query
        .to_number
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        conds = conds.add(Column::ToNumber.like(format!("{}%", v)));
    }
    if let Some(v) = query
        .number
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let pat = format!("%{}%", v);
        conds = conds.add(
            Condition::any()
                .add(Column::FromNumber.like(pat.clone()))
                .add(Column::ToNumber.like(pat)),
        );
    }
    // Bound the scan: without a lower bound this would full-scan the table.
    let effective_from = resolve_effective_from(date_from, date_to, Utc::now());
    conds = conds.add(Column::StartedAt.gte(effective_from));
    if let Some(to) = date_to {
        conds = conds.add(Column::StartedAt.lte(to));
    }

    let rows = Entity::find()
        .filter(conds)
        .order_by_desc(Column::StartedAt)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(build_summary_response(rows)))
}

#[cfg(not(feature = "console"))]
async fn handle_dashboard_summary(
    State(_state): State<AppState>,
    Query(_query): Query<DashboardSummaryQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    Err(ApiError::unavailable(
        "dashboard summary requires the `console` feature",
    ))
}

#[cfg(all(test, feature = "console"))]
mod tests {
    use super::{
        build_summary_response, parse_bound, resolve_effective_from, resolve_summary_tz,
    };
    use crate::models::call_record::Model;
    use chrono::{DateTime, Duration, TimeZone, Utc};

    fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    /// A minimal answered-call row; tests override only the fields they exercise.
    fn base_row() -> Model {
        let t = ts(2026, 6, 28, 10, 0, 0);
        Model {
            id: 0,
            call_id: "c".to_string(),
            display_id: None,
            direction: "outbound".to_string(),
            status: "completed".to_string(),
            status_code: Some(200),
            hangup_reason: None,
            failure_source: None,
            outcome_kind: Some("OK".to_string()),
            q850_cause: None,
            q850_text: None,
            sipflow_available: false,
            started_at: t,
            ended_at: None,
            answer_time: None,
            ring_time: None,
            duration_secs: 0,
            billable_duration_secs: None,
            from_number: None,
            to_number: None,
            caller_name: None,
            agent_name: None,
            queue: None,
            department_id: None,
            extension_id: None,
            sip_trunk_id: None,
            route_id: None,
            sip_gateway: None,
            rewrite_original_from: None,
            rewrite_original_to: None,
            caller_uri: None,
            callee_uri: None,
            recording_url: None,
            recording_duration_secs: None,
            has_transcript: false,
            transcript_status: "none".to_string(),
            transcript_language: None,
            tags: None,
            leg_timeline: None,
            metadata: None,
            created_at: t,
            updated_at: t,
            archived_at: None,
        }
    }

    // ---- resolve_effective_from ----

    #[test]
    fn effective_from_uses_explicit_date_from() {
        let from = ts(2026, 1, 1, 0, 0, 0);
        let to = ts(2026, 2, 1, 0, 0, 0);
        let now = ts(2026, 6, 28, 0, 0, 0);
        assert_eq!(resolve_effective_from(Some(from), Some(to), now), from);
    }

    #[test]
    fn effective_from_anchors_to_date_to_when_no_from() {
        // The bug-fix case: a lone backdated date_to must anchor the 30-day
        // window to date_to, not to now (which would be an impossible range).
        let to = ts(2025, 1, 1, 23, 59, 59);
        let now = ts(2026, 6, 28, 0, 0, 0);
        assert_eq!(
            resolve_effective_from(None, Some(to), now),
            to - Duration::days(30)
        );
    }

    #[test]
    fn effective_from_defaults_to_now_minus_30d() {
        let now = ts(2026, 6, 28, 0, 0, 0);
        assert_eq!(
            resolve_effective_from(None, None, now),
            now - Duration::days(30)
        );
    }

    // ---- parse_bound ----

    #[test]
    fn parse_bound_bare_date_in_ist() {
        let ist = chrono_tz::Asia::Kolkata;
        // start of 2026-06-24 IST = 2026-06-23T18:30:00Z
        let start = parse_bound(&Some("2026-06-24".to_string()), false, ist).unwrap();
        assert_eq!(start, ts(2026, 6, 23, 18, 30, 0));
        // end of 2026-06-24 IST = 2026-06-24T18:29:59Z
        let end = parse_bound(&Some("2026-06-24".to_string()), true, ist).unwrap();
        assert_eq!(end, ts(2026, 6, 24, 18, 29, 59));
    }

    #[test]
    fn parse_bound_bare_date_in_utc() {
        let start = parse_bound(&Some("2026-06-24".to_string()), false, chrono_tz::UTC).unwrap();
        assert_eq!(start, ts(2026, 6, 24, 0, 0, 0));
    }

    #[test]
    fn parse_bound_rfc3339_honored_as_written() {
        // explicit +05:30 offset → converted to UTC; the tz arg is irrelevant.
        let v = parse_bound(
            &Some("2026-06-24T00:00:00+05:30".to_string()),
            false,
            chrono_tz::Asia::Kolkata,
        )
        .unwrap();
        assert_eq!(v, ts(2026, 6, 23, 18, 30, 0));
    }

    #[test]
    fn parse_bound_none_and_invalid() {
        let ist = chrono_tz::Asia::Kolkata;
        assert!(parse_bound(&None, false, ist).is_none());
        assert!(parse_bound(&Some("".to_string()), false, ist).is_none());
        assert!(parse_bound(&Some("not-a-date".to_string()), false, ist).is_none());
    }

    // ---- resolve_summary_tz ----

    #[test]
    fn summary_tz_precedence() {
        // query param wins over console config
        assert_eq!(
            resolve_summary_tz(Some("UTC"), Some("Asia/Kolkata".into())),
            chrono_tz::UTC
        );
        // falls back to console tz when no query param
        assert_eq!(resolve_summary_tz(None, Some("UTC".into())), chrono_tz::UTC);
        // final default is IST when nothing usable is provided
        assert_eq!(resolve_summary_tz(None, None), chrono_tz::Asia::Kolkata);
        assert_eq!(resolve_summary_tz(Some("  "), None), chrono_tz::Asia::Kolkata);
        assert_eq!(
            resolve_summary_tz(Some("Not/AZone"), None),
            chrono_tz::Asia::Kolkata
        );
    }

    // ---- build_summary_response ----

    #[test]
    fn summary_answered_call_durations_and_aggregates() {
        let mut r = base_row();
        r.from_number = Some("+91111".to_string());
        r.started_at = ts(2026, 6, 28, 10, 0, 0);
        r.ring_time = Some(ts(2026, 6, 28, 10, 0, 2));
        r.answer_time = Some(ts(2026, 6, 28, 10, 0, 5));
        r.ended_at = Some(ts(2026, 6, 28, 10, 1, 5)); // +65s from start, +60s from answer

        let resp = build_summary_response(vec![r]);

        assert_eq!(resp.call_list.len(), 1);
        let item = &resp.call_list[0];
        assert_eq!(item.total_secs, Some(65));
        assert_eq!(item.ring_secs, Some(3)); // answer - ring
        assert_eq!(item.talk_secs, Some(60)); // end - answer
        assert_eq!(item.error_reason, None);

        let s = &resp.summary;
        assert_eq!(s.total, 1);
        assert_eq!(s.answered, 1);
        assert_eq!(s.failed, 0);
        assert_eq!(s.asr, 100.0);
        assert_eq!(s.outbound, 1);
        assert_eq!(s.inbound, 0);
        assert_eq!(s.total_talk_minutes, 1.0);
        assert_eq!(s.avg_talk_secs, 60.0);
        assert_eq!(s.outcome_breakdown.get("answered"), Some(&1));
        assert_eq!(s.per_number.len(), 1);
        let p = &s.per_number[0];
        assert_eq!(p.number, "+91111");
        assert_eq!(p.answered, 1);
        assert_eq!(p.talk_minutes, 1.0);
        assert_eq!(p.asr, 100.0);
    }

    #[test]
    fn summary_failed_calls_classified_and_counted() {
        // 486 busy, no ring (1s elapsed, not > 3 → not "rang")
        let mut busy = base_row();
        busy.from_number = Some("+92".to_string());
        busy.status = "failed".to_string();
        busy.status_code = Some(486);
        busy.started_at = ts(2026, 6, 28, 10, 0, 0);
        busy.ended_at = Some(ts(2026, 6, 28, 10, 0, 1));

        // 480 with a ring → "rang_no_answer"
        let mut rang = base_row();
        rang.from_number = Some("+93".to_string());
        rang.status = "missed".to_string();
        rang.status_code = Some(480);
        rang.started_at = ts(2026, 6, 28, 11, 0, 0);
        rang.ring_time = Some(ts(2026, 6, 28, 11, 0, 2));
        rang.ended_at = Some(ts(2026, 6, 28, 11, 0, 10));

        let resp = build_summary_response(vec![busy, rang]);
        let s = &resp.summary;
        assert_eq!(s.total, 2);
        assert_eq!(s.answered, 0);
        assert_eq!(s.failed, 2);
        assert_eq!(s.asr, 0.0);
        assert_eq!(s.total_talk_minutes, 0.0);
        assert_eq!(s.avg_talk_secs, 0.0);
        assert_eq!(s.outcome_breakdown.get("busy"), Some(&1));
        assert_eq!(s.outcome_breakdown.get("rang_no_answer"), Some(&1));

        let p93 = s.per_number.iter().find(|p| p.number == "+93").unwrap();
        assert_eq!(p93.rang_no_answer, 1);
        assert_eq!(p93.failed, 1);
    }

    #[test]
    fn summary_per_number_sorted_by_volume_desc_and_breakdown_sums() {
        let mk = |num: &str, code: i16| {
            let mut r = base_row();
            r.from_number = Some(num.to_string());
            r.status_code = Some(code);
            r.status = if code == 200 { "completed" } else { "failed" }.to_string();
            r
        };
        // "+A" has 2 calls, "+B" has 1 → +A must sort first.
        let rows = vec![mk("+A", 200), mk("+B", 486), mk("+A", 200)];
        let resp = build_summary_response(rows);

        assert_eq!(resp.summary.per_number.len(), 2);
        assert_eq!(resp.summary.per_number[0].number, "+A");
        assert_eq!(resp.summary.per_number[0].total, 2);
        assert_eq!(resp.summary.per_number[1].number, "+B");

        // outcome breakdown always partitions the total exactly.
        let sum: i64 = resp.summary.outcome_breakdown.values().sum();
        assert_eq!(sum, resp.summary.total);
        assert_eq!(resp.summary.total, 3);
    }

    #[test]
    fn summary_empty_input_and_unknown_number_bucket() {
        let resp = build_summary_response(vec![]);
        assert_eq!(resp.summary.total, 0);
        assert_eq!(resp.summary.asr, 0.0);
        assert!(resp.call_list.is_empty());
        assert!(resp.summary.per_number.is_empty());

        // a row without from_number lands under "(unknown)".
        let resp = build_summary_response(vec![base_row()]);
        assert_eq!(resp.summary.per_number[0].number, "(unknown)");
    }
}
