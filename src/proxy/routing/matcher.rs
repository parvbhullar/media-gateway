use anyhow::{Result, anyhow};
use async_trait::async_trait;
use regex::Regex;
use rsipstack::{
    dialog::{authenticate::Credential, invitation::InviteOption},
    transport::SipAddr,
};
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::Arc,
};
use tracing::info;

use crate::{
    call::{DialDirection, RoutingState, policy::PolicyCheckStatus},
    config::{DialplanHints, RouteResult},
    proxy::routing::{
        ActionType, RouteQueueConfig, RouteRule, SourceTrunk, TrunkConfig,
        codec_normalize::intersect_codecs,
    },
    proxy::trunk_acl_eval::{AclVerdict, evaluate_acl_rules},
    proxy::trunk_capacity_state::{AcquireOutcome, Permit},
};

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RouteTrace {
    pub matched_rule: Option<String>,
    pub selected_trunk: Option<String>,
    /// Set when the route dest resolved through a trunk_group; holds the
    /// trunk_group name while `selected_trunk` holds the resolved gateway.
    pub trunk_group_name: Option<String>,
    pub used_default_route: bool,
    pub rewrite_operations: Vec<String>,
    pub abort: Option<RouteAbortTrace>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteAbortTrace {
    pub code: u16,
    pub reason: Option<String>,
}

#[async_trait]
pub trait RouteResourceLookup: Send + Sync {
    async fn load_queue(&self, path: &str) -> Result<Option<RouteQueueConfig>>;
}

/// Try to resolve dest_config as a trunk_group name. If the single-name dest
/// is a known trunk_group, delegate to the resolver+dispatch helper. If not,
/// return Ok(None) so the caller falls through to the existing select_trunk
/// path unchanged.
async fn try_select_via_trunk_group(
    db: Option<&sea_orm::DatabaseConnection>,
    dest_config: &crate::proxy::routing::DestConfig,
    option: &InviteOption,
    routing_state: Arc<RoutingState>,
    trunks: Option<&HashMap<String, TrunkConfig>>,
) -> Result<Option<String>> {
    let db = match db {
        Some(d) => d,
        None => return Ok(None),
    };
    let group_name = match dest_config {
        crate::proxy::routing::DestConfig::Single(name) => name.clone(),
        crate::proxy::routing::DestConfig::Multiple(_) => return Ok(None),
    };
    use crate::models::trunk_group::{Column as TgColumn, Entity as TgEntity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let is_group = TgEntity::find()
        .filter(TgColumn::Name.eq(group_name.clone()))
        .one(db)
        .await?
        .is_some();
    if !is_group {
        return Ok(None);
    }
    let selected =
        crate::proxy::routing::trunk_group_resolver::select_gateway_for_trunk_group(
            db,
            &group_name,
            option,
            routing_state,
            trunks,
        )
        .await?;
    Ok(Some(selected))
}

/// Main routing function
///
/// Routes INVITE requests based on configured routing rules and trunk configurations:
/// 1. Match routing rules by priority
/// 2. Apply rewrite rules
/// 3. Select target trunk
/// 4. Set destination, headers and credentials
#[derive(Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    Execute,
    Inspect,
}

pub async fn match_invite(
    trunks: Option<&HashMap<String, TrunkConfig>>,
    routes: Option<&Vec<RouteRule>>,
    resource_lookup: Option<&dyn RouteResourceLookup>,
    option: InviteOption,
    origin: &rsipstack::sip::Request,
    source_trunk: Option<&SourceTrunk>,
    routing_state: Arc<RoutingState>,
    direction: &DialDirection,
) -> Result<RouteResult> {
    match_invite_impl(
        trunks,
        routes,
        resource_lookup,
        option,
        origin,
        source_trunk,
        routing_state,
        direction,
        MatchMode::Execute,
        None,
        Vec::new(),
        None,
    )
    .await
    .map(|(r, _)| r)
}

/// Phase 5 Plan 05-04: extended entry point that accepts a caller-codec list
/// (extracted from SDP by the caller; the matcher does not parse SDP) and a
/// peer IP for per-trunk ACL evaluation. Used by `src/proxy/call.rs`.
///
/// Returns `(RouteResult, Option<Permit>)`. The Permit, when present, must
/// be attached to the registry entry via
/// `ActiveProxyCallRegistry::attach_permit` so that capacity is released
/// when the call ends (RAII drop).
pub async fn match_invite_with_codecs(
    trunks: Option<&HashMap<String, TrunkConfig>>,
    routes: Option<&Vec<RouteRule>>,
    resource_lookup: Option<&dyn RouteResourceLookup>,
    option: InviteOption,
    origin: &rsipstack::sip::Request,
    source_trunk: Option<&SourceTrunk>,
    routing_state: Arc<RoutingState>,
    direction: &DialDirection,
    caller_codecs: Vec<String>,
    peer_ip: Option<std::net::IpAddr>,
) -> Result<(RouteResult, Option<Permit>)> {
    match_invite_impl(
        trunks,
        routes,
        resource_lookup,
        option,
        origin,
        source_trunk,
        routing_state,
        direction,
        MatchMode::Execute,
        None,
        caller_codecs,
        peer_ip,
    )
    .await
}

pub async fn match_invite_with_trace(
    trunks: Option<&HashMap<String, TrunkConfig>>,
    routes: Option<&Vec<RouteRule>>,
    resource_lookup: Option<&dyn RouteResourceLookup>,
    option: InviteOption,
    origin: &rsipstack::sip::Request,
    source_trunk: Option<&SourceTrunk>,
    routing_state: Arc<RoutingState>,
    direction: &DialDirection,
    trace: &mut RouteTrace,
) -> Result<RouteResult> {
    match_invite_impl(
        trunks,
        routes,
        resource_lookup,
        option,
        origin,
        source_trunk,
        routing_state,
        direction,
        MatchMode::Execute,
        Some(trace),
        Vec::new(),
        None,
    )
    .await
    .map(|(r, _)| r)
}

/// Phase 5 Plan 05-04: trace + caller_codecs + peer_ip variant. Returns the
/// optional Permit alongside the RouteResult, mirroring
/// `match_invite_with_codecs`.
pub async fn match_invite_with_trace_and_codecs(
    trunks: Option<&HashMap<String, TrunkConfig>>,
    routes: Option<&Vec<RouteRule>>,
    resource_lookup: Option<&dyn RouteResourceLookup>,
    option: InviteOption,
    origin: &rsipstack::sip::Request,
    source_trunk: Option<&SourceTrunk>,
    routing_state: Arc<RoutingState>,
    direction: &DialDirection,
    trace: &mut RouteTrace,
    caller_codecs: Vec<String>,
    peer_ip: Option<std::net::IpAddr>,
) -> Result<(RouteResult, Option<Permit>)> {
    match_invite_impl(
        trunks,
        routes,
        resource_lookup,
        option,
        origin,
        source_trunk,
        routing_state,
        direction,
        MatchMode::Execute,
        Some(trace),
        caller_codecs,
        peer_ip,
    )
    .await
}

pub async fn inspect_invite(
    trunks: Option<&HashMap<String, TrunkConfig>>,
    routes: Option<&Vec<RouteRule>>,
    resource_lookup: Option<&dyn RouteResourceLookup>,
    option: InviteOption,
    origin: &rsipstack::sip::Request,
    source_trunk: Option<&SourceTrunk>,
    routing_state: Arc<RoutingState>,
    direction: &DialDirection,
) -> Result<RouteResult> {
    match_invite_impl(
        trunks,
        routes,
        resource_lookup,
        option,
        origin,
        source_trunk,
        routing_state,
        direction,
        MatchMode::Inspect,
        None,
        Vec::new(),
        None,
    )
    .await
    .map(|(r, _)| r)
}

/// Phase 5 Plan 05-04: Look up trunk_group_id for `name`, returning None when
/// the DB or row is absent. Errors on DB connection failure.
async fn lookup_trunk_group_id_for_name(
    db: &sea_orm::DatabaseConnection,
    name: &str,
) -> Result<Option<i64>> {
    use crate::models::trunk_group::{Column as TgColumn, Entity as TgEntity};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    Ok(TgEntity::find()
        .filter(TgColumn::Name.eq(name))
        .one(db)
        .await?
        .map(|m| m.id))
}

/// Phase 5 Plan 05-04: Read the per-trunk ACL ruleset (ordered by `position`).
/// Returns an empty Vec when no rows exist (D-14 default-allow path).
async fn load_trunk_acl_rules(
    db: &sea_orm::DatabaseConnection,
    trunk_group_id: i64,
) -> Result<Vec<String>> {
    use crate::models::trunk_acl_entries::{
        Column as AclColumn, Entity as AclEntity,
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
    let rows = AclEntity::find()
        .filter(AclColumn::TrunkGroupId.eq(trunk_group_id))
        .order_by_asc(AclColumn::Position)
        .all(db)
        .await?;
    Ok(rows.into_iter().map(|r| r.rule).collect())
}

/// Phase 5 Plan 05-04: Read max_calls + max_cps for the trunk group
/// (None when no row exists or both fields are NULL → unlimited per D-04).
async fn load_capacity_limits(
    db: &sea_orm::DatabaseConnection,
    trunk_group_id: i64,
) -> Result<(Option<u32>, Option<u32>)> {
    use crate::models::trunk_capacity::{
        Column as CapColumn, Entity as CapEntity,
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    if let Some(row) = CapEntity::find()
        .filter(CapColumn::TrunkGroupId.eq(trunk_group_id))
        .one(db)
        .await?
    {
        let max_calls = row.max_calls.and_then(|v| u32::try_from(v).ok());
        let max_cps = row.max_cps.and_then(|v| u32::try_from(v).ok());
        Ok((max_calls, max_cps))
    } else {
        Ok((None, None))
    }
}

/// Phase 5 Plan 05-04: Apply the three enforcement gates (ACL → capacity →
/// codec) for the resolved trunk group. Returns:
/// - `Ok(Ok(Some(permit)))` — gates passed, permit acquired (move into entry)
/// - `Ok(Ok(None))` — gates passed, no capacity gate (no DB or no group row)
/// - `Ok(Err(reject))` — gate denied; `reject` is a ready `RouteResult::Reject`
/// - `Err(e)` — DB failure
async fn apply_phase5_gates(
    routing_state: &Arc<RoutingState>,
    trunk_group_name: &str,
    caller_codecs: &[String],
    peer_ip: Option<std::net::IpAddr>,
    trunk_codecs: Option<&Vec<String>>,
) -> Result<std::result::Result<Option<Permit>, RouteResult>> {
    let db = match routing_state.db() {
        Some(d) => d,
        None => return Ok(Ok(None)),
    };
    let group_id =
        match lookup_trunk_group_id_for_name(db, trunk_group_name).await? {
            Some(id) => id,
            None => return Ok(Ok(None)),
        };

    // Gate 1: per-trunk ACL (D-15, D-16, D-17 fresh read per INVITE)
    if let Some(ip) = peer_ip {
        let rules = load_trunk_acl_rules(db, group_id).await?;
        if let AclVerdict::Deny = evaluate_acl_rules(&rules, ip) {
            info!(
                trunk = %trunk_group_name,
                peer_ip = %ip,
                "trunk ACL deny → 403"
            );
            return Ok(Err(RouteResult::Reject {
                code: 403,
                reason: "trunk_acl_blocked".to_string(),
                retry_after_secs: None,
            }));
        }
    }

    // Gate 2: capacity (max_calls + max_cps token bucket, D-03 + D-09)
    let permit = if let Some(cap_state) = routing_state.trunk_capacity_state() {
        let (max_calls, max_cps) = load_capacity_limits(db, group_id).await?;
        match cap_state.try_acquire(group_id, max_calls, max_cps) {
            AcquireOutcome::Ok(permit) => Some(permit),
            AcquireOutcome::CallsExhausted => {
                info!(
                    trunk = %trunk_group_name,
                    "trunk capacity exhausted (max_calls) → 503"
                );
                return Ok(Err(RouteResult::Reject {
                    code: 503,
                    reason: "trunk_capacity_exhausted".to_string(),
                    retry_after_secs: Some(5),
                }));
            }
            AcquireOutcome::CpsExhausted => {
                info!(
                    trunk = %trunk_group_name,
                    "trunk CPS exhausted → 503"
                );
                return Ok(Err(RouteResult::Reject {
                    code: 503,
                    reason: "trunk_cps_exhausted".to_string(),
                    retry_after_secs: Some(5),
                }));
            }
        }
    } else {
        None
    };

    // Gate 3: codec intersection (D-18, D-20 empty trunk list = allow-all)
    if let Some(trunk_codec_list) = trunk_codecs {
        if !trunk_codec_list.is_empty() {
            let chosen = intersect_codecs(caller_codecs, trunk_codec_list);
            if chosen.is_empty() {
                info!(
                    trunk = %trunk_group_name,
                    "codec intersection empty → 488"
                );
                drop(permit); // T-05-04-10: release capacity on codec reject
                return Ok(Err(RouteResult::Reject {
                    code: 488,
                    reason: "codec_mismatch_488".to_string(),
                    retry_after_secs: None,
                }));
            }
        }
    }

    Ok(Ok(permit))
}

async fn match_invite_impl(
    trunks: Option<&HashMap<String, TrunkConfig>>,
    routes: Option<&Vec<RouteRule>>,
    resource_lookup: Option<&dyn RouteResourceLookup>,
    option: InviteOption,
    origin: &rsipstack::sip::Request,
    source_trunk: Option<&SourceTrunk>,
    routing_state: Arc<RoutingState>,
    direction: &DialDirection,
    mode: MatchMode,
    mut trace: Option<&mut RouteTrace>,
    caller_codecs: Vec<String>,
    peer_ip: Option<std::net::IpAddr>,
) -> Result<(RouteResult, Option<Permit>)> {
    let mut option = option;
    let routes = match routes {
        Some(routes) => routes,
        None => return Ok((RouteResult::NotHandled(option, None), None)),
    };

    // Extract URI information early to avoid borrowing conflicts
    let caller_user = option.caller.user().unwrap_or_default().to_string();
    let caller_host = option.caller.host().clone();
    let callee_user = option.callee.user().unwrap_or_default().to_string();
    let callee_host = option.callee.host().clone();
    let request_user = origin.uri.user().unwrap_or_default().to_string();
    let request_host = origin.uri.host().clone();

    info!(
        "Matching {:?} caller={}@{}, callee={}@{}, request={}@{}",
        direction, caller_user, caller_host, callee_user, callee_host, request_user, request_host
    );

    // Traverse routing rules by priority
    for rule in routes {
        if let Some(true) = rule.disabled {
            continue;
        }

        if !rule.direction.matches(direction) {
            continue;
        }

        if !rule.source_trunks.is_empty() {
            match source_trunk {
                Some(trunk)
                    if rule
                        .source_trunks
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&trunk.name)) => {}
                Some(_) => continue,
                None => continue,
            }
        }

        if !rule.source_trunk_ids.is_empty() {
            match source_trunk.and_then(|t| t.id) {
                Some(id) if rule.source_trunk_ids.iter().any(|rule_id| *rule_id == id) => {}
                _ => continue,
            }
        }

        // Check matching conditions
        let ctx = MatchContext {
            origin,
            caller_user: &caller_user,
            caller_host: &caller_host,
            callee_user: &callee_user,
            callee_host: &callee_host,
            request_user: &request_user,
            request_host: &request_host,
        };
        let rule_matched = matches_rule(rule, &ctx)?;

        if !rule_matched {
            continue;
        }

        // Resolve source trunk country
        let origin_country = if let Some(source) = source_trunk {
            trunks
                .and_then(|t| t.get(&source.name))
                .and_then(|c| c.country.as_deref())
        } else {
            None
        };

        let captures = collect_match_captures(rule, &ctx)?;

        if let Some(trace) = &mut trace {
            trace.matched_rule = Some(rule.name.clone());
        }

        // Apply rewrite rules
        let rewrites = if let Some(rewrite) = &rule.rewrite {
            if let Some(trace) = &mut trace {
                trace
                    .rewrite_operations
                    .extend(describe_rewrite_ops(rewrite));
            }
            apply_rewrite_rules(&mut option, rewrite, origin, &captures)?
        } else {
            HashMap::new()
        };

        info!(
            "Matched rule: {:?} action:{:?} rewrites:{:?}",
            rule.name, rule.action, rewrites
        );

        // Check Route Policy (using rewritten numbers)
        if let Some(policy) = &rule.policy {
            if let Some(guard) = &routing_state.policy_guard {
                let current_caller = option.caller.user().unwrap_or_default();
                let current_callee = option.callee.user().unwrap_or_default();

                if let PolicyCheckStatus::Rejected(rejection) = guard
                    .check_policy(
                        &rule.name,
                        policy,
                        &current_caller,
                        &current_callee,
                        origin_country,
                    )
                    .await?
                {
                    let reason = rejection.to_string();
                    info!(
                        "Call rejected by route policy: {} reason: {}",
                        rule.name, reason
                    );
                    if let Some(trace) = &mut trace {
                        trace.abort = Some(RouteAbortTrace {
                            code: rsipstack::sip::StatusCode::Forbidden.into(),
                            reason: Some(reason.clone()),
                        });
                    }
                    return Ok((
                        RouteResult::Abort(
                            rsipstack::sip::StatusCode::Forbidden,
                            Some(reason),
                        ),
                        None,
                    ));
                }
            }
        }

        let hints = if !rule.codecs.is_empty() {
            let mut hints = DialplanHints::default();
            hints.allow_codecs = Some(rule.codecs.clone());
            Some(hints)
        } else {
            None
        };

        // Handle based on action type
        match rule.action.get_action_type() {
            ActionType::Reject => {
                if let Some(reject_config) = &rule.action.reject {
                    let reason = reject_config.reason.clone();
                    info!(
                        "Rejecting call with code {} and reason: {:?}",
                        reject_config.code, reason
                    );
                    if let Some(trace) = &mut trace {
                        trace.abort = Some(RouteAbortTrace {
                            code: reject_config.code,
                            reason: reason.clone(),
                        });
                    }
                    return Ok((
                        RouteResult::Abort(reject_config.code.into(), reason),
                        None,
                    ));
                } else {
                    if let Some(trace) = &mut trace {
                        trace.abort = Some(RouteAbortTrace {
                            code: rsipstack::sip::StatusCode::Forbidden.into(),
                            reason: None,
                        });
                    }
                    return Ok((
                        RouteResult::Abort(rsipstack::sip::StatusCode::Forbidden, None),
                        None,
                    ));
                }
            }
            ActionType::Busy => {
                if let Some(trace) = &mut trace {
                    trace.abort = Some(RouteAbortTrace {
                        code: rsipstack::sip::StatusCode::BusyHere.into(),
                        reason: None,
                    });
                }
                return Ok((
                    RouteResult::Abort(rsipstack::sip::StatusCode::BusyHere, None),
                    None,
                ));
            }
            ActionType::Forward => {
                let mut phase5_permit: Option<Permit> = None;
                if let Some(dest_config) = &rule.action.dest {
                    if mode == MatchMode::Execute {
                        let (selected_trunk, via_trunk_group) =
                            match try_select_via_trunk_group(
                                routing_state.db(),
                                dest_config,
                                &option,
                                routing_state.clone(),
                                trunks,
                            )
                            .await?
                            {
                                Some(gateway) => {
                                    let tg_name = if let crate::proxy::routing::DestConfig::Single(n) = dest_config {
                                        Some(n.clone())
                                    } else {
                                        None
                                    };
                                    (gateway, tg_name)
                                }
                                None => (
                                    select_trunk(
                                        dest_config,
                                        &rule.action.select,
                                        &rule.action.hash_key,
                                        &option,
                                        routing_state.clone(),
                                        trunks,
                                    )?,
                                    None,
                                ),
                            };

                        if let Some(trace) = &mut trace {
                            trace.selected_trunk = Some(selected_trunk.clone());
                            trace.trunk_group_name = via_trunk_group.clone();
                        }

                        if let Some(trunk_config) = trunks
                            .as_ref()
                            .and_then(|trunks| trunks.get(&selected_trunk))
                        {
                            // Check Trunk Policy
                            if let Some(policy) = &trunk_config.policy {
                                if let Some(guard) = &routing_state.policy_guard {
                                    let current_caller = option.caller.user().unwrap_or_default();
                                    let current_callee = option.callee.user().unwrap_or_default();

                                    if let PolicyCheckStatus::Rejected(rejection) = guard
                                        .check_policy(
                                            &format!("trunk:{}", selected_trunk),
                                            policy,
                                            &current_caller,
                                            &current_callee,
                                            origin_country,
                                        )
                                        .await?
                                    {
                                        let reason = rejection.to_string();
                                        info!(
                                            "Call rejected by trunk policy: {} reason: {}",
                                            selected_trunk, reason
                                        );
                                        if let Some(trace) = &mut trace {
                                            trace.abort = Some(RouteAbortTrace {
                                                code: rsipstack::sip::StatusCode::Forbidden.into(),
                                                reason: Some(reason.clone()),
                                            });
                                        }
                                        return Ok((
                                            RouteResult::Abort(
                                                rsipstack::sip::StatusCode::Forbidden,
                                                Some(reason),
                                            ),
                                            None,
                                        ));
                                    }
                                }
                            }

                            apply_trunk_config(&mut option, trunk_config)?;
                            info!(
                                "Selected trunk: {} for destination: {}",
                                selected_trunk, trunk_config.dest
                            );
                        } else {
                            info!("Trunk '{}' not found in configuration", selected_trunk);
                        }

                        // Phase 5 Plan 05-04: enforcement gates (ACL → capacity → codec)
                        if let Some(group_name) = &via_trunk_group {
                            let trunk_codecs = hints
                                .as_ref()
                                .and_then(|h| h.allow_codecs.as_ref());
                            match apply_phase5_gates(
                                &routing_state,
                                group_name,
                                &caller_codecs,
                                peer_ip,
                                trunk_codecs,
                            )
                            .await?
                            {
                                Ok(permit) => phase5_permit = permit,
                                Err(reject) => {
                                    if let Some(trace) = &mut trace {
                                        if let RouteResult::Reject {
                                            code, reason, ..
                                        } = &reject
                                        {
                                            trace.abort = Some(RouteAbortTrace {
                                                code: *code,
                                                reason: Some(reason.clone()),
                                            });
                                        }
                                    }
                                    return Ok((reject, None));
                                }
                            }
                        }
                    }
                }
                return Ok((RouteResult::Forward(option, hints), phase5_permit));
            }
            ActionType::Queue => {
                let queue_ref = rule
                    .action
                    .queue
                    .as_ref()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow!("queue action requires a 'queue' reference"))?;

                let lookup = resource_lookup.ok_or_else(|| {
                    anyhow!(
                        "queue action cannot resolve '{}' without resource lookup",
                        queue_ref
                    )
                })?;

                // Try to resolve as ID if it looks like one (e.g. "123")
                // Or if it is prefixed with "queue:" which is stripped before calling this?
                // Actually, `rule.action.queue` comes from the route rule config.
                // If it's a file path, `load_queue` handles it.
                // If it's an ID, we need to handle it.

                // If the reference is just digits, treat it as a DB ID reference "db-<id>"
                let lookup_ref = if queue_ref.chars().all(|c| c.is_ascii_digit()) {
                    format!("db-{}", queue_ref)
                } else {
                    queue_ref.clone()
                };

                let queue_cfg = lookup
                    .load_queue(lookup_ref.as_str())
                    .await?
                    .ok_or_else(|| anyhow!("queue '{}' not found", queue_ref))?;
                let mut queue_plan = queue_cfg.to_queue_plan()?;
                if queue_plan.label.is_none() {
                    queue_plan.label = Some(queue_ref.clone());
                }
                let needs_trunk = queue_plan.dial_strategy.is_none();
                let mut phase5_permit_q: Option<Permit> = None;
                if needs_trunk {
                    let dest_config = rule.action.dest.as_ref().ok_or_else(|| {
                        anyhow!("queue action requires 'dest' or inline queue targets")
                    })?;

                    if mode == MatchMode::Execute {
                        let (selected_trunk, via_trunk_group) =
                            match try_select_via_trunk_group(
                                routing_state.db(),
                                dest_config,
                                &option,
                                routing_state.clone(),
                                trunks,
                            )
                            .await?
                            {
                                Some(gateway) => {
                                    let tg_name = if let crate::proxy::routing::DestConfig::Single(n) = dest_config {
                                        Some(n.clone())
                                    } else {
                                        None
                                    };
                                    (gateway, tg_name)
                                }
                                None => (
                                    select_trunk(
                                        dest_config,
                                        &rule.action.select,
                                        &rule.action.hash_key,
                                        &option,
                                        routing_state.clone(),
                                        trunks,
                                    )?,
                                    None,
                                ),
                            };

                        if let Some(trace) = &mut trace {
                            trace.selected_trunk = Some(selected_trunk.clone());
                            trace.trunk_group_name = via_trunk_group.clone();
                        }

                        if let Some(trunk_config) = trunks
                            .as_ref()
                            .and_then(|trunks| trunks.get(&selected_trunk))
                        {
                            // Check Trunk Policy
                            if let Some(policy) = &trunk_config.policy {
                                if let Some(guard) = &routing_state.policy_guard {
                                    let current_caller = option.caller.user().unwrap_or_default();
                                    let current_callee = option.callee.user().unwrap_or_default();

                                    if let PolicyCheckStatus::Rejected(rejection) = guard
                                        .check_policy(
                                            &format!("trunk:{}", selected_trunk),
                                            policy,
                                            &current_caller,
                                            &current_callee,
                                            origin_country,
                                        )
                                        .await?
                                    {
                                        let reason = rejection.to_string();
                                        info!(
                                            "Call rejected by trunk policy: {} reason: {}",
                                            selected_trunk, reason
                                        );
                                        if let Some(trace) = &mut trace {
                                            trace.abort = Some(RouteAbortTrace {
                                                code: rsipstack::sip::StatusCode::Forbidden.into(),
                                                reason: Some(reason.clone()),
                                            });
                                        }
                                        return Ok((
                                            RouteResult::Abort(
                                                rsipstack::sip::StatusCode::Forbidden,
                                                Some(reason),
                                            ),
                                            None,
                                        ));
                                    }
                                }
                            }
                            apply_trunk_config(&mut option, trunk_config)?;
                        }

                        // Phase 5 Plan 05-04: enforcement gates (ACL → capacity → codec)
                        if let Some(group_name) = &via_trunk_group {
                            let trunk_codecs = hints
                                .as_ref()
                                .and_then(|h| h.allow_codecs.as_ref());
                            match apply_phase5_gates(
                                &routing_state,
                                group_name,
                                &caller_codecs,
                                peer_ip,
                                trunk_codecs,
                            )
                            .await?
                            {
                                Ok(permit) => phase5_permit_q = permit,
                                Err(reject) => {
                                    if let Some(trace) = &mut trace {
                                        if let RouteResult::Reject {
                                            code, reason, ..
                                        } = &reject
                                        {
                                            trace.abort = Some(RouteAbortTrace {
                                                code: *code,
                                                reason: Some(reason.clone()),
                                            });
                                        }
                                    }
                                    return Ok((reject, None));
                                }
                            }
                        }
                    }
                }

                return Ok((
                    RouteResult::Queue {
                        option,
                        queue: queue_plan,
                        hints,
                    },
                    phase5_permit_q,
                ));
            }
            ActionType::Application => {
                let app_name = rule
                    .action
                    .app
                    .as_ref()
                    .ok_or_else(|| anyhow!("application action requires 'app' field"))?;

                return Ok((
                    RouteResult::Application {
                        option,
                        app_name: app_name.clone(),
                        app_params: rule.action.app_params.clone(),
                        auto_answer: rule.action.auto_answer,
                    },
                    None,
                ));
            }
        }
    }

    return Ok((RouteResult::NotHandled(option, None), None));
}

/// Context for rule matching to reduce function arguments
struct MatchContext<'a> {
    origin: &'a rsipstack::sip::Request,
    caller_user: &'a str,
    caller_host: &'a rsipstack::sip::Host,
    callee_user: &'a str,
    callee_host: &'a rsipstack::sip::Host,
    request_user: &'a str,
    request_host: &'a rsipstack::sip::Host,
}

/// Check if routing rule matches
fn matches_rule(rule: &crate::proxy::routing::RouteRule, ctx: &MatchContext) -> Result<bool> {
    let conditions = &rule.match_conditions;

    // Check from.user
    if let Some(pattern) = &conditions.from_user {
        if !matches_pattern(pattern, ctx.caller_user)? {
            return Ok(false);
        }
    }

    // Check from.host
    if let Some(pattern) = &conditions.from_host {
        if !matches_pattern(pattern, &ctx.caller_host.to_string())? {
            return Ok(false);
        }
    }

    // Check to.user
    if let Some(pattern) = &conditions.to_user {
        if !matches_pattern(pattern, ctx.callee_user)? {
            return Ok(false);
        }
    }

    // Check to.host
    if let Some(pattern) = &conditions.to_host {
        if !matches_pattern(pattern, &ctx.callee_host.to_string())? {
            return Ok(false);
        }
    }

    // Check request_uri.user
    if let Some(pattern) = &conditions.request_uri_user {
        if !matches_pattern(pattern, ctx.request_user)? {
            return Ok(false);
        }
    }

    // Check request_uri.host
    if let Some(pattern) = &conditions.request_uri_host {
        if !matches_pattern(pattern, &ctx.request_host.to_string())? {
            return Ok(false);
        }
    }

    // Check compatibility fields
    if let Some(pattern) = &conditions.caller {
        let caller_full = format!("{}@{}", ctx.caller_user, ctx.caller_host);
        if !matches_pattern(pattern, &caller_full)? {
            return Ok(false);
        }
    }

    if let Some(pattern) = &conditions.callee {
        let callee_full = format!("{}@{}", ctx.callee_user, ctx.callee_host);
        if !matches_pattern(pattern, &callee_full)? {
            return Ok(false);
        }
    }

    // Check headers
    for (header_key, pattern) in &conditions.headers {
        if let Some(header_name) = header_key.strip_prefix("header.") {
            // Remove "header." prefix
            if let Some(header_value) = get_header_value(ctx.origin, header_name) {
                if !matches_pattern(pattern, &header_value)? {
                    return Ok(false);
                }
            } else {
                return Ok(false); // header not exist
            }
        }
    }

    Ok(true)
}

/// Collect capture groups from matched conditions to support rewrite templates
fn collect_match_captures(
    rule: &crate::proxy::routing::RouteRule,
    ctx: &MatchContext,
) -> Result<HashMap<String, Vec<String>>> {
    let mut captures = HashMap::new();
    let conditions = &rule.match_conditions;

    collect_field_capture(
        &mut captures,
        "from.user",
        conditions.from_user.as_deref(),
        ctx.caller_user,
    )?;

    let caller_host = ctx.caller_host.to_string();
    collect_field_capture(
        &mut captures,
        "from.host",
        conditions.from_host.as_deref(),
        &caller_host,
    )?;

    collect_field_capture(
        &mut captures,
        "to.user",
        conditions.to_user.as_deref(),
        ctx.callee_user,
    )?;

    let callee_host = ctx.callee_host.to_string();
    collect_field_capture(
        &mut captures,
        "to.host",
        conditions.to_host.as_deref(),
        &callee_host,
    )?;

    collect_field_capture(
        &mut captures,
        "request_uri.user",
        conditions.request_uri_user.as_deref(),
        ctx.request_user,
    )?;

    let request_host = ctx.request_host.to_string();
    collect_field_capture(
        &mut captures,
        "request_uri.host",
        conditions.request_uri_host.as_deref(),
        &request_host,
    )?;

    if let Some(pattern) = &conditions.caller {
        let caller_full = format!("{}@{}", ctx.caller_user, ctx.caller_host);
        collect_field_capture(
            &mut captures,
            "caller",
            Some(pattern.as_str()),
            &caller_full,
        )?;
    }

    if let Some(pattern) = &conditions.callee {
        let callee_full = format!("{}@{}", ctx.callee_user, ctx.callee_host);
        collect_field_capture(
            &mut captures,
            "callee",
            Some(pattern.as_str()),
            &callee_full,
        )?;
    }

    for (header_key, pattern) in &conditions.headers {
        if let Some(header_name) = header_key.strip_prefix("header.") {
            if let Some(value) = get_header_value(ctx.origin, header_name) {
                collect_field_capture(&mut captures, header_key, Some(pattern.as_str()), &value)?;
            }
        }
    }

    Ok(captures)
}

fn collect_field_capture(
    captures: &mut HashMap<String, Vec<String>>,
    key: &str,
    pattern: Option<&str>,
    value: &str,
) -> Result<()> {
    if let Some(pattern) = pattern {
        if let Some(groups) = extract_regex_captures(pattern, value)? {
            captures.insert(key.to_string(), groups);
        }
    }
    Ok(())
}

fn extract_regex_captures(pattern: &str, value: &str) -> Result<Option<Vec<String>>> {
    if pattern.is_empty() {
        return Ok(None);
    }

    // Compile pattern as regex to obtain capture groups
    let regex =
        Regex::new(pattern).map_err(|e| anyhow!("Invalid regex pattern '{}': {}", pattern, e))?;
    if let Some(captures) = regex.captures(value) {
        let mut groups = Vec::new();
        for index in 0..captures.len() {
            groups.push(
                captures
                    .get(index)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default(),
            );
        }
        return Ok(Some(groups));
    }

    Ok(None)
}

/// Match pattern (supports regex)
fn matches_pattern(pattern: &str, value: &str) -> Result<bool> {
    // If pattern doesn't contain regex special characters, use exact match
    if !pattern.contains('^')
        && !pattern.contains('$')
        && !pattern.contains('*')
        && !pattern.contains('+')
        && !pattern.contains('?')
        && !pattern.contains('[')
        && !pattern.contains('(')
        && !pattern.contains('\\')
    {
        return Ok(pattern == value);
    }

    // Use regex matching
    let regex =
        Regex::new(pattern).map_err(|e| anyhow!("Invalid regex pattern '{}': {}", pattern, e))?;
    Ok(regex.is_match(value))
}

/// Get header value
fn get_header_value(request: &rsipstack::sip::Request, header_name: &str) -> Option<String> {
    for header in request.headers.iter() {
        match header {
            rsipstack::sip::Header::Other(name, value)
                if name.to_lowercase() == header_name.to_lowercase() =>
            {
                return Some(value.clone());
            }
            rsipstack::sip::Header::UserAgent(value) if header_name.to_lowercase() == "user-agent" => {
                return Some(value.to_string());
            }
            rsipstack::sip::Header::Contact(contact) if header_name.to_lowercase() == "contact" => {
                return Some(contact.to_string());
            }
            // Add other standard header handling
            _ => continue,
        }
    }
    None
}

/// Apply rewrite rules
fn apply_rewrite_rules(
    option: &mut InviteOption,
    rewrite: &crate::proxy::routing::RewriteRules,
    origin: &rsipstack::sip::Request,
    captures: &HashMap<String, Vec<String>>,
) -> Result<HashMap<String, String>> {
    let mut rewrites = HashMap::new();

    // Rewrite caller
    if let Some(pattern) = &rewrite.from_user {
        let new_user = apply_rewrite_pattern_with_match(
            pattern,
            option.caller.user().unwrap_or_default(),
            captures.get("from.user"),
        )?;
        option.caller = update_uri_user(&option.caller, &new_user)?;
        rewrites.insert("from.user".to_string(), new_user);
    }

    if let Some(pattern) = &rewrite.from_host {
        let current_host = option.caller.host().to_string();
        let new_host =
            apply_rewrite_pattern_with_match(pattern, &current_host, captures.get("from.host"))?;
        option.caller = update_uri_host(&option.caller, &new_host)?;
        rewrites.insert("from.host".to_string(), new_host);
    }

    // Rewrite callee
    if let Some(pattern) = &rewrite.to_user {
        let new_user = apply_rewrite_pattern_with_match(
            pattern,
            option.callee.user().unwrap_or_default(),
            captures.get("to.user"),
        )?;
        option.callee = update_uri_user(&option.callee, &new_user)?;
        rewrites.insert("to.user".to_string(), new_user);
    }

    if let Some(pattern) = &rewrite.to_host {
        let current_host = option.callee.host().to_string();
        let new_host =
            apply_rewrite_pattern_with_match(pattern, &current_host, captures.get("to.host"))?;
        option.callee = update_uri_host(&option.callee, &new_host)?;
        rewrites.insert("to.host".to_string(), new_host);
    }

    // Add or modify headers
    for (header_key, pattern) in &rewrite.headers {
        if let Some(header_name) = header_key.strip_prefix("header.") {
            let new_value = apply_rewrite_pattern(pattern, "", origin)?;

            let new_header = rsipstack::sip::Header::Other(header_name.to_string(), new_value);

            if option.headers.is_none() {
                option.headers = Some(Vec::new());
            }
            option.headers.as_mut().unwrap().push(new_header);
        }
    }

    Ok(rewrites)
}

fn describe_rewrite_ops(rewrite: &crate::proxy::routing::RewriteRules) -> Vec<String> {
    let mut ops = Vec::new();

    let mut push = |label: &str, value: &Option<String>| {
        if value.as_ref().map(|v| !v.is_empty()).unwrap_or(false) {
            ops.push(label.to_string());
        }
    };

    push("from.user", &rewrite.from_user);
    push("from.host", &rewrite.from_host);
    push("to.user", &rewrite.to_user);
    push("to.host", &rewrite.to_host);
    push("to.port", &rewrite.to_port);
    push("request_uri.user", &rewrite.request_uri_user);
    push("request_uri.host", &rewrite.request_uri_host);
    push("request_uri.port", &rewrite.request_uri_port);

    for header in rewrite.headers.keys() {
        ops.push(header.to_string());
    }

    ops
}

/// Apply rewrite pattern (supports capture groups)
fn apply_rewrite_pattern_with_match(
    pattern: &str,
    original: &str,
    capture_groups: Option<&Vec<String>>,
) -> Result<String> {
    if !pattern.contains('{') {
        return Ok(pattern.to_string());
    }

    let mut result = String::new();
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' {
            let mut index_buffer = String::new();
            let mut found_closing = false;

            while let Some(&next) = chars.peek() {
                chars.next();
                if next == '}' {
                    found_closing = true;
                    break;
                }
                index_buffer.push(next);
            }

            if !found_closing {
                return Err(anyhow!(
                    "Unclosed capture group placeholder in rewrite pattern '{}'",
                    pattern
                ));
            }

            if index_buffer.is_empty() {
                return Err(anyhow!(
                    "Empty capture group placeholder in rewrite pattern '{}'",
                    pattern
                ));
            }

            let index_value = index_buffer.parse::<usize>().map_err(|e| {
                anyhow!(
                    "Invalid capture group index '{}' in rewrite pattern '{}': {}",
                    index_buffer,
                    pattern,
                    e
                )
            })?;

            let replacement = capture_groups
                .and_then(|groups| groups.get(index_value).cloned())
                .or_else(|| {
                    if index_value == 0 {
                        Some(original.to_string())
                    } else {
                        extract_capture_group(original, index_value)
                    }
                })
                .unwrap_or_else(|| original.to_string());

            result.push_str(&replacement);
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

/// Extract capture group from common patterns
fn extract_capture_group(original: &str, group_num: usize) -> Option<String> {
    if group_num == 0 {
        return Some(original.to_string());
    }

    // Common regex patterns we support
    let patterns = [
        // (\d+) - any digits
        (r"^(\d+)$", vec![0]), // Group 1 is the entire string if all digits
        // prefix(\d+)suffix
        (r"^[^\d]*(\d+)[^\d]*$", vec![]), // Will be computed dynamically
    ];

    for (pattern_str, positions) in &patterns {
        if let Ok(regex) = Regex::new(pattern_str) {
            if let Some(captures) = regex.captures(original) {
                if group_num <= captures.len() && group_num > 0 {
                    if let Some(capture) = captures.get(group_num) {
                        return Some(capture.as_str().to_string());
                    }
                }
                // Fallback for simple position-based extraction
                if !positions.is_empty() && group_num == 1 {
                    let start_pos = positions[0];
                    if original.len() > start_pos {
                        // Extract digits from this position onward
                        let substr = &original[start_pos..];
                        let digits: String =
                            substr.chars().take_while(|c| c.is_ascii_digit()).collect();
                        if !digits.is_empty() {
                            return Some(digits);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Apply rewrite pattern
fn apply_rewrite_pattern(pattern: &str, original: &str, _origin: &rsipstack::sip::Request) -> Result<String> {
    // Support simple replacement patterns like "96123{1}" where {1} is capture group
    if pattern.contains('{') && pattern.contains('}') {
        // This is a pattern with capture groups, need to extract from original value
        // Simplified implementation: assume pattern is "prefix{1}suffix" format
        let start = pattern.find('{').unwrap();
        let end = pattern.find('}').unwrap();
        let prefix = &pattern[..start];
        let suffix = &pattern[end + 1..];
        let _group_num: usize = pattern[start + 1..end].parse().unwrap_or(1);

        // Should use previously matched capture groups here, simplified implementation returns original value
        Ok(format!("{}{}{}", prefix, original, suffix))
    } else {
        // Direct replacement
        Ok(pattern.to_string())
    }
}

/// Update URI user part
fn update_uri_user(uri: &rsipstack::sip::Uri, new_user: &str) -> Result<rsipstack::sip::Uri> {
    let mut new_uri = uri.clone();
    new_uri.auth = Some(rsipstack::sip::Auth {
        user: new_user.to_string(),
        password: uri.auth.as_ref().and_then(|a| a.password.clone()),
    });
    Ok(new_uri)
}

/// Update URI host part
fn update_uri_host(uri: &rsipstack::sip::Uri, new_host: &str) -> Result<rsipstack::sip::Uri> {
    let mut new_uri = uri.clone();
    new_uri.host_with_port = new_host
        .try_into()
        .map_err(|e| anyhow!("Invalid host '{}': {:?}", new_host, e))?;
    Ok(new_uri)
}

/// Select trunk
pub(crate) fn select_trunk(
    dest_config: &crate::proxy::routing::DestConfig,
    select_method: &str,
    hash_key: &Option<String>,
    option: &InviteOption,
    routing_state: Arc<RoutingState>,
    trunks_config: Option<&std::collections::HashMap<String, crate::proxy::routing::TrunkConfig>>,
) -> Result<String> {
    let trunks = match dest_config {
        crate::proxy::routing::DestConfig::Single(trunk) => vec![trunk.clone()],
        crate::proxy::routing::DestConfig::Multiple(trunk_list) => trunk_list.clone(),
    };

    if trunks.is_empty() {
        return Err(anyhow!("No trunks configured"));
    }

    if trunks.len() == 1 {
        return Ok(trunks[0].clone());
    }

    match select_method {
        "random" => {
            use rand::RngExt;
            let index = rand::rng().random_range(0..trunks.len());
            Ok(trunks[index].clone())
        }
        "hash" => {
            let hash_value = if let Some(key) = hash_key {
                match key.as_str() {
                    "from.user" => option.caller.user().unwrap_or_default().to_string(),
                    "to.user" => option.callee.user().unwrap_or_default().to_string(),
                    "call-id" => "default".to_string(), // Simplified implementation
                    _ => key.clone(),
                }
            } else {
                option.caller.to_string()
            };

            let mut hasher = DefaultHasher::new();
            hash_value.hash(&mut hasher);
            let index = (hasher.finish() as usize) % trunks.len();
            Ok(trunks[index].clone())
        }
        "rr" => {
            // Real round-robin implementation with state
            let destination_key = format!("{:?}", dest_config);
            let index = routing_state.next_round_robin_index(&destination_key, trunks.len());
            Ok(trunks[index].clone())
        }
        "weighted" => {
            // Weighted random selection based on trunk weights
            select_trunk_weighted(&trunks, trunks_config)
        }
        _ => {
            // Default to round-robin for unknown selection methods
            let destination_key = format!("{:?}", dest_config);
            let index = routing_state.next_round_robin_index(&destination_key, trunks.len());
            Ok(trunks[index].clone())
        }
    }
}

/// Weighted random trunk selection
/// Uses trunk weight configuration (default: 100 if not specified)
fn select_trunk_weighted(
    trunks: &[String],
    trunks_config: Option<&std::collections::HashMap<String, crate::proxy::routing::TrunkConfig>>,
) -> Result<String> {
    use rand::RngExt;
    
    if trunks.is_empty() {
        return Err(anyhow!("No trunks for weighted selection"));
    }
    
    if trunks.len() == 1 {
        return Ok(trunks[0].clone());
    }
    
    // Collect weights for each trunk
    let mut weights: Vec<u32> = Vec::with_capacity(trunks.len());
    let mut total_weight: u32 = 0;
    
    for trunk_name in trunks {
        let weight = trunks_config
            .and_then(|configs| configs.get(trunk_name))
            .and_then(|config| config.weight)
            .unwrap_or(100); // Default weight: 100
        
        weights.push(weight);
        total_weight = total_weight.saturating_add(weight);
    }
    
    if total_weight == 0 {
        // Fall back to uniform random if all weights are 0
        let index = rand::rng().random_range(0..trunks.len());
        return Ok(trunks[index].clone());
    }
    
    // Generate random value between 0 and total_weight
    let mut rng = rand::rng();
    let random_val = rng.random_range(0..total_weight);
    
    // Find the trunk corresponding to the random value
    let mut cumulative_weight: u32 = 0;
    for (idx, weight) in weights.iter().enumerate() {
        cumulative_weight = cumulative_weight.saturating_add(*weight);
        if random_val < cumulative_weight {
            return Ok(trunks[idx].clone());
        }
    }
    
    // Fallback to last trunk (shouldn't reach here)
    Ok(trunks[trunks.len() - 1].clone())
}

/// Apply trunk configuration
pub(crate) fn apply_trunk_config(option: &mut InviteOption, trunk: &TrunkConfig) -> Result<()> {
    // Set destination
    let dest_uri: rsipstack::sip::Uri = trunk
        .dest
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("Invalid trunk destination '{}': {:?}", trunk.dest, e))?;

    let transport = if let Some(transport_str) = &trunk.transport {
        match transport_str.to_lowercase().as_str() {
            "udp" => Some(rsipstack::sip::transport::Transport::Udp),
            "tcp" => Some(rsipstack::sip::transport::Transport::Tcp),
            "tls" => Some(rsipstack::sip::transport::Transport::Tls),
            "ws" => Some(rsipstack::sip::transport::Transport::Ws),
            "wss" => Some(rsipstack::sip::transport::Transport::Wss),
            _ => None,
        }
    } else {
        None
    };

    option.destination = Some(SipAddr {
        r#type: transport,
        addr: dest_uri.host_with_port.clone(),
    });

    // Save original caller before potential rewrite for P-Asserted-Identity header
    let original_caller = option.caller.clone();

    if trunk.rewrite_hostport {
        option.callee.host_with_port = dest_uri.host_with_port.clone();
        option.caller.host_with_port = dest_uri.host_with_port.clone();
    }

    // Set authentication info
    if let (Some(username), Some(password)) = (&trunk.username, &trunk.password) {
        option.credential = Some(Credential {
            username: username.clone(),
            password: password.clone(),
            realm: dest_uri.host().to_string().into(),
        });
    }

    // Add trunk related headers
    if option.headers.is_none() {
        option.headers = Some(Vec::new());
    }

    let headers = option.headers.as_mut().unwrap();

    // Add P-Asserted-Identity header (using original caller, not rewritten)
    if trunk.username.is_some() {
        let pai_header = rsipstack::sip::Header::Other(
            "P-Asserted-Identity".to_string(),
            format!("<{}>", original_caller),
        );
        headers.push(pai_header);
    }

    Ok(())
}

// ─── DID-first inbound resolution ───────────────────────────────────────────
use crate::proxy::routing::did_index::DidIndex;

/// Outcome of a DID-first lookup against a [`DidIndex`].
#[derive(Debug)]
pub enum DidLookup {
    /// Route directly to this extension, skipping the rule engine.
    ShortCircuitExtension(String),
    /// Known DID, no extension — fall through to rules (caller may want to
    /// tag context).
    KnownNoExtension {
        trunk_name: String,
        number: String,
    },
    /// DID owned by another trunk; strict mode → reject with this reason.
    Reject(String),
    /// Not a DID we know (unparseable, unknown, or loose-mode mismatch);
    /// continue with existing matching.
    FallThrough,
}

/// DID-first inbound resolution. Pure function — no I/O.
///
/// * `index` — current DID snapshot.
/// * `default_country` — ISO alpha-2 (e.g. `"US"`) for normalizing
///   local-format numbers.
/// * `callee_user` — the `To:` user part as received on the wire.
/// * `source_trunk_name` — the trunk the INVITE arrived on.
/// * `strict_mode` — when true, mismatched ownership is a reject; otherwise
///   it's a warn + fall-through.
pub fn did_lookup_result(
    index: &DidIndex,
    default_country: Option<&str>,
    callee_user: &str,
    source_trunk_name: &str,
    strict_mode: bool,
) -> DidLookup {
    let region_upper = default_country.map(|c| c.to_ascii_uppercase());
    let normalized = match crate::models::did::normalize_did(callee_user, region_upper.as_deref()) {
        Ok(n) => n,
        Err(_) => return DidLookup::FallThrough,
    };

    let Some(entry) = index.lookup(&normalized) else {
        return DidLookup::FallThrough;
    };

    let Some(owner_trunk) = entry.trunk_name.as_deref() else {
        tracing::debug!(
            did = %normalized,
            source = %source_trunk_name,
            "inbound DID is unassigned (no owning trunk), falling through"
        );
        return DidLookup::FallThrough;
    };

    if owner_trunk != source_trunk_name {
        if strict_mode {
            return DidLookup::Reject(format!(
                "DID {} belongs to trunk '{}', call arrived on '{}'",
                normalized, owner_trunk, source_trunk_name
            ));
        }
        tracing::warn!(
            did = %normalized,
            owner = %owner_trunk,
            source = %source_trunk_name,
            "inbound DID ownership mismatch (loose mode, falling through)"
        );
        return DidLookup::FallThrough;
    }

    if let Some(ext) = &entry.extension_number {
        DidLookup::ShortCircuitExtension(ext.clone())
    } else {
        DidLookup::KnownNoExtension {
            trunk_name: owner_trunk.to_string(),
            number: normalized,
        }
    }
}

/// Build the [`RouteResult::Forward`] used when a DID short-circuits to an
/// on-net extension. Mirrors the `ActionType::Forward` branch in
/// [`match_invite_impl`] for the "no dest trunk" case (internal extension):
/// the callee user is rewritten to the extension number and the result is
/// wrapped in a bare `Forward(option, None)`. No `DialplanHints`, no trunk
/// config, no rewrite of host/port — the local registrar resolves the
/// contact from the extension user, just like a rule-driven internal-only
/// forward.
pub fn build_did_extension_route_result(
    mut option: InviteOption,
    extension: &str,
) -> Result<RouteResult> {
    option.callee = update_uri_user(&option.callee, extension)?;
    Ok(RouteResult::Forward(option, None))
}
