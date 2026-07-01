//! `/api/v1/cdrs` — carrier-API call detail records (Phase 1, Plan 01-03).
//!
//! Thin JSON adapter over `models::call_record`. Recording and sip-flow
//! sub-routes return 501 per CARRIER-API.md — they are promoted to real
//! handlers in Phase 12 (Recordings first-class).

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Duration, Timelike, Utc};
use chrono_tz::Tz;
use sea_orm::{ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::callrecord::outcome::{self, OutcomeKind};
use crate::handler::api_v1::common::{Pagination, PaginatedResponse};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::call_record::{
    self, Column as CdrColumn, Entity as CdrEntity, Model as CdrModel,
};

/// Parsed Q.850 cause for the call's final hangup message (enrich-cdr-api);
/// `None` on the `CdrView` when no cause was recorded.
#[derive(Debug, Serialize)]
pub struct Q850View {
    pub cause: i16,
    pub text: Option<String>,
}

/// Recording availability + pointer. `url` is the stored recording location
/// (local path / HTTP / `s3://…`); serving the bytes is a separate route.
#[derive(Debug, Serialize)]
pub struct RecordingView {
    pub available: bool,
    pub url: Option<String>,
    pub duration_secs: Option<i32>,
}

/// SipFlow availability + a pointer to the sip-flow route. `url` is the
/// `/api/v1/cdrs/{id}/sip-flow` path when a capture exists (enrich-cdr-api).
#[derive(Debug, Serialize)]
pub struct SipFlowView {
    pub available: bool,
    pub url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CdrView {
    pub id: i64,
    pub call_id: String,
    pub direction: String,
    pub status: String,
    pub status_code: Option<i16>,
    /// Denormalized OK/USR/SYS outcome class (enrich-cdr-api).
    pub outcome_kind: Option<String>,
    pub hangup_reason: Option<String>,
    /// Failure origin: "sbc" | "upstream" | "caller"; null for a successful or
    /// clean-hangup call. Lets a consumer tell an SBC-side 503 from a carrier
    /// 503 (503-attribution).
    pub failure_source: Option<String>,
    /// Parsed Q.850 cause/text for the final hangup; null when absent.
    pub q850: Option<Q850View>,
    pub started_at: DateTime<Utc>,
    pub answer_time: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_secs: i32,
    /// Answered (billable) seconds; NULL for unanswered calls (task 2.3).
    pub billable_duration_secs: Option<i32>,
    pub from_number: Option<String>,
    pub to_number: Option<String>,
    pub sip_gateway: Option<String>,
    /// Terminating trunk id, for per-trunk attribution/summary.
    pub sip_trunk_id: Option<i64>,
    /// Matched route + terminating extension, for call attribution (task 2.4).
    pub route_id: Option<i64>,
    pub extension_id: Option<i64>,
    pub caller_uri: Option<String>,
    pub callee_uri: Option<String>,
    /// Per-leg SIP role map, lifted from the CDR metadata JSON (task 2.4).
    pub sip_leg_roles: Option<serde_json::Value>,
    /// Recording availability + URL + duration (replaces the flat
    /// `recording_url`; enrich-cdr-api — flagged as a response-shape change).
    pub recording: RecordingView,
    /// SipFlow availability + sip-flow route pointer (enrich-cdr-api).
    pub sipflow: SipFlowView,
    pub created_at: DateTime<Utc>,
}

/// Lift the per-leg SIP role map out of the CDR metadata JSON.
/// `persist_call_record` stores it under this reserved key (CDR-07); `None`
/// when absent or metadata is null (task 2.4).
fn leg_roles_from_metadata(metadata: &Option<serde_json::Value>) -> Option<serde_json::Value> {
    metadata
        .as_ref()
        .and_then(|md| md.get("sip_leg_roles").cloned())
}

impl From<CdrModel> for CdrView {
    fn from(m: CdrModel) -> Self {
        let sip_leg_roles = leg_roles_from_metadata(&m.metadata);
        let q850 = m.q850_cause.map(|cause| Q850View {
            cause,
            text: m.q850_text,
        });
        let recording = RecordingView {
            available: m.recording_url.is_some(),
            duration_secs: m.recording_duration_secs,
            url: m.recording_url,
        };
        let sipflow = SipFlowView {
            available: m.sipflow_available,
            url: m
                .sipflow_available
                .then(|| format!("/api/v1/cdrs/{}/sip-flow", m.id)),
        };
        Self {
            id: m.id,
            call_id: m.call_id,
            direction: m.direction,
            status: m.status,
            status_code: m.status_code,
            outcome_kind: m.outcome_kind,
            hangup_reason: m.hangup_reason,
            failure_source: m.failure_source,
            q850,
            started_at: m.started_at,
            answer_time: m.answer_time,
            ended_at: m.ended_at,
            duration_secs: m.duration_secs,
            billable_duration_secs: m.billable_duration_secs,
            from_number: m.from_number,
            to_number: m.to_number,
            sip_gateway: m.sip_gateway,
            sip_trunk_id: m.sip_trunk_id,
            route_id: m.route_id,
            extension_id: m.extension_id,
            caller_uri: m.caller_uri,
            callee_uri: m.callee_uri,
            sip_leg_roles,
            recording,
            sipflow,
            created_at: m.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CdrListQuery {
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub page_size: Option<u64>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// Filter by final SIP response code (e.g. 486, 503).
    #[serde(default)]
    pub status_code: Option<i16>,
    /// Filter by failure origin ("sbc" | "upstream" | "caller"), e.g. to count
    /// SBC-side 503s vs carrier 503s (503-attribution).
    #[serde(default)]
    pub failure_source: Option<String>,
    /// Filter by terminating trunk id (enrich-cdr-api: per-trunk CDR listing).
    #[serde(default)]
    pub sip_trunk_id: Option<i64>,
    #[serde(default)]
    pub from_number: Option<String>,
    #[serde(default)]
    pub to_number: Option<String>,
    /// Substring match against either from_number or to_number.
    /// Mutually inclusive with `from_number`/`to_number` — all provided
    /// filters AND together. Use this when you don't care which side.
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_date: Option<DateTime<Utc>>,
}

impl CdrListQuery {
    fn pagination(&self) -> Pagination {
        Pagination {
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/cdrs", get(list_cdrs))
        .route("/cdrs/summary", get(cdr_summary))
        .route("/cdrs/{id}", get(get_cdr).delete(delete_cdr))
        .route("/cdrs/{id}/recording", get(cdr_recording))
        .route("/cdrs/{id}/sip-flow", get(cdr_sip_flow))
}

async fn list_cdrs(
    State(state): State<AppState>,
    Query(q): Query<CdrListQuery>,
) -> ApiResult<Json<PaginatedResponse<CdrView>>> {
    let db = state.db();
    let pagination = q.pagination();
    let page_no = pagination.page.max(1);
    let page_size = pagination.limit();

    let mut conds = Condition::all();
    if let Some(v) = q.direction.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::Direction.eq(v.clone()));
    }
    if let Some(v) = q.status.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::Status.eq(v.clone()));
    }
    if let Some(v) = q.status_code {
        conds = conds.add(CdrColumn::StatusCode.eq(v));
    }
    if let Some(v) = q.failure_source.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::FailureSource.eq(v.clone()));
    }
    if let Some(v) = q.sip_trunk_id {
        conds = conds.add(CdrColumn::SipTrunkId.eq(v));
    }
    if let Some(v) = q.from_number.as_ref().filter(|s| !s.is_empty()) {
        // Prefix match — matches Postman doc ("Filter by caller number prefix").
        conds = conds.add(CdrColumn::FromNumber.like(format!("{}%", v)));
    }
    if let Some(v) = q.to_number.as_ref().filter(|s| !s.is_empty()) {
        // Prefix match — matches Postman doc ("Filter by callee number prefix").
        conds = conds.add(CdrColumn::ToNumber.like(format!("{}%", v)));
    }
    if let Some(v) = q.number.as_ref().filter(|s| !s.is_empty()) {
        let pat = format!("%{}%", v);
        conds = conds.add(
            Condition::any()
                .add(CdrColumn::FromNumber.like(pat.clone()))
                .add(CdrColumn::ToNumber.like(pat)),
        );
    }
    if let Some(v) = q.start_date {
        conds = conds.add(CdrColumn::StartedAt.gte(v));
    }
    if let Some(v) = q.end_date {
        conds = conds.add(CdrColumn::StartedAt.lte(v));
    }

    let paginator = CdrEntity::find()
        .filter(conds)
        .order_by_desc(CdrColumn::StartedAt)
        .paginate(db, page_size);

    let total = paginator
        .num_items()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let rows = paginator
        .fetch_page(page_no.saturating_sub(1))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(PaginatedResponse::new(
        rows.into_iter().map(CdrView::from).collect(),
        page_no,
        page_size,
        total,
    )))
}

/// Filters + grouping for the CDR report summary (enrich-cdr-api). Mirrors the
/// list filters (trunk, number, direction, status, status_code, date range)
/// plus `group_by` (hour|day|month) and a `tz` for bucket labels.
#[derive(Debug, Deserialize)]
pub struct CdrSummaryQuery {
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub status_code: Option<i16>,
    #[serde(default)]
    pub from_number: Option<String>,
    #[serde(default)]
    pub to_number: Option<String>,
    /// Substring match against either from_number or to_number.
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub sip_trunk_id: Option<i64>,
    /// Bucket granularity: `hour` | `day` | `month`. Defaults to `day`.
    #[serde(default)]
    pub group_by: Option<String>,
    /// IANA timezone for bucket labels + hourly volume (default Asia/Kolkata).
    #[serde(default)]
    pub tz: Option<String>,
}

fn summary_conditions(q: &CdrSummaryQuery) -> Condition {
    let mut conds = Condition::all();
    if let Some(v) = q.direction.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::Direction.eq(v.clone()));
    }
    if let Some(v) = q.status.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::Status.eq(v.clone()));
    }
    if let Some(v) = q.status_code {
        conds = conds.add(CdrColumn::StatusCode.eq(v));
    }
    if let Some(v) = q.from_number.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::FromNumber.like(format!("{}%", v)));
    }
    if let Some(v) = q.to_number.as_ref().filter(|s| !s.is_empty()) {
        conds = conds.add(CdrColumn::ToNumber.like(format!("{}%", v)));
    }
    if let Some(v) = q.number.as_ref().filter(|s| !s.is_empty()) {
        let pat = format!("%{}%", v);
        conds = conds.add(
            Condition::any()
                .add(CdrColumn::FromNumber.like(pat.clone()))
                .add(CdrColumn::ToNumber.like(pat)),
        );
    }
    if let Some(v) = q.sip_trunk_id {
        conds = conds.add(CdrColumn::SipTrunkId.eq(v));
    }
    if let Some(v) = q.start_date {
        conds = conds.add(CdrColumn::StartedAt.gte(v));
    }
    if let Some(v) = q.end_date {
        conds = conds.add(CdrColumn::StartedAt.lte(v));
    }
    conds
}

// ── Grouped report ────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub(crate) enum GroupBy {
    Hour,
    Day,
    Month,
}

impl GroupBy {
    pub(crate) fn parse(s: Option<&str>) -> Self {
        match s.map(|x| x.trim().to_ascii_lowercase()).as_deref() {
            Some("hour") => Self::Hour,
            Some("month") => Self::Month,
            _ => Self::Day,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Month => "month",
        }
    }
    fn bucket_key(self, local: &DateTime<Tz>) -> String {
        match self {
            Self::Hour => local.format("%Y-%m-%d %H:00").to_string(),
            Self::Day => local.format("%Y-%m-%d").to_string(),
            Self::Month => local.format("%Y-%m").to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CdrReportResponse {
    pub group_by: String,
    pub buckets: Vec<BucketStat>,
    pub hourly_volume: Vec<HourlyVolume>,
    pub by_direction: Vec<DirectionStat>,
    pub by_gateway: Vec<GatewayStat>,
    pub by_status_code: Vec<StatusCodeStat>,
    pub top_failure_reasons: Vec<FailureReasonStat>,
    pub hangup_reasons: Vec<HangupReasonStat>,
    /// Per caller-number breakdown, highest volume first (analytics-console-page).
    pub per_number: Vec<NumberStat>,
    /// Routing attribution by (direction, trunk, route).
    pub by_routing: Vec<RoutingStat>,
    /// Per-origin international breakdown (dest outside the home country).
    pub international: Vec<IntlOriginStat>,
    /// Total international calls in the window.
    pub international_total: i64,
    pub summary: ReportSummary,
}

#[derive(Debug, Serialize)]
pub struct BucketStat {
    pub bucket: String,
    pub total: i64,
    pub inbound: i64,
    pub outbound: i64,
    pub ok: i64,
    pub usr: i64,
    pub sys: i64,
    pub asr: f64,
    pub talk_minutes: f64,
}

#[derive(Debug, Serialize)]
pub struct HourlyVolume {
    pub hour: String,
    pub inbound: i64,
    pub outbound: i64,
}

#[derive(Debug, Serialize)]
pub struct DirectionStat {
    pub direction: String,
    pub total: i64,
    pub ok: i64,
    pub usr: i64,
    pub sys: i64,
    pub conn_pct: f64,
}

#[derive(Debug, Serialize)]
pub struct GatewayStat {
    pub gateway: String,
    pub total: i64,
    pub ok: i64,
    pub usr: i64,
    pub sys: i64,
    pub conn_pct: f64,
    pub sys_pct: f64,
}

#[derive(Debug, Serialize)]
pub struct StatusCodeStat {
    pub code: i16,
    pub kind: String,
    pub count: i64,
    pub remark: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FailureReasonStat {
    pub code: i16,
    pub kind: String,
    pub count: i64,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HangupReasonStat {
    pub reason: String,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct ReportSummary {
    pub total: i64,
    pub ok: i64,
    pub usr: i64,
    pub sys: i64,
    /// SIP-connected calls (answer_time present). `connected + failed_user +
    /// failed_sys == total` (analytics-console-page).
    pub connected: i64,
    /// Calls where voice started (billable_duration_secs > 0); ⊆ connected.
    pub answered: i64,
    /// Aliases for the report vocabulary: USR / SYS failure counts.
    pub failed_user: i64,
    pub failed_sys: i64,
    pub inbound: i64,
    pub outbound: i64,
    pub asr: f64,
    /// Failures flagged on the SBC or upstream leg (failure_source in sbc|upstream).
    pub upstream_flagged: i64,
    pub talk_minutes: f64,
    pub avg_talk_secs: f64,
}

/// Per caller-number (DID) row for the "By Number" table.
#[derive(Debug, Serialize)]
pub struct NumberStat {
    pub number: String,
    pub total: i64,
    pub connected: i64,
    pub answered: i64,
    pub failed: i64,
    pub conn_pct: f64,
    pub talk_minutes: f64,
}

/// Routing-attribution row: which trunk + route (+ direction) carried the call.
/// v1 is the single terminating trunk; the Leg A→Leg B dual-trunk path is a
/// fast-follow (needs ingress/egress capture).
#[derive(Debug, Serialize)]
pub struct RoutingStat {
    pub direction: String,
    pub trunk: String,
    pub route_id: Option<i64>,
    pub total: i64,
    pub connected: i64,
    pub failed: i64,
    pub conn_pct: f64,
    pub talk_minutes: f64,
}

/// Per-origin international row (dest not in the home country).
#[derive(Debug, Serialize)]
pub struct IntlOriginStat {
    pub origin: String,
    pub total: i64,
    pub answered: i64,
    pub failed: i64,
    pub distinct_dests: i64,
    pub talk_minutes: f64,
}

/// Per-row projection the report aggregates over — decoupled from the wide
/// `CdrModel` so the aggregation is pure and unit-testable in isolation.
pub(crate) struct ReportRow {
    started_at: DateTime<Utc>,
    direction: String,
    kind: OutcomeKind,
    status_code: Option<i16>,
    q850_text: Option<String>,
    sip_gateway: Option<String>,
    hangup_reason: Option<String>,
    upstream_flagged: bool,
    billable_secs: Option<i32>,
    /// SIP dialog connected (200 OK) — drives the "Connected" tier.
    answer_time: Option<DateTime<Utc>>,
    from_number: Option<String>,
    to_number: Option<String>,
    route_id: Option<i64>,
}

/// Resolve a row's OK/USR/SYS class: the denormalized column when present, else
/// recompute (legacy rows written before the column / backfill).
fn kind_of(m: &CdrModel) -> OutcomeKind {
    match m.outcome_kind.as_deref() {
        Some("OK") => OutcomeKind::Ok,
        Some("USR") => OutcomeKind::Usr,
        Some("SYS") => OutcomeKind::Sys,
        _ => {
            let rang = m.ring_time.is_some()
                || m.answer_time.is_some()
                || m
                    .ended_at
                    .map(|e| (e - m.started_at).num_seconds() > 3)
                    .unwrap_or(false);
            outcome::classify(
                m.status_code.unwrap_or(0).max(0) as u16,
                m.q850_cause.map(|c| c.max(0) as u16),
                rang,
            )
        }
    }
}

pub(crate) fn project(m: &CdrModel) -> ReportRow {
    ReportRow {
        started_at: m.started_at,
        direction: m.direction.clone(),
        kind: kind_of(m),
        status_code: m.status_code,
        q850_text: m.q850_text.clone(),
        sip_gateway: m.sip_gateway.clone(),
        hangup_reason: m.hangup_reason.clone(),
        upstream_flagged: matches!(m.failure_source.as_deref(), Some("sbc") | Some("upstream")),
        billable_secs: m.billable_duration_secs,
        answer_time: m.answer_time,
        from_number: m.from_number.clone(),
        to_number: m.to_number.clone(),
        route_id: m.route_id,
    }
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn pct(n: i64, d: i64) -> f64 {
    if d > 0 {
        round1(n as f64 / d as f64 * 100.0)
    } else {
        0.0
    }
}

#[derive(Default, Clone, Copy)]
struct Tally {
    total: i64,
    ok: i64,
    usr: i64,
    sys: i64,
}

impl Tally {
    fn add(&mut self, k: OutcomeKind) {
        self.total += 1;
        match k {
            OutcomeKind::Ok => self.ok += 1,
            OutcomeKind::Usr => self.usr += 1,
            OutcomeKind::Sys => self.sys += 1,
        }
    }
    fn conn_pct(&self) -> f64 {
        pct(self.ok, self.total)
    }
    fn sys_pct(&self) -> f64 {
        pct(self.sys, self.total)
    }
    /// Representative kind for the by-code/by-reason tables (the report shows
    /// one kind per code; pick the most frequent).
    fn dominant(&self) -> &'static str {
        if self.ok >= self.usr && self.ok >= self.sys {
            "OK"
        } else if self.usr >= self.sys {
            "USR"
        } else {
            "SYS"
        }
    }
}

/// Aggregate projected rows into the grouped report. Pure (no DB/clock) so it is
/// unit-testable; reproduces the operational report's sections. `home_dial_code`
/// (e.g. `+91`) drives international classification.
pub(crate) fn build_cdr_report(
    rows: Vec<ReportRow>,
    group_by: GroupBy,
    tz: Tz,
    home_dial_code: &str,
) -> CdrReportResponse {
    use std::collections::{BTreeMap, HashSet};

    #[derive(Default)]
    struct BucketAcc {
        tally: Tally,
        inbound: i64,
        outbound: i64,
        talk_secs: i64,
    }
    // total / connected / answered / talk — for per-number and per-routing rows.
    #[derive(Default)]
    struct FlowAcc {
        total: i64,
        connected: i64,
        answered: i64,
        talk_secs: i64,
    }
    #[derive(Default)]
    struct IntlAcc {
        total: i64,
        answered: i64,
        talk_secs: i64,
        dests: HashSet<String>,
    }

    let mut bucket_map: BTreeMap<String, BucketAcc> = BTreeMap::new();
    let mut hour_map: BTreeMap<u32, (i64, i64)> = BTreeMap::new();
    let mut dir_map: BTreeMap<String, Tally> = BTreeMap::new();
    let mut gw_map: BTreeMap<String, Tally> = BTreeMap::new();
    let mut code_map: BTreeMap<i16, Tally> = BTreeMap::new();
    let mut failure_map: BTreeMap<(i16, String), Tally> = BTreeMap::new();
    let mut hangup_map: BTreeMap<String, i64> = BTreeMap::new();
    let mut number_map: BTreeMap<String, FlowAcc> = BTreeMap::new();
    let mut routing_map: BTreeMap<(String, String, Option<i64>), FlowAcc> = BTreeMap::new();
    let mut intl_map: BTreeMap<String, IntlAcc> = BTreeMap::new();

    let mut summary = Tally::default();
    let mut inbound = 0i64;
    let mut outbound = 0i64;
    let mut upstream_flagged = 0i64;
    let mut talk_secs = 0i64;
    let mut answered_with_talk = 0i64;
    let mut connected_total = 0i64;
    let mut answered_total = 0i64;
    let mut international_total = 0i64;

    for r in &rows {
        let local = r.started_at.with_timezone(&tz);
        let is_inbound = r.direction == "inbound";
        let is_outbound = r.direction == "outbound";
        // Connected = SIP dialog answered (200 OK); Answered = voice started.
        let connected = r.answer_time.is_some();
        let answered = r.billable_secs.map(|b| b > 0).unwrap_or(false);
        let talk = if answered {
            r.billable_secs.unwrap_or(0).max(0) as i64
        } else {
            0
        };

        summary.add(r.kind);
        if is_inbound {
            inbound += 1;
        } else if is_outbound {
            outbound += 1;
        }
        if r.upstream_flagged {
            upstream_flagged += 1;
        }
        if connected {
            connected_total += 1;
        }
        if answered {
            answered_total += 1;
            answered_with_talk += 1;
        }
        talk_secs += talk;

        {
            let b = bucket_map.entry(group_by.bucket_key(&local)).or_default();
            b.tally.add(r.kind);
            if is_inbound {
                b.inbound += 1;
            } else if is_outbound {
                b.outbound += 1;
            }
            b.talk_secs += talk;
        }

        let h = hour_map.entry(local.hour()).or_insert((0, 0));
        if is_inbound {
            h.0 += 1;
        } else if is_outbound {
            h.1 += 1;
        }

        dir_map.entry(r.direction.clone()).or_default().add(r.kind);

        let gw = r
            .sip_gateway
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(direct)".to_string());
        gw_map.entry(gw.clone()).or_default().add(r.kind);

        if let Some(code) = r.status_code {
            code_map.entry(code).or_default().add(r.kind);
            if !matches!(r.kind, OutcomeKind::Ok) {
                let text = r.q850_text.clone().unwrap_or_default();
                failure_map.entry((code, text)).or_default().add(r.kind);
            }
        }

        if let Some(reason) = &r.hangup_reason {
            *hangup_map.entry(reason.clone()).or_insert(0) += 1;
        }

        // Per caller-number (By Number).
        let num_key = r
            .from_number
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(unknown)".to_string());
        let na = number_map.entry(num_key).or_default();
        na.total += 1;
        na.connected += connected as i64;
        na.answered += answered as i64;
        na.talk_secs += talk;

        // Routing attribution (By Dialplan) — direction · trunk · route.
        let ra = routing_map
            .entry((r.direction.clone(), gw, r.route_id))
            .or_default();
        ra.total += 1;
        ra.connected += connected as i64;
        ra.answered += answered as i64;
        ra.talk_secs += talk;

        // International — destination outside the home country.
        if let Some(to) = r.to_number.as_deref() {
            if crate::callrecord::intl::is_international(to, home_dial_code) {
                international_total += 1;
                let origin = r
                    .from_number
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "(unknown)".to_string());
                let ia = intl_map.entry(origin).or_default();
                ia.total += 1;
                ia.answered += answered as i64;
                ia.talk_secs += talk;
                ia.dests.insert(to.to_string());
            }
        }
    }

    let buckets: Vec<BucketStat> = bucket_map
        .into_iter()
        .map(|(bucket, a)| BucketStat {
            bucket,
            total: a.tally.total,
            inbound: a.inbound,
            outbound: a.outbound,
            ok: a.tally.ok,
            usr: a.tally.usr,
            sys: a.tally.sys,
            asr: a.tally.conn_pct(),
            talk_minutes: round1(a.talk_secs as f64 / 60.0),
        })
        .collect();

    let hourly_volume: Vec<HourlyVolume> = hour_map
        .into_iter()
        .map(|(h, (inb, outb))| HourlyVolume {
            hour: format!("{:02}:00", h),
            inbound: inb,
            outbound: outb,
        })
        .collect();

    let by_direction: Vec<DirectionStat> = dir_map
        .into_iter()
        .map(|(direction, t)| DirectionStat {
            direction,
            total: t.total,
            ok: t.ok,
            usr: t.usr,
            sys: t.sys,
            conn_pct: t.conn_pct(),
        })
        .collect();

    let mut by_gateway: Vec<GatewayStat> = gw_map
        .into_iter()
        .map(|(gateway, t)| GatewayStat {
            gateway,
            total: t.total,
            ok: t.ok,
            usr: t.usr,
            sys: t.sys,
            conn_pct: t.conn_pct(),
            sys_pct: t.sys_pct(),
        })
        .collect();
    by_gateway.sort_by(|a, b| b.total.cmp(&a.total));

    let mut by_status_code: Vec<StatusCodeStat> = code_map
        .into_iter()
        .map(|(code, t)| StatusCodeStat {
            code,
            kind: t.dominant().to_string(),
            count: t.total,
            remark: outcome::remark(code.max(0) as u16).map(str::to_string),
        })
        .collect();
    by_status_code.sort_by(|a, b| b.count.cmp(&a.count));

    let mut top_failure_reasons: Vec<FailureReasonStat> = failure_map
        .into_iter()
        .map(|((code, text), t)| FailureReasonStat {
            code,
            kind: t.dominant().to_string(),
            count: t.total,
            reason: (!text.is_empty()).then_some(text),
        })
        .collect();
    top_failure_reasons.sort_by(|a, b| b.count.cmp(&a.count));
    top_failure_reasons.truncate(12);

    let mut hangup_reasons: Vec<HangupReasonStat> = hangup_map
        .into_iter()
        .map(|(reason, count)| HangupReasonStat { reason, count })
        .collect();
    hangup_reasons.sort_by(|a, b| b.count.cmp(&a.count));

    let mut per_number: Vec<NumberStat> = number_map
        .into_iter()
        .map(|(number, a)| NumberStat {
            number,
            total: a.total,
            connected: a.connected,
            answered: a.answered,
            failed: a.total - a.connected,
            conn_pct: pct(a.connected, a.total),
            talk_minutes: round1(a.talk_secs as f64 / 60.0),
        })
        .collect();
    per_number.sort_by(|a, b| b.total.cmp(&a.total));

    let mut by_routing: Vec<RoutingStat> = routing_map
        .into_iter()
        .map(|((direction, trunk, route_id), a)| RoutingStat {
            direction,
            trunk,
            route_id,
            total: a.total,
            connected: a.connected,
            failed: a.total - a.connected,
            conn_pct: pct(a.connected, a.total),
            talk_minutes: round1(a.talk_secs as f64 / 60.0),
        })
        .collect();
    by_routing.sort_by(|a, b| b.total.cmp(&a.total));

    let mut international: Vec<IntlOriginStat> = intl_map
        .into_iter()
        .map(|(origin, a)| IntlOriginStat {
            origin,
            total: a.total,
            answered: a.answered,
            failed: a.total - a.answered,
            distinct_dests: a.dests.len() as i64,
            talk_minutes: round1(a.talk_secs as f64 / 60.0),
        })
        .collect();
    international.sort_by(|a, b| b.total.cmp(&a.total));

    let avg_talk_secs = if answered_with_talk > 0 {
        round1(talk_secs as f64 / answered_with_talk as f64)
    } else {
        0.0
    };

    let report_summary = ReportSummary {
        total: summary.total,
        ok: summary.ok,
        usr: summary.usr,
        sys: summary.sys,
        connected: connected_total,
        answered: answered_total,
        failed_user: summary.usr,
        failed_sys: summary.sys,
        inbound,
        outbound,
        asr: summary.conn_pct(),
        upstream_flagged,
        talk_minutes: round1(talk_secs as f64 / 60.0),
        avg_talk_secs,
    };

    CdrReportResponse {
        group_by: group_by.as_str().to_string(),
        buckets,
        hourly_volume,
        by_direction,
        by_gateway,
        by_status_code,
        top_failure_reasons,
        hangup_reasons,
        per_number,
        by_routing,
        international,
        international_total,
        summary: report_summary,
    }
}

/// Fetch + filter + aggregate the grouped report for a query. Single shared
/// path behind `/api/v1/cdrs/summary` and the console `/console/analytics/data`
/// (analytics-console-page) so the two can't drift.
pub(crate) async fn compute_cdr_report(
    db: &sea_orm::DatabaseConnection,
    q: &CdrSummaryQuery,
) -> Result<CdrReportResponse, sea_orm::DbErr> {
    let group_by = GroupBy::parse(q.group_by.as_deref());
    let tz: Tz = q
        .tz
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
        .unwrap_or(chrono_tz::Asia::Kolkata);

    let mut conds = summary_conditions(q);
    // Bound the scan: default to the last 30 days when no lower bound is given.
    if q.start_date.is_none() {
        conds = conds.add(CdrColumn::StartedAt.gte(Utc::now() - Duration::days(30)));
    }

    let home = crate::config_merge::read_home_dial_code(db).await;
    let rows = CdrEntity::find().filter(conds).all(db).await?;
    let report_rows: Vec<ReportRow> = rows.iter().map(project).collect();
    Ok(build_cdr_report(report_rows, group_by, tz, &home))
}

async fn cdr_summary(
    State(state): State<AppState>,
    Query(q): Query<CdrSummaryQuery>,
) -> ApiResult<Json<CdrReportResponse>> {
    let report = compute_cdr_report(state.db(), &q)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(report))
}

async fn get_cdr(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<CdrView>> {
    let db = state.db();
    let row = CdrEntity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("CDR {id} not found")))?;
    Ok(Json(CdrView::from(row)))
}

async fn delete_cdr(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let outcome = call_record::Entity::delete_by_id(id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if outcome.rows_affected == 0 {
        return Err(ApiError::not_found(format!("CDR {id} not found")));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Recording retrieval (enrich-cdr-api) — delegates to the shared console
/// streamer (file → S3 → sipflow fallback, with HTTP range support). The
/// api_v1 auth middleware already gates the route.
#[cfg(feature = "console")]
async fn cdr_recording(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match state.console.as_ref() {
        Some(console) => {
            crate::console::handlers::call_record::stream_recording_response(
                console, id, &headers,
            )
            .await
        }
        None => ApiError::unavailable("recording requires console state").into_response(),
    }
}

#[cfg(not(feature = "console"))]
async fn cdr_recording(Path(_id): Path<i64>) -> ApiResult<StatusCode> {
    Err(ApiError::unavailable("recording requires the `console` feature"))
}

/// Sip-flow retrieval (enrich-cdr-api) — delegates to the shared console
/// builder. The api_v1 auth middleware already gates the route.
#[cfg(feature = "console")]
async fn cdr_sip_flow(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match state.console.as_ref() {
        // &Arc<ConsoleState> coerces to &ConsoleState in arg position.
        Some(console) => {
            crate::console::handlers::call_record::build_sip_flow_response(
                console,
                &id.to_string(),
            )
            .await
        }
        None => ApiError::unavailable("sip flow requires console state").into_response(),
    }
}

#[cfg(not(feature = "console"))]
async fn cdr_sip_flow(Path(_id): Path<i64>) -> ApiResult<StatusCode> {
    Err(ApiError::unavailable("sip flow requires the `console` feature"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leg_roles_extracted_from_metadata() {
        let meta = serde_json::json!({
            "sip_leg_roles": {"leg-a": "caller", "leg-b": "callee"},
            "other": "x",
        });
        let roles = leg_roles_from_metadata(&Some(meta)).expect("present");
        assert_eq!(roles["leg-a"], "caller");
        assert_eq!(roles["leg-b"], "callee");
    }

    #[test]
    fn leg_roles_none_when_absent_or_null() {
        assert!(leg_roles_from_metadata(&None).is_none());
        assert!(leg_roles_from_metadata(&Some(serde_json::json!({"other": "x"}))).is_none());
    }

    // ---- grouped report (build_cdr_report) ----

    fn rrow(
        ts: DateTime<Utc>,
        dir: &str,
        kind: OutcomeKind,
        code: i16,
        talk: Option<i32>,
        upstream: bool,
    ) -> ReportRow {
        ReportRow {
            started_at: ts,
            direction: dir.to_string(),
            kind,
            status_code: Some(code),
            q850_text: None,
            sip_gateway: None,
            hangup_reason: None,
            upstream_flagged: upstream,
            billable_secs: talk,
            // a call with talk is connected (has an answer_time)
            answer_time: talk.map(|_| ts),
            from_number: None,
            to_number: None,
            route_id: None,
        }
    }

    #[test]
    fn report_partitions_total_and_buckets_in_ist() {
        use chrono::TimeZone;
        let ist = chrono_tz::Asia::Kolkata;
        let rows = vec![
            // 04:30Z = 10:00 IST, outbound answered, 120s talk
            rrow(
                Utc.with_ymd_and_hms(2026, 6, 30, 4, 30, 0).unwrap(),
                "outbound",
                OutcomeKind::Ok,
                200,
                Some(120),
                false,
            ),
            // 20:00Z on the 29th = 01:30 IST on the 30th → same IST day
            rrow(
                Utc.with_ymd_and_hms(2026, 6, 29, 20, 0, 0).unwrap(),
                "outbound",
                OutcomeKind::Usr,
                480,
                None,
                false,
            ),
            // inbound SYS, upstream-flagged
            rrow(
                Utc.with_ymd_and_hms(2026, 6, 30, 4, 45, 0).unwrap(),
                "inbound",
                OutcomeKind::Sys,
                403,
                None,
                true,
            ),
        ];
        let r = build_cdr_report(rows, GroupBy::Day, ist, "+91");

        assert_eq!(r.summary.ok + r.summary.usr + r.summary.sys, r.summary.total);
        assert_eq!(r.summary.total, 3);
        assert_eq!(r.summary.ok, 1);
        // Connected (has answer_time) + Answered (talk > 0) tiers.
        assert_eq!(r.summary.connected, 1);
        assert_eq!(r.summary.answered, 1);
        assert_eq!(r.summary.connected + r.summary.failed_user + r.summary.failed_sys, r.summary.total);
        assert_eq!(r.summary.upstream_flagged, 1);
        assert_eq!(r.summary.talk_minutes, 2.0);
        assert_eq!(r.summary.avg_talk_secs, 120.0);

        // All three fall in the IST day 2026-06-30 (cross-day rollover handled).
        assert_eq!(r.buckets.len(), 1);
        assert_eq!(r.buckets[0].bucket, "2026-06-30");

        // 04:30Z → 10:00 IST; 04:45Z → 10:15 IST (hour 10).
        let h10 = r.hourly_volume.iter().find(|h| h.hour == "10:00").unwrap();
        assert_eq!(h10.outbound, 1);
        assert_eq!(h10.inbound, 1);

        // by_status_code carries kind + remark.
        let c480 = r.by_status_code.iter().find(|c| c.code == 480).unwrap();
        assert_eq!(c480.kind, "USR");
        assert!(c480.remark.is_some());
    }

    #[test]
    fn report_month_grouping_in_ist() {
        use chrono::TimeZone;
        let r = build_cdr_report(
            vec![rrow(
                Utc.with_ymd_and_hms(2026, 6, 30, 4, 30, 0).unwrap(),
                "outbound",
                OutcomeKind::Ok,
                200,
                Some(60),
                false,
            )],
            GroupBy::Month,
            chrono_tz::Asia::Kolkata,
            "+91",
        );
        assert_eq!(r.group_by, "month");
        assert_eq!(r.buckets[0].bucket, "2026-06");
    }
}
