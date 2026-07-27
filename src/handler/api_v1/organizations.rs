//! `/api/v1/organizations` — org-level multi-tenancy CRUD, disable/enable,
//! and usage reporting. Org lifecycle (create/update/disable/enable) is
//! owned by an external system that calls this API with its own `org_id`;
//! media-gateway never mints one itself. `PUT` is upsert so the external
//! system doesn't need to track create-vs-update state on its side.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, put},
};
use chrono::{DateTime, NaiveDate, Utc};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::call_record::{Column as CallRecordColumn, Entity as CallRecordEntity};
use crate::models::did::{Column as DidColumn, Entity as DidEntity};
use crate::models::extension::{Column as ExtColumn, Entity as ExtEntity};
use crate::models::organization::{Model as OrgModel, NewOrganization};
use crate::models::trunk::{Column as TrunkColumn, Entity as TrunkEntity};

#[derive(Debug, Serialize)]
pub struct OrganizationView {
    pub org_id: String,
    pub name: String,
    pub enabled: bool,
    pub max_cps: Option<i32>,
    pub max_calls: Option<i32>,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<OrgModel> for OrganizationView {
    fn from(m: OrgModel) -> Self {
        Self {
            org_id: m.org_id,
            name: m.name,
            enabled: m.enabled,
            max_cps: m.max_cps,
            max_calls: m.max_calls,
            contact_name: m.contact_name,
            contact_email: m.contact_email,
            notes: m.notes,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// Today's resource counts for one org — used by the list/detail views and
/// the console Organizations tab.
#[derive(Debug, Serialize)]
pub struct OrgTodayCounts {
    pub did_count: i64,
    pub trunk_count: i64,
    pub extension_count: i64,
    pub call_record_count_today: i64,
}

#[derive(Debug, Serialize)]
pub struct OrganizationWithCounts {
    #[serde(flatten)]
    pub org: OrganizationView,
    pub today: OrgTodayCounts,
}

async fn today_counts(
    db: &sea_orm::DatabaseConnection,
    org_id: &str,
) -> ApiResult<OrgTodayCounts> {
    let day_start: DateTime<Utc> = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| NaiveDate::default().and_hms_opt(0, 0, 0).unwrap())
        .and_utc();

    let did_count = DidEntity::find()
        .filter(DidColumn::OrgId.eq(org_id))
        .count(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let trunk_count = TrunkEntity::find()
        .filter(TrunkColumn::OrgId.eq(org_id))
        .count(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let extension_count = ExtEntity::find()
        .filter(ExtColumn::OrgId.eq(org_id))
        .count(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let call_record_count_today = CallRecordEntity::find()
        .filter(CallRecordColumn::OrgId.eq(org_id))
        .filter(CallRecordColumn::StartedAt.gte(day_start))
        .count(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(OrgTodayCounts {
        did_count: did_count as i64,
        trunk_count: trunk_count as i64,
        extension_count: extension_count as i64,
        call_record_count_today: call_record_count_today as i64,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertOrganizationRequest {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub max_cps: Option<i32>,
    #[serde(default)]
    pub max_calls: Option<i32>,
    #[serde(default)]
    pub contact_name: Option<String>,
    #[serde(default)]
    pub contact_email: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisableAction {
    Immediate,
    Drain,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisableRequest {
    pub action: DisableAction,
    #[serde(default)]
    pub grace_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct UsageResponse {
    pub calls: i64,
    pub minutes: f64,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/organizations", get(list_organizations))
        .route("/organizations/{org_id}", put(upsert_organization).get(get_organization))
        .route("/organizations/{org_id}/disable", patch(disable_organization))
        .route("/organizations/{org_id}/enable", patch(enable_organization))
        .route("/organizations/{org_id}/usage", get(usage))
}

async fn list_organizations(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<OrganizationWithCounts>>> {
    let db = state.db();
    let orgs = OrgModel::list_all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let mut out = Vec::with_capacity(orgs.len());
    for org in orgs {
        let today = today_counts(db, &org.org_id).await?;
        out.push(OrganizationWithCounts {
            org: OrganizationView::from(org),
            today,
        });
    }
    Ok(Json(out))
}

async fn get_organization(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
) -> ApiResult<Json<OrganizationWithCounts>> {
    let db = state.db();
    let org = OrgModel::get(db, &org_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("organization '{org_id}' not found")))?;
    let today = today_counts(db, &org_id).await?;
    Ok(Json(OrganizationWithCounts {
        org: OrganizationView::from(org),
        today,
    }))
}

async fn upsert_organization(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
    Json(req): Json<UpsertOrganizationRequest>,
) -> ApiResult<(StatusCode, Json<OrganizationView>)> {
    let db = state.db();
    let existed = OrgModel::get(db, &org_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .is_some();

    OrgModel::upsert(
        db,
        NewOrganization {
            org_id: org_id.clone(),
            name: req.name,
            enabled: req.enabled,
            max_cps: req.max_cps,
            max_calls: req.max_calls,
            contact_name: req.contact_name,
            contact_email: req.contact_email,
            notes: req.notes,
        },
    )
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;

    let row = OrgModel::get(db, &org_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::internal("row vanished after upsert"))?;

    let status = if existed { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, Json(OrganizationView::from(row))))
}

async fn disable_organization(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
    Json(req): Json<DisableRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let db = state.db();
    if !crate::models::organization::Model::set_enabled(db, &org_id, false)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    {
        return Err(ApiError::not_found(format!("organization '{org_id}' not found")));
    }

    let registry = &state.sip_server().inner.active_call_registry;
    match req.action {
        DisableAction::Immediate => {
            hangup_all_for_org(registry, &org_id, "org_disabled_immediate");
        }
        DisableAction::Drain => {
            let grace = req.grace_seconds.unwrap_or(0);
            let registry = registry.clone();
            let org_id = org_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(grace)).await;
                hangup_all_for_org(&registry, &org_id, "org_disabled_drain_expired");
            });
        }
    }

    Ok(Json(serde_json::json!({ "org_id": org_id, "enabled": false })))
}

/// Hang up every currently active call whose `org_id` matches, via the
/// existing `SipCommand::Hangup` path used elsewhere for system-initiated
/// teardown (see the attended-transfer cleanup in `src/proxy/call.rs` for
/// the same `HangupCommand::local` pattern).
///
/// Only reaches calls tracked in `ActiveProxyCallRegistry` (the legacy
/// SIP-forward path); external-bridge calls (`BridgeSessions`) are not yet
/// reachable by this action — see
/// `docs/superpowers/plans/2026-07-27-org-level-multitenancy.md` Task 11
/// follow-up notes.
fn hangup_all_for_org(
    registry: &crate::proxy::active_call_registry::ActiveProxyCallRegistry,
    org_id: &str,
    source: &str,
) {
    let session_ids = registry.session_ids_by_org(org_id);
    info!(
        org_id,
        count = session_ids.len(),
        "org disable: hung up N active calls via ActiveProxyCallRegistry"
    );
    for session_id in session_ids {
        if let Some(handle) = registry.get_handle(&session_id) {
            let _ = handle.send_command(crate::call::domain::CallCommand::Hangup(
                crate::call::domain::HangupCommand::local(
                    source,
                    Some(crate::callrecord::CallRecordHangupReason::BySystem),
                    Some(200),
                ),
            ));
        }
    }
}

async fn enable_organization(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let db = state.db();
    if !crate::models::organization::Model::set_enabled(db, &org_id, true)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    {
        return Err(ApiError::not_found(format!("organization '{org_id}' not found")));
    }
    Ok(Json(serde_json::json!({ "org_id": org_id, "enabled": true })))
}

async fn usage(
    State(state): State<AppState>,
    Path(org_id): Path<String>,
    Query(q): Query<UsageQuery>,
) -> ApiResult<Json<UsageResponse>> {
    let db = state.db();
    let rows = CallRecordEntity::find()
        .filter(CallRecordColumn::OrgId.eq(&org_id))
        .filter(CallRecordColumn::StartedAt.gte(q.from))
        .filter(CallRecordColumn::StartedAt.lt(q.to))
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let calls = rows.len() as i64;
    let total_secs: i64 = rows
        .iter()
        .filter_map(|r| r.billable_duration_secs)
        .map(|s| s as i64)
        .sum();

    Ok(Json(UsageResponse {
        calls,
        minutes: total_secs as f64 / 60.0,
    }))
}
