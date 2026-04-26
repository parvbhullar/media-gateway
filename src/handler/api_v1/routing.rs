//! `/api/v1/routing/resolve` — RTE-03 dry-run route resolution.
//!
//! Phase 3 Plan 03-05. Reuses `src/proxy/routing/matcher.rs::match_invite_with_trace`
//! per D-13 — by construction, dry-run cannot drift from production dispatch.
//! Mounted under the protected (Bearer-auth) router per D-16. Uses
//! RoutingState::new_with_db(Some(db.clone())) per D-17.
//!
//! Routes and trunk config are read from the live AppState data_context
//! snapshot (same source production dispatch uses). Routes are in
//! `state.sip_server().inner.data_context.routes_snapshot()`; trunks in
//! `state.sip_server().inner.data_context.trunks_snapshot()`.
//!
//! NOT in this plan: /routing/tables CRUD (Phase 6, RTE-01..05).
//! Phase 6 should populate `matched_table` and `matched_record_index` in the
//! response (currently always None).

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    routing::post,
};
use rsipstack::dialog::invitation::InviteOption;
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::call::{DialDirection, RoutingState};
use crate::config::RouteResult;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::proxy::routing::matcher::{RouteTrace, match_invite_with_trace};

// ── Wire types (D-14, D-15) ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRouteRequest {
    pub caller_number: String,
    pub destination_number: String,
    #[serde(default)]
    pub src_ip: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize)]
pub struct ResolveTarget {
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ResolveRouteResponse {
    pub result: String,
    pub matched_table: Option<String>,
    pub matched_record_index: Option<i32>,
    /// Phase 6 Plan 06-01 — D-30 plumbing. Wave 3 (06-04) populates this
    /// with the matched record's UUIDv4 from `supersip_routing_tables`.
    /// Always `None` until 06-04 lands.
    pub matched_record_id: Option<String>,
    pub match_reason: Option<String>,
    pub target: Option<ResolveTarget>,
    pub selected_gateway: Option<String>,
    pub trace: Vec<serde_json::Value>,
}

// ── Router ───────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/routing/resolve", post(resolve_route))
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn build_invite_option(req: &ResolveRouteRequest) -> ApiResult<InviteOption> {
    let caller = format!("sip:{}@dryrun.local", req.caller_number);
    let callee = format!("sip:{}@dryrun.local", req.destination_number);
    Ok(InviteOption {
        caller: caller.as_str().try_into().map_err(|e| {
            ApiError::bad_request(format!("invalid caller_number: {:?}", e))
        })?,
        callee: callee.as_str().try_into().map_err(|e| {
            ApiError::bad_request(format!("invalid destination_number: {:?}", e))
        })?,
        contact: "sip:dryrun@127.0.0.1:5060".try_into().unwrap(),
        ..Default::default()
    })
}

fn build_origin_request(req: &ResolveRouteRequest) -> ApiResult<rsipstack::sip::Request> {
    let uri_str = format!("sip:{}@dryrun.local", req.destination_number);
    let uri = uri_str.as_str().try_into().map_err(|e| {
        ApiError::bad_request(format!("invalid destination URI: {:?}", e))
    })?;
    let from_str = format!(
        "Caller <sip:{}@dryrun.local>;tag=dryrun",
        req.caller_number
    );
    let to_str = format!("Callee <sip:{}@dryrun.local>", req.destination_number);
    let mut headers: rsipstack::sip::Headers = vec![
        rsipstack::sip::Header::Via(
            "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKdryrun"
                .try_into()
                .unwrap(),
        ),
        rsipstack::sip::Header::From(from_str.as_str().try_into().map_err(|e| {
            ApiError::bad_request(format!("from header: {:?}", e))
        })?),
        rsipstack::sip::Header::To(to_str.as_str().try_into().map_err(|e| {
            ApiError::bad_request(format!("to header: {:?}", e))
        })?),
        rsipstack::sip::Header::CallId("dryrun-call-id".into()),
        rsipstack::sip::Header::CSeq("1 INVITE".try_into().unwrap()),
        rsipstack::sip::Header::MaxForwards(70.into()),
    ]
    .into();

    if let Some(h) = &req.headers {
        for (k, v) in h {
            headers.push(rsipstack::sip::Header::Other(k.clone(), v.clone()));
        }
    }

    Ok(rsipstack::sip::Request {
        method: rsipstack::sip::Method::Invite,
        uri,
        version: rsipstack::sip::Version::V2,
        headers,
        body: Vec::new(),
    })
}

// ── Handler ──────────────────────────────────────────────────────────────

async fn resolve_route(
    State(state): State<AppState>,
    Json(req): Json<ResolveRouteRequest>,
) -> ApiResult<Json<ResolveRouteResponse>> {
    let db = state.db().clone();

    let invite_option = build_invite_option(&req)?;
    let origin = build_origin_request(&req)?;

    let routing_state = Arc::new(RoutingState::new_with_db(Some(db.clone())));
    let mut trace = RouteTrace::default();

    let data_ctx = &state.sip_server().inner.data_context;
    let trunks_owned = data_ctx.trunks_snapshot();
    let routes_owned = data_ctx.routes_snapshot();

    let result = match_invite_with_trace(
        if trunks_owned.is_empty() {
            None
        } else {
            Some(&trunks_owned)
        },
        if routes_owned.is_empty() {
            None
        } else {
            Some(&routes_owned)
        },
        None,
        invite_option,
        &origin,
        None,
        routing_state,
        &DialDirection::Outbound,
        &mut trace,
    )
    .await
    .map_err(|e| ApiError::internal(format!("dispatch error: {}", e)))?;

    let trace_value = serde_json::to_value(&trace)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let trace_vec = vec![trace_value];

    let response = match result {
        RouteResult::Forward(_option, _hints) => {
            // trunk_group_name is set when the route target resolved through a
            // trunk_group; selected_trunk then holds the picked gateway.
            let (target, selected_gateway) = if let Some(ref tg_name) = trace.trunk_group_name {
                (
                    Some(ResolveTarget {
                        kind: "trunk_group".into(),
                        name: tg_name.clone(),
                    }),
                    trace.selected_trunk.clone(),
                )
            } else if let Some(ref gw_name) = trace.selected_trunk {
                (
                    Some(ResolveTarget {
                        kind: "gateway".into(),
                        name: gw_name.clone(),
                    }),
                    None,
                )
            } else {
                (None, None)
            };
            ResolveRouteResponse {
                result: "matched".into(),
                matched_table: trace.matched_table.clone(),
                matched_record_index: trace.matched_record_index,
                matched_record_id: trace.matched_record_id.clone(),
                match_reason: trace.matched_rule.clone(),
                target,
                selected_gateway,
                trace: trace_vec,
            }
        }
        RouteResult::NotHandled(_, _) => ResolveRouteResponse {
            result: "not_handled".into(),
            matched_table: None,
            matched_record_index: None,
            matched_record_id: None,
            match_reason: None,
            target: None,
            selected_gateway: None,
            trace: trace_vec,
        },
        RouteResult::Abort(status, reason) => ResolveRouteResponse {
            result: "abort".into(),
            matched_table: None,
            matched_record_index: None,
            matched_record_id: None,
            match_reason: reason
                .clone()
                .or_else(|| Some(format!("aborted with status {:?}", status))),
            target: None,
            selected_gateway: None,
            trace: trace_vec,
        },
        RouteResult::Reject { code, reason, .. } => ResolveRouteResponse {
            // Phase 5 Plan 05-04: trunk-enforcement reject (403/488/503).
            result: "reject".into(),
            matched_table: None,
            matched_record_index: None,
            matched_record_id: None,
            match_reason: Some(format!("{} ({})", reason, code)),
            target: None,
            selected_gateway: None,
            trace: trace_vec,
        },
        RouteResult::Queue { .. } => ResolveRouteResponse {
            result: "matched".into(),
            matched_table: None,
            matched_record_index: None,
            matched_record_id: None,
            match_reason: trace.matched_rule.clone(),
            target: trace.selected_trunk.as_ref().map(|n| ResolveTarget {
                kind: "queue".into(),
                name: n.clone(),
            }),
            selected_gateway: None,
            trace: trace_vec,
        },
        RouteResult::Application { app_name, .. } => ResolveRouteResponse {
            result: "matched".into(),
            matched_table: None,
            matched_record_index: None,
            matched_record_id: None,
            match_reason: trace.matched_rule.clone(),
            target: Some(ResolveTarget {
                kind: "application".into(),
                name: app_name,
            }),
            selected_gateway: None,
            trace: trace_vec,
        },
    };

    Ok(Json(response))
}
