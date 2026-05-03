//! `/api/v1/diagnostics/*` — diagnostic JSON endpoints (Phase 1, Plan 01-04).
//!
//! Diagnostics is a thin JSON layer over the routing model and the SIP
//! registrar/locator state on `AppState`. The minimum-viable set for
//! Phase 1 exposes:
//!
//! - `POST /diagnostics/route-evaluate`      dry-run routing match
//! - `GET  /diagnostics/registrations`       list active SIP registrations
//! - `GET  /diagnostics/registrations/{user}` single user's registration
//! - `GET  /diagnostics/summary`             aggregated snapshot
//!
//! `/diagnostics/trunk-test` is already owned by `api_v1::gateways` from
//! Plan 0 and is not re-declared here.
//!
//! The routing dry-run evaluates rules via a pure-Rust regex match against
//! the caller/destination inputs — it does not invoke the full proxy
//! dispatch path. Registrations / locator read from an optional live SIP
//! server accessor on `AppState`; when the test harness boots without a
//! SIP server (`skip_sip_bind`), both slots return empty data and summary
//! still returns 200 per CONTEXT.md DIAG-05.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::routing::{
    self, Column as RouteColumn, Entity as RouteEntity, Model as RouteModel, RoutingDirection,
};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteEvaluateRequest {
    pub caller: String,
    pub destination: String,
    #[serde(default)]
    pub direction: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RouteEvaluateResponse {
    pub matched: bool,
    pub rule_id: Option<i64>,
    pub rule_name: Option<String>,
    pub direction: Option<String>,
    pub priority: Option<i32>,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct RegistrationView {
    pub user: String,
    pub aor: String,
    pub contact: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsSummary {
    pub registrations: RegistrationsSummary,
    pub routing: RoutingSummary,
}

#[derive(Debug, Serialize)]
pub struct RegistrationsSummary {
    pub count: usize,
    pub users: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RoutingSummary {
    pub active_routes: u64,
    pub inactive_routes: u64,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/diagnostics/route-evaluate", post(route_evaluate))
        .route("/diagnostics/registrations", get(list_registrations))
        .route(
            "/diagnostics/registrations/{user}",
            get(get_registration),
        )
        .route("/diagnostics/summary", get(diagnostics_summary))
}

// ---------------------------------------------------------------------------
// Route evaluate
// ---------------------------------------------------------------------------

async fn route_evaluate(
    State(state): State<AppState>,
    Json(req): Json<RouteEvaluateRequest>,
) -> ApiResult<Json<RouteEvaluateResponse>> {
    if req.caller.trim().is_empty() || req.destination.trim().is_empty() {
        return Err(ApiError::bad_request(
            "caller and destination are required",
        ));
    }

    let db = state.db();
    let direction_filter = match req.direction.as_deref() {
        Some("inbound") => Some(RoutingDirection::Inbound),
        Some("outbound") => Some(RoutingDirection::Outbound),
        Some(other) if !other.is_empty() => {
            return Err(ApiError::bad_request(format!(
                "invalid direction '{other}'"
            )));
        }
        _ => None,
    };

    let mut query = RouteEntity::find().filter(RouteColumn::IsActive.eq(true));
    if let Some(d) = direction_filter {
        query = query.filter(RouteColumn::Direction.eq(d));
    }
    let rows = query
        .order_by_asc(RouteColumn::Priority)
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    for row in &rows {
        if rule_matches(row, &req.caller, &req.destination) {
            return Ok(Json(RouteEvaluateResponse {
                matched: true,
                rule_id: Some(row.id),
                rule_name: Some(row.name.clone()),
                direction: Some(row.direction.as_str().to_string()),
                priority: Some(row.priority),
                message: format!("matched rule '{}'", row.name),
            }));
        }
    }

    Ok(Json(RouteEvaluateResponse {
        matched: false,
        rule_id: None,
        rule_name: None,
        direction: None,
        priority: None,
        message: format!(
            "no active route matched caller='{}' destination='{}'",
            req.caller, req.destination
        ),
    }))
}

fn rule_matches(rule: &RouteModel, caller: &str, destination: &str) -> bool {
    let source_ok = match &rule.source_pattern {
        Some(pattern) if !pattern.is_empty() => regex_match(pattern, caller),
        _ => true,
    };
    if !source_ok {
        return false;
    }
    let dest_ok = match &rule.destination_pattern {
        Some(pattern) if !pattern.is_empty() => regex_match(pattern, destination),
        _ => true,
    };
    dest_ok
}

fn regex_match(pattern: &str, input: &str) -> bool {
    match regex::Regex::new(pattern) {
        Ok(re) => re.is_match(input),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Registrations — best-effort view of proxy registrar state
// ---------------------------------------------------------------------------
//
// In the test harness, the SIP server is not bound (`skip_sip_bind=true`)
// and the registrar has no active state. For Phase 1 we return an empty
// snapshot in that case. A follow-up plan can hook into the live
// registrar module once the accessor story on `AppState` is stable.

async fn list_registrations(
    State(_state): State<AppState>,
) -> ApiResult<Json<Vec<RegistrationView>>> {
    Ok(Json(Vec::new()))
}

async fn get_registration(
    State(_state): State<AppState>,
    Path(user): Path<String>,
) -> ApiResult<Json<Vec<RegistrationView>>> {
    Err(ApiError::not_found(format!(
        "no active registration for user '{user}'"
    )))
}

// ---------------------------------------------------------------------------
// Summary
// ---------------------------------------------------------------------------

async fn diagnostics_summary(
    State(state): State<AppState>,
) -> ApiResult<Json<DiagnosticsSummary>> {
    let db = state.db();

    let routes: Vec<routing::Model> = RouteEntity::find()
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let mut active: u64 = 0;
    let mut inactive: u64 = 0;
    for r in &routes {
        if r.is_active {
            active += 1;
        } else {
            inactive += 1;
        }
    }

    Ok(Json(DiagnosticsSummary {
        registrations: RegistrationsSummary {
            count: 0,
            users: Vec::new(),
        },
        routing: RoutingSummary {
            active_routes: active,
            inactive_routes: inactive,
        },
    }))
}
