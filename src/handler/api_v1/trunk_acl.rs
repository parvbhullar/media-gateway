//! `/api/v1/trunks/{name}/acl` — TSUB-05 full implementation.
//!
//! Phase 5 Plan 05-03. Backed by `supersip_trunk_acl_entries` (Plan 05-01
//! schema). UNIQUE (trunk_group_id, rule) per D-10. Multi-row replacement
//! for the legacy JSON `rustpbx_trunk_groups.acl` column. Position
//! auto-assigned by the handler as MAX(position)+1 (starting at 0 for the
//! first row) per D-12. DELETE-by-rule strict 404 per D-12. Rule grammar
//! `^(allow|deny) (all|<CIDR>|<IP>)$` (D-13) is validated by
//! [`validate_acl_rule`] — exposed `pub` so Plan 05-04 can reuse it for
//! enforcement-time parsing without re-validating via regex.
//!
//! `TrunkAclEntryView` is the wire type — `trunk_acl_entries::Model` is
//! NEVER serialized directly (SHELL-04).
//!
//! Default policy is `allow` (D-14). Operators that want default-deny MUST
//! append a final `deny all` rule; this handler does not inject one.
//!
//! NOTE: `src/handler/api_v1/mod.rs` already declares this module and
//! merges `router()` into the `/api/v1` surface (registered by Plan 05-01).
//! This file replaces the empty stub body — the `pub fn router()` signature
//! is preserved to keep the existing `.merge(trunk_acl::router())` line
//! compiling.

use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::StatusCode,
    routing::{delete, get},
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::trunk_acl_entries::{
    self, Column as AclColumn, Entity as AclEntity, Model as AclModel,
};
use crate::models::trunk_group::{
    Column as TrunkGroupColumn, Entity as TrunkGroupEntity,
};

// ─── Wire types (SHELL-04) ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TrunkAclEntryView {
    pub rule: String,
    pub position: i32,
}

impl From<AclModel> for TrunkAclEntryView {
    fn from(m: AclModel) -> Self {
        Self {
            rule: m.rule,
            position: m.position,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddTrunkAclEntryRequest {
    pub rule: String,
}

// ─── Validation helpers ──────────────────────────────────────────────────

/// Enforce D-13 rule grammar: `^(allow|deny) (all|<CIDR>|<IP>)$`.
///
/// Three valid forms:
///   - `allow all` / `deny all`
///   - `allow <IPv4|IPv6>` / `deny <IPv4|IPv6>`
///   - `allow <CIDR>` / `deny <CIDR>`
///
/// Marked `pub` so Plan 05-04 can reuse it for enforcement-time parsing
/// instead of running a separate regex round-trip. Returns a descriptive
/// error string suitable for surfacing in a 400 response body.
pub fn validate_acl_rule(rule: &str) -> Result<(), String> {
    let trimmed = rule.trim();
    let mut parts = trimmed.splitn(2, ' ');
    let action = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("").trim();

    let action_lc = action.to_ascii_lowercase();
    if action_lc != "allow" && action_lc != "deny" {
        return Err(format!(
            "rule must start with 'allow' or 'deny': {}",
            rule
        ));
    }
    if target.is_empty() {
        return Err(format!("rule missing target after action: {}", rule));
    }
    if target.eq_ignore_ascii_case("all") {
        return Ok(());
    }
    if target.contains('/') {
        let mut ts = target.splitn(2, '/');
        let ip = ts.next().unwrap();
        let prefix: u8 = ts
            .next()
            .and_then(|p| p.parse().ok())
            .ok_or_else(|| format!("invalid CIDR prefix: {}", target))?;
        let parsed: std::net::IpAddr = ip
            .parse()
            .map_err(|_| format!("invalid CIDR address: {}", target))?;
        let max = if parsed.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(format!(
                "CIDR prefix {} exceeds max for family",
                prefix
            ));
        }
        return Ok(());
    }
    let _ip: std::net::IpAddr = target
        .parse()
        .map_err(|_| format!("invalid IP literal: {}", target))?;
    Ok(())
}

/// Resolve `{name}` to a `trunk_group_id`. Returns 404 if the parent trunk
/// group does not exist — every sub-resource handler calls this first so
/// that missing-parent precedes child lookup (consistent 404 contract
/// across sub-resources; same shape as `trunk_credentials.rs`).
async fn lookup_trunk_group_id(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    account_id: &str,
) -> ApiResult<i64> {
    let group = TrunkGroupEntity::find()
        .filter(TrunkGroupColumn::Name.eq(name))
        .filter(TrunkGroupColumn::AccountId.eq(account_id))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("trunk group '{}' not found", name))
        })?;
    Ok(group.id)
}

// ─── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/trunks/{name}/acl", get(list_acl).post(add_acl_entry))
        .route("/trunks/{name}/acl/{rule}", delete(delete_acl_entry))
}

// ─── Handlers ────────────────────────────────────────────────────────────

/// GET /trunks/{name}/acl — list all ACL entries for a trunk group, ordered
/// by `position` ASC. Empty trunk returns `[]`. Missing trunk returns 404.
async fn list_acl(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<TrunkAclEntryView>>> {
    let db = state.db();
    let trunk_group_id = lookup_trunk_group_id(db, &name, &scope.account_id).await?;

    let rows = AclEntity::find()
        .filter(AclColumn::TrunkGroupId.eq(trunk_group_id))
        .order_by_asc(AclColumn::Position)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(
        rows.into_iter().map(TrunkAclEntryView::from).collect(),
    ))
}

/// POST /trunks/{name}/acl — validate rule grammar (D-13), pre-check
/// UNIQUE (trunk_group_id, rule) for a friendly 409 (D-10/D-12), compute
/// next position as MAX(position)+1 (or 0 for first row), insert, return
/// 201 with the view. The DB UNIQUE index is the safety net for concurrent
/// writes — races surface as 500 (acceptable; operator workflow).
async fn add_acl_entry(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
    Json(req): Json<AddTrunkAclEntryRequest>,
) -> ApiResult<(StatusCode, Json<TrunkAclEntryView>)> {
    let db = state.db();
    validate_acl_rule(&req.rule).map_err(ApiError::bad_request)?;
    let trunk_group_id = lookup_trunk_group_id(db, &name, &scope.account_id).await?;

    // Pre-check duplicate (UNIQUE (trunk_group_id, rule) per D-10).
    let dup = AclEntity::find()
        .filter(AclColumn::TrunkGroupId.eq(trunk_group_id))
        .filter(AclColumn::Rule.eq(req.rule.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "rule '{}' already exists on trunk '{}'",
            req.rule, name
        )));
    }

    // D-12: auto-assign position = MAX(position) + 1, or 0 for first row.
    let next_position: i32 = AclEntity::find()
        .filter(AclColumn::TrunkGroupId.eq(trunk_group_id))
        .order_by_desc(AclColumn::Position)
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map(|r| r.position + 1)
        .unwrap_or(0);

    let now = Utc::now();
    let am = trunk_acl_entries::ActiveModel {
        trunk_group_id: Set(trunk_group_id),
        rule: Set(req.rule.clone()),
        position: Set(next_position),
        created_at: Set(now),
        ..Default::default()
    };

    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(TrunkAclEntryView::from(inserted)),
    ))
}

/// DELETE /trunks/{name}/acl/{rule} — strict 404-on-miss per D-12.
/// `{rule}` is URL-decoded by axum's Path extractor before this handler
/// runs (clients must URL-encode spaces and `/`). Position is NOT
/// renumbered on delete — gaps are acceptable.
async fn delete_acl_entry(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path((name, rule)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let trunk_group_id = lookup_trunk_group_id(db, &name, &scope.account_id).await?;

    let row = AclEntity::find()
        .filter(AclColumn::TrunkGroupId.eq(trunk_group_id))
        .filter(AclColumn::Rule.eq(rule.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "rule '{}' not found on trunk '{}'",
                rule, name
            ))
        })?;

    AclEntity::delete_by_id(row.id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
