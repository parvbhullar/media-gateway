//! `/api/v1/security/*` — Phase 10 Security Suite (SEC-01..SEC-05).
//!
//! Plan 10-02 Wave 2 — full handler implementations replacing the 10-01
//! stub bodies. Each route either reads/writes the SeaORM-backed
//! `supersip_security_*` tables OR snapshots the in-memory
//! [`SecurityState`](crate::proxy::security_state::SecurityState) DashMaps
//! (flood + brute-force counters live in-memory only; see CONTEXT.md D-02).
//!
//! Cache invalidation contract (CONTEXT.md D-15, RISK-06):
//!   - PATCH /firewall — DELETE-all + INSERT-all in ONE DB transaction;
//!     `replace_firewall_cache` is called AFTER commit so the hot path
//!     never observes an empty cache mid-update.
//!   - DELETE /blocks/{ip} — set `unblocked_at = now()` on the row, then
//!     `purge_block_cache_for_ip` to drop the in-memory entry.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::security_blocks::{
    self, Column as BlockColumn, Entity as BlockEntity,
};
use crate::models::security_rules::{
    self, Column as RuleColumn, Entity as RuleEntity,
};
use crate::proxy::security_state::{
    AuthFailureEntry, FirewallRule, FloodEntry,
};

// ─── Wire types (SHELL-04 — never serialize Models directly) ─────────────

#[derive(Debug, Serialize)]
pub struct FirewallRuleView {
    pub id: i64,
    pub position: i32,
    pub action: String,
    pub cidr: String,
    pub description: Option<String>,
}

impl From<security_rules::Model> for FirewallRuleView {
    fn from(m: security_rules::Model) -> Self {
        Self {
            id: m.id,
            position: m.position,
            action: m.action,
            cidr: m.cidr,
            description: m.description,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct FirewallRuleInput {
    pub position: i32,
    pub action: String,
    pub cidr: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReplaceFirewallRequest {
    pub rules: Vec<FirewallRuleInput>,
}

#[derive(Debug, Serialize)]
pub struct FloodTrackerResponse {
    pub data: Vec<FloodEntry>,
}

#[derive(Debug, Serialize)]
pub struct AuthFailuresResponse {
    pub data: Vec<AuthFailureEntry>,
}

#[derive(Debug, Serialize)]
pub struct SecurityBlockView {
    pub id: i64,
    pub ip: String,
    pub realm: String,
    pub block_reason: String,
    pub blocked_at: String,
    pub unblocked_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SecurityBlocksResponse {
    pub data: Vec<SecurityBlockView>,
}

// ─── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/security/firewall",
            get(list_firewall).patch(replace_firewall),
        )
        .route("/security/flood-tracker", get(list_flood_tracker))
        .route("/security/blocks", get(list_blocks))
        .route("/security/blocks/{ip}", delete(delete_block))
        .route("/security/auth-failures", get(list_auth_failures))
}

// ─── Handlers ────────────────────────────────────────────────────────────

/// GET /security/firewall — ordered list (ASC by position) from
/// `supersip_security_rules`. Empty DB returns `[]`.
async fn list_firewall(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<FirewallRuleView>>> {
    let db = state.db();
    let rows = RuleEntity::find()
        .order_by_asc(RuleColumn::Position)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(rows.into_iter().map(FirewallRuleView::from).collect()))
}

/// PATCH /security/firewall — full replace (CONTEXT.md D-13).
/// Validates each rule first (reuses Phase 5 `validate_acl_rule`), then
/// runs DELETE-all + INSERT-all inside ONE DB transaction. Cache update
/// happens AFTER commit (RISK-06 mitigation).
async fn replace_firewall(
    State(state): State<AppState>,
    Json(req): Json<ReplaceFirewallRequest>,
) -> ApiResult<Json<Vec<FirewallRuleView>>> {
    // Validate all rules up-front so we never partially apply on bad input.
    for rule in &req.rules {
        let action_lc = rule.action.to_ascii_lowercase();
        if action_lc != "allow" && action_lc != "deny" {
            return Err(ApiError::bad_request(format!(
                "rule action must be 'allow' or 'deny': {}",
                rule.action
            )));
        }
        // Reuse trunk_acl validator over the canonical "<action> <cidr>"
        // form so firewall rule grammar matches Phase 5 D-13 exactly.
        let canonical = format!("{} {}", action_lc, rule.cidr);
        crate::handler::api_v1::trunk_acl::validate_acl_rule(&canonical)
            .map_err(ApiError::bad_request)?;
    }

    let db = state.db();
    let tx = db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // DELETE all existing rows.
    RuleEntity::delete_many()
        .exec(&tx)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // INSERT all new rows.
    let now = Utc::now();
    for rule in &req.rules {
        let am = security_rules::ActiveModel {
            position: Set(rule.position),
            action: Set(rule.action.to_ascii_lowercase()),
            cidr: Set(rule.cidr.clone()),
            description: Set(rule.description.clone()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        am.insert(&tx)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Re-query AFTER commit (RISK-06): build the cache from the durable
    // view so the hot path observes the same ordering the handler returns.
    let rows = RuleEntity::find()
        .order_by_asc(RuleColumn::Position)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let cache_rules: Vec<FirewallRule> = rows
        .iter()
        .map(|m| FirewallRule {
            position: m.position,
            action: m.action.clone(),
            cidr: m.cidr.clone(),
            description: m.description.clone(),
        })
        .collect();
    state.security_state().replace_firewall_cache(cache_rules);

    Ok(Json(
        rows.into_iter().map(FirewallRuleView::from).collect(),
    ))
}

/// GET /security/flood-tracker — live in-memory snapshot (D-07). No DB.
async fn list_flood_tracker(
    State(state): State<AppState>,
) -> ApiResult<Json<FloodTrackerResponse>> {
    let entries = state.security_state().snapshot_flood_entries();
    Ok(Json(FloodTrackerResponse { data: entries }))
}

/// GET /security/blocks — list active blocks (`unblocked_at IS NULL`).
async fn list_blocks(
    State(state): State<AppState>,
) -> ApiResult<Json<SecurityBlocksResponse>> {
    let db = state.db();
    let rows = BlockEntity::find()
        .filter(BlockColumn::UnblockedAt.is_null())
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let data = rows
        .into_iter()
        .map(|m| SecurityBlockView {
            id: m.id,
            ip: m.ip,
            realm: m.realm,
            block_reason: m.block_reason,
            blocked_at: m.blocked_at.to_rfc3339(),
            unblocked_at: m.unblocked_at.map(|t| t.to_rfc3339()),
        })
        .collect();
    Ok(Json(SecurityBlocksResponse { data }))
}

/// DELETE /security/blocks/{ip} — set `unblocked_at` on every active row
/// for `ip`, purge in-memory cache, return 204. 404 when no active row.
async fn delete_block(
    State(state): State<AppState>,
    Path(ip): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let rows = BlockEntity::find()
        .filter(BlockColumn::Ip.eq(ip.clone()))
        .filter(BlockColumn::UnblockedAt.is_null())
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    if rows.is_empty() {
        return Err(ApiError::not_found(format!(
            "no active block found for ip '{}'",
            ip
        )));
    }

    let now = Utc::now();
    for row in rows {
        let mut am: security_blocks::ActiveModel = row.into();
        am.unblocked_at = Set(Some(now));
        am.update(db)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }

    state.security_state().purge_block_cache_for_ip(&ip);

    Ok(StatusCode::NO_CONTENT)
}

/// GET /security/auth-failures — live in-memory snapshot (D-11). No DB.
async fn list_auth_failures(
    State(state): State<AppState>,
) -> ApiResult<Json<AuthFailuresResponse>> {
    let entries = state.security_state().snapshot_auth_failure_entries();
    Ok(Json(AuthFailuresResponse { data: entries }))
}
