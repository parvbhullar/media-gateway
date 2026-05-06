//! `/api/v1/routing/tables/{name}/records[/{record_id}]` — RTE-02 record
//! CRUD surface (Phase 6 Plan 06-03).
//!
//! Records are stored as an embedded JSON array on the parent
//! `supersip_routing_tables.records` column (D-01). This handler is the
//! adapter layer that exposes them as discrete REST resources with stable
//! `record_id` URLs (UUID v4, server-generated on POST per D-02).
//!
//! Endpoints (D-28):
//!   - GET    /routing/tables/{name}/records              — list records (position ASC)
//!   - POST   /routing/tables/{name}/records              — append OR insert-at-index (server gen UUIDv4)
//!   - GET    /routing/tables/{name}/records/{record_id}  — fetch one
//!   - PUT    /routing/tables/{name}/records/{record_id}  — replace (record_id + position preserved, D-04)
//!   - DELETE /routing/tables/{name}/records/{record_id}  — remove (no position renumber)
//!
//! Validator `validate_routing_record` is `pub` so Plan 06-02 (initial
//! records on POST-table) and Plan 06-04 (matcher safety net) can reuse it.

use std::collections::HashMap;
use std::net::IpAddr;

use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::StatusCode,
    routing::get,
};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::routing_tables;

// ─── Constants ──────────────────────────────────────────────────────────

/// Per-table record cap (Claude's discretion, threat T-06-03-04).
const MAX_RECORDS_PER_TABLE: usize = 1000;

/// Regex pattern length cap (threat T-06-03-01: regex DoS by complexity).
const MAX_REGEX_PATTERN_LEN: usize = 4096;

/// HttpQuery timeout cap in milliseconds (D-16, threat T-06-03-03).
const MAX_HTTP_QUERY_TIMEOUT_MS: u32 = 5000;

/// Default HttpQuery timeout when caller omits `timeout_ms`.
const DEFAULT_HTTP_QUERY_TIMEOUT_MS: u32 = 2000;

// ─── Wire types (D-03, D-08..D-12, D-24) ────────────────────────────────

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RoutingRecord {
    pub record_id: String, // UUID v4 (server-generated on POST)
    pub position: i32,
    #[serde(rename = "match")]
    pub match_: RoutingMatch,
    pub target: RoutingTarget,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default = "default_true")]
    pub is_active: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutingMatch {
    /// D-08 Longest-prefix match.
    Lpm { prefix: String },
    /// D-09 Exact equality.
    ExactMatch { value: String },
    /// D-10 Regex match (pattern compiled at write time, length-capped).
    Regex { pattern: String },
    /// D-11 Numeric compare.
    Compare { op: CompareOp, value: CompareValue },
    /// D-12 External HTTP query.
    HttpQuery {
        url: String,
        #[serde(default)]
        timeout_ms: Option<u32>,
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompareOp {
    Eq,
    Lt,
    Gt,
    In,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum CompareValue {
    Single(u32),
    Range([u32; 2]),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutingTarget {
    /// D-24 dispatch to a trunk group.
    TrunkGroup { name: String },
    Gateway { name: String },
    NextTable { name: String },
    Reject { code: u16, reason: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRecordRequest {
    #[serde(rename = "match")]
    pub match_: RoutingMatch,
    pub target: RoutingTarget,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default = "default_true")]
    pub is_active: bool,
    /// None = append; Some(i) = insert at index i, shifting >= i upward.
    #[serde(default)]
    pub position: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRecordRequest {
    #[serde(rename = "match")]
    pub match_: RoutingMatch,
    pub target: RoutingTarget,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default = "default_true")]
    pub is_active: bool,
    // NO record_id, NO position — server-managed (D-04, D-28).
}

// ─── Validator ──────────────────────────────────────────────────────────

/// PUBLIC validator — single source of truth for record correctness.
///
/// Re-used by:
///   - Plan 06-02 (`CreateRoutingTableRequest.records` initial array)
///   - Plan 06-04 (matcher safety net before dispatch)
///
/// Enforces:
///   - Regex pattern length cap + compile (T-06-03-01 regex DoS)
///   - HttpQuery URL scheme http/https only + denylist of private/loopback
///     IPs and `localhost` hostname (T-06-03-02 SSRF)
///   - HttpQuery `timeout_ms` <= 5000 (T-06-03-03 DoS)
///   - Compare op/value shape correlation (`In` requires Range, others Single)
///   - Reject target code in 400..=699
pub fn validate_routing_record(rec: &RoutingRecord) -> Result<(), String> {
    validate_match(&rec.match_)?;
    validate_target(&rec.target)?;
    Ok(())
}

fn validate_match(m: &RoutingMatch) -> Result<(), String> {
    match m {
        RoutingMatch::Lpm { prefix } => {
            if prefix.is_empty() {
                return Err("lpm.prefix must not be empty".to_string());
            }
            Ok(())
        }
        RoutingMatch::ExactMatch { value } => {
            if value.is_empty() {
                return Err("exact_match.value must not be empty".to_string());
            }
            Ok(())
        }
        RoutingMatch::Regex { pattern } => {
            if pattern.len() > MAX_REGEX_PATTERN_LEN {
                return Err(format!(
                    "regex.pattern length {} exceeds cap {}",
                    pattern.len(),
                    MAX_REGEX_PATTERN_LEN
                ));
            }
            regex::Regex::new(pattern)
                .map_err(|e| format!("regex.pattern does not compile: {}", e))?;
            Ok(())
        }
        RoutingMatch::Compare { op, value } => match (op, value) {
            (CompareOp::In, CompareValue::Range([lo, hi])) => {
                if lo > hi {
                    return Err(format!(
                        "compare.value range [{}, {}] has lo > hi",
                        lo, hi
                    ));
                }
                Ok(())
            }
            (CompareOp::In, CompareValue::Single(_)) => Err(
                "compare.op=in requires value to be a [lo, hi] range".to_string(),
            ),
            (_, CompareValue::Range(_)) => Err(format!(
                "compare.op={:?} requires a single numeric value, not a range",
                op
            )),
            (_, CompareValue::Single(_)) => Ok(()),
        },
        RoutingMatch::HttpQuery {
            url,
            timeout_ms,
            ..
        } => {
            let parsed = url::Url::parse(url)
                .map_err(|e| format!("http_query.url invalid: {}", e))?;
            let scheme = parsed.scheme();
            if scheme != "http" && scheme != "https" {
                return Err(format!(
                    "http_query.url scheme must be http or https, got '{}' (SSRF mitigation)",
                    scheme
                ));
            }
            let host = parsed
                .host_str()
                .ok_or_else(|| "http_query.url has no host".to_string())?;
            if is_loopback_or_private_host(host) {
                return Err(format!(
                    "http_query.url host '{}' is loopback or private (SSRF denylist)",
                    host
                ));
            }
            let effective_timeout =
                timeout_ms.unwrap_or(DEFAULT_HTTP_QUERY_TIMEOUT_MS);
            if effective_timeout > MAX_HTTP_QUERY_TIMEOUT_MS {
                return Err(format!(
                    "http_query.timeout_ms {} exceeds cap {}",
                    effective_timeout, MAX_HTTP_QUERY_TIMEOUT_MS
                ));
            }
            Ok(())
        }
    }
}

fn validate_target(t: &RoutingTarget) -> Result<(), String> {
    match t {
        RoutingTarget::TrunkGroup { name }
        | RoutingTarget::Gateway { name }
        | RoutingTarget::NextTable { name } => {
            if name.is_empty() {
                Err("target.name must not be empty".to_string())
            } else {
                Ok(())
            }
        }
        RoutingTarget::Reject { code, reason: _ } => {
            if !(400..=699).contains(code) {
                return Err(format!(
                    "reject.code {} must be in 400..=699 SIP error range",
                    code
                ));
            }
            Ok(())
        }
    }
}

/// SSRF denylist: returns true for hostnames/IPs that must NOT be reachable
/// from the operator-supplied HttpQuery URL.
///
/// Blocked:
///   - Bare `localhost` (case-insensitive)
///   - IPv4 loopback 127.0.0.0/8, link-local 169.254.0.0/16
///   - IPv4 private RFC1918: 10/8, 172.16/12, 192.168/16
///   - IPv6 loopback ::1, ULA fc00::/7, link-local fe80::/10
fn is_loopback_or_private_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = stripped.parse::<IpAddr>() {
        return is_private_or_loopback_ip(&ip);
    }
    false
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
                // fc00::/7 ULA
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10 link-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

// ─── Router ─────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/routing/tables/{name}/records",
            get(list_records).post(create_record),
        )
        .route(
            "/routing/tables/{name}/records/{record_id}",
            get(get_record).put(update_record).delete(delete_record),
        )
}

// ─── Read-modify-write helpers ──────────────────────────────────────────

async fn load_table(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    account_id: &str,
) -> ApiResult<routing_tables::Model> {
    routing_tables::Entity::find()
        .filter(routing_tables::Column::Name.eq(name))
        .filter(routing_tables::Column::AccountId.eq(account_id))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("routing table '{}' not found", name))
        })
}

fn parse_records(model: &routing_tables::Model) -> ApiResult<Vec<RoutingRecord>> {
    if model.records.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value::<Vec<RoutingRecord>>(model.records.clone())
        .map_err(|e| {
            ApiError::internal(format!(
                "stored records column for table '{}' is corrupt: {}",
                model.name, e
            ))
        })
}

async fn save_records(
    db: &sea_orm::DatabaseConnection,
    model: routing_tables::Model,
    records: Vec<RoutingRecord>,
) -> ApiResult<()> {
    let new_json = serde_json::to_value(&records)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut am: routing_tables::ActiveModel = model.into();
    am.records = Set(new_json);
    am.updated_at = Set(Utc::now());
    am.update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

/// Enforces "at most one default per table" (D-18, T-06-03-07).
fn check_default_uniqueness(records: &[RoutingRecord]) -> Result<(), ApiError> {
    let count = records.iter().filter(|r| r.is_default).count();
    if count > 1 {
        return Err(ApiError::bad_request(format!(
            "at most one record may have is_default=true per table; found {}",
            count
        )));
    }
    Ok(())
}

// ─── Handlers ───────────────────────────────────────────────────────────

/// GET /routing/tables/{name}/records — list records ordered by position ASC.
async fn list_records(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<RoutingRecord>>> {
    let db = state.db();
    let model = load_table(db, &name, &scope.account_id).await?;
    let mut records = parse_records(&model)?;
    records.sort_by_key(|r| r.position);
    Ok(Json(records))
}

/// POST /routing/tables/{name}/records — append OR insert-at-index.
/// Server generates `record_id` (UUID v4 per D-02).
async fn create_record(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
    Json(req): Json<CreateRecordRequest>,
) -> ApiResult<(StatusCode, Json<RoutingRecord>)> {
    let db = state.db();
    let model = load_table(db, &name, &scope.account_id).await?;
    let mut records = parse_records(&model)?;

    if records.len() >= MAX_RECORDS_PER_TABLE {
        return Err(ApiError::bad_request(format!(
            "table '{}' already has {} records (cap is {})",
            name,
            records.len(),
            MAX_RECORDS_PER_TABLE
        )));
    }

    records.sort_by_key(|r| r.position);
    let new_position: i32 = match req.position {
        None => records.last().map(|r| r.position + 1).unwrap_or(0),
        Some(idx) => {
            if idx < 0 {
                return Err(ApiError::bad_request(
                    "position must be >= 0".to_string(),
                ));
            }
            for r in records.iter_mut() {
                if r.position >= idx {
                    r.position += 1;
                }
            }
            idx
        }
    };

    let new_record = RoutingRecord {
        record_id: Uuid::new_v4().to_string(),
        position: new_position,
        match_: req.match_,
        target: req.target,
        is_default: req.is_default,
        is_active: req.is_active,
    };
    validate_routing_record(&new_record).map_err(ApiError::bad_request)?;

    records.push(new_record.clone());
    records.sort_by_key(|r| r.position);
    check_default_uniqueness(&records)?;

    save_records(db, model, records).await?;
    Ok((StatusCode::CREATED, Json(new_record)))
}

/// GET /routing/tables/{name}/records/{record_id}
async fn get_record(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path((name, record_id)): Path<(String, String)>,
) -> ApiResult<Json<RoutingRecord>> {
    let db = state.db();
    let model = load_table(db, &name, &scope.account_id).await?;
    let records = parse_records(&model)?;
    let rec = records
        .into_iter()
        .find(|r| r.record_id == record_id)
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "record '{}' not found in table '{}'",
                record_id, name
            ))
        })?;
    Ok(Json(rec))
}

/// PUT /routing/tables/{name}/records/{record_id} — full replace; preserves
/// `record_id` and `position` (D-04, D-28).
async fn update_record(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path((name, record_id)): Path<(String, String)>,
    Json(req): Json<UpdateRecordRequest>,
) -> ApiResult<Json<RoutingRecord>> {
    let db = state.db();
    let model = load_table(db, &name, &scope.account_id).await?;
    let mut records = parse_records(&model)?;

    let idx = records
        .iter()
        .position(|r| r.record_id == record_id)
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "record '{}' not found in table '{}'",
                record_id, name
            ))
        })?;

    let preserved_position = records[idx].position;
    let preserved_id = records[idx].record_id.clone();

    let updated = RoutingRecord {
        record_id: preserved_id,
        position: preserved_position,
        match_: req.match_,
        target: req.target,
        is_default: req.is_default,
        is_active: req.is_active,
    };
    validate_routing_record(&updated).map_err(ApiError::bad_request)?;

    records[idx] = updated.clone();
    check_default_uniqueness(&records)?;

    save_records(db, model, records).await?;
    Ok(Json(updated))
}

/// DELETE /routing/tables/{name}/records/{record_id} — sparse: positions of
/// surviving records are NOT renumbered (gaps acceptable).
async fn delete_record(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path((name, record_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let model = load_table(db, &name, &scope.account_id).await?;
    let mut records = parse_records(&model)?;

    let before = records.len();
    records.retain(|r| r.record_id != record_id);
    if records.len() == before {
        return Err(ApiError::not_found(format!(
            "record '{}' not found in table '{}'",
            record_id, name
        )));
    }

    save_records(db, model, records).await?;
    Ok(StatusCode::NO_CONTENT)
}
