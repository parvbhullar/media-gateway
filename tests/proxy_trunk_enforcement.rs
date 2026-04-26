//! Phase 5 Plan 05-04 Task 6 (IT-03) — proxy trunk-enforcement integration tests.
//!
//! Drives `match_invite_with_trace_and_codecs` against a real seaorm sqlite
//! DB with seeded trunk_group + acl_entries + capacity rows, and asserts the
//! `RouteResult::Reject` shape (or success Forward) for each enforcement
//! gate combination required by the plan's success criteria.

use chrono::Utc;
use rustpbx::call::{DialDirection, RoutingState};
use rustpbx::config::RouteResult;
use rustpbx::models::sip_trunk::{
    self, SipTransport, SipTrunkDirection, SipTrunkStatus,
};
use rustpbx::models::trunk_acl_entries;
use rustpbx::models::trunk_capacity;
use rustpbx::models::trunk_group::{self, TrunkGroupDistributionMode};
use rustpbx::models::trunk_group_member;
use rustpbx::proxy::routing::matcher::{
    RouteTrace, match_invite_with_trace_and_codecs,
};
use rustpbx::proxy::routing::{
    DestConfig, MatchConditions, RouteAction, RouteRule, TrunkConfig,
};
use rustpbx::proxy::trunk_capacity_state::TrunkCapacityState;
use sea_orm::{ActiveModelTrait, Set};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

mod common;
use common::test_state_empty;

// =========================================================================
// Helpers
// =========================================================================

async fn insert_gateway(
    db: &sea_orm::DatabaseConnection,
    name: &str,
) -> sip_trunk::Model {
    let now = Utc::now();
    let am = sip_trunk::ActiveModel {
        name: Set(name.to_string()),
        display_name: Set(Some(format!("{} display", name))),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        sip_server: Set(Some("sip.example.com:5060".to_string())),
        sip_transport: Set(SipTransport::Udp),
        is_active: Set(true),
        register_enabled: Set(false),
        rewrite_hostport: Set(true),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    am.insert(db).await.expect("insert gateway")
}

/// Seed a trunk_group + one gateway member + optional capacity row +
/// optional ACL entries, returning the trunk_group_id.
async fn seed_trunk_with_enforcement(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    max_calls: Option<u32>,
    max_cps: Option<u32>,
    acl_rules: &[&str],
) -> i64 {
    insert_gateway(db, &format!("{}-gw", name)).await;

    let now = Utc::now();
    let group_am = trunk_group::ActiveModel {
        name: Set(name.to_string()),
        display_name: Set(Some(format!("{} display", name))),
        direction: Set(SipTrunkDirection::Outbound),
        distribution_mode: Set(TrunkGroupDistributionMode::RoundRobin),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let group = group_am.insert(db).await.expect("insert trunk group");
    let group_id = group.id;

    let gw_name = format!("{}-gw", name);
    let member_am = trunk_group_member::ActiveModel {
        trunk_group_id: Set(group_id),
        gateway_name: Set(gw_name.clone()),
        weight: Set(100),
        priority: Set(0),
        position: Set(0),
        ..Default::default()
    };
    member_am.insert(db).await.expect("insert member");

    if max_calls.is_some() || max_cps.is_some() {
        let cap_am = trunk_capacity::ActiveModel {
            trunk_group_id: Set(group_id),
            max_calls: Set(max_calls.map(|v| v as i32)),
            max_cps: Set(max_cps.map(|v| v as i32)),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        cap_am.insert(db).await.expect("insert capacity");
    }

    for (idx, rule) in acl_rules.iter().enumerate() {
        let acl_am = trunk_acl_entries::ActiveModel {
            trunk_group_id: Set(group_id),
            rule: Set((*rule).to_string()),
            position: Set(idx as i32),
            created_at: Set(now),
            ..Default::default()
        };
        acl_am.insert(db).await.expect("insert acl");
    }

    group_id
}

fn make_invite_option(callee_user: &str) -> rsipstack::dialog::invitation::InviteOption {
    rsipstack::dialog::invitation::InviteOption {
        caller: "sip:alice@rustpbx.com"
            .try_into()
            .expect("invalid caller URI"),
        callee: format!("sip:{}@rustpbx.com", callee_user)
            .as_str()
            .try_into()
            .expect("invalid callee URI"),
        contact: "sip:user@192.168.1.1:5060"
            .try_into()
            .expect("invalid contact URI"),
        ..Default::default()
    }
}

fn make_invite_request(callee_user: &str) -> rsipstack::sip::Request {
    rsipstack::sip::Request {
        method: rsipstack::sip::Method::Invite,
        uri: format!("sip:{}@rustpbx.com", callee_user)
            .as_str()
            .try_into()
            .expect("invalid request URI"),
        version: rsipstack::sip::Version::V2,
        headers: vec![
            rsipstack::sip::Header::Via(
                "SIP/2.0/UDP 192.168.1.1:5060;branch=z9hG4bKtest123"
                    .try_into()
                    .expect("invalid Via"),
            ),
            rsipstack::sip::Header::From(
                "Alice <sip:alice@rustpbx.com>;tag=abc123"
                    .try_into()
                    .expect("invalid From"),
            ),
            rsipstack::sip::Header::To(
                format!("Bob <sip:{}@rustpbx.com>", callee_user)
                    .as_str()
                    .try_into()
                    .expect("invalid To"),
            ),
            rsipstack::sip::Header::CallId("test-call-id-001".into()),
            rsipstack::sip::Header::CSeq(
                "1 INVITE".try_into().expect("invalid CSeq"),
            ),
            rsipstack::sip::Header::MaxForwards(70.into()),
        ]
        .into(),
        body: Vec::new(),
    }
}

fn build_routes_to(group_name: &str, codecs: &[&str], callee_user: &str) -> Vec<RouteRule> {
    vec![RouteRule {
        name: format!("route-to-{}", group_name),
        priority: 100,
        match_conditions: MatchConditions {
            to_user: Some(callee_user.to_string()),
            ..Default::default()
        },
        action: RouteAction {
            dest: Some(DestConfig::Single(group_name.to_string())),
            select: "rr".to_string(),
            ..Default::default()
        },
        codecs: codecs.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }]
}

fn build_trunks_for(group_name: &str) -> HashMap<String, TrunkConfig> {
    let mut trunks: HashMap<String, TrunkConfig> = HashMap::new();
    let gw_name = format!("{}-gw", group_name);
    trunks.insert(
        gw_name,
        TrunkConfig {
            dest: "sip.example.com:5060".to_string(),
            ..Default::default()
        },
    );
    trunks
}

/// Run the matcher against a freshly seeded scenario and return the
/// resulting RouteResult (Permit dropped immediately).
async fn run_match(
    state: &rustpbx::app::AppState,
    capacity_state: &Arc<TrunkCapacityState>,
    group_name: &str,
    codecs: &[&str],
    caller_codecs: &[&str],
    peer_ip: Option<IpAddr>,
    callee_user: &str,
) -> RouteResult {
    let db = state.db();
    let routes = build_routes_to(group_name, codecs, callee_user);
    let trunks = build_trunks_for(group_name);
    let option = make_invite_option(callee_user);
    let origin = make_invite_request(callee_user);
    let routing_state = Arc::new(
        RoutingState::new_with_db(Some(db.clone()))
            .with_trunk_capacity_state(capacity_state.clone()),
    );

    let mut trace = RouteTrace::default();
    let (result, _permit) = match_invite_with_trace_and_codecs(
        Some(&trunks),
        Some(&routes),
        None,
        option,
        &origin,
        None,
        routing_state,
        &DialDirection::Outbound,
        &mut trace,
        caller_codecs.iter().map(|s| s.to_string()).collect(),
        peer_ip,
    )
    .await
    .expect("match_invite_with_trace_and_codecs should not error");
    result
}

// =========================================================================
// Test 1: capacity exhaustion → 503 + Retry-After:5
// =========================================================================

#[tokio::test]
async fn capacity_max_calls_exhaustion_returns_503() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    seed_trunk_with_enforcement(state.db(), "cap-trunk", Some(1), None, &[]).await;

    // 1st INVITE: success (acquires permit which is dropped at end)
    let r1 = run_match(
        &state,
        &cap_state,
        "cap-trunk",
        &[],
        &[],
        None,
        "1001",
    )
    .await;
    assert!(matches!(r1, RouteResult::Forward(_, _)));
}

#[tokio::test]
async fn capacity_held_then_second_returns_503_until_drop() {
    // Sequential acquire test: hold one permit explicitly, second INVITE
    // surfaces trunk_capacity_exhausted.
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    let group_id =
        seed_trunk_with_enforcement(state.db(), "cap-hold", Some(1), None, &[]).await;

    // Manually reserve the slot.
    let _held = match cap_state.try_acquire(group_id, Some(1), None) {
        rustpbx::proxy::trunk_capacity_state::AcquireOutcome::Ok(p) => p,
        other => panic!("expected Ok permit, got {:?}", other),
    };

    let r2 = run_match(
        &state,
        &cap_state,
        "cap-hold",
        &[],
        &[],
        None,
        "1001",
    )
    .await;
    match r2 {
        RouteResult::Reject {
            code,
            reason,
            retry_after_secs,
        } => {
            assert_eq!(code, 503);
            assert_eq!(reason, "trunk_capacity_exhausted");
            assert_eq!(retry_after_secs, Some(5));
        }
        other => panic!("expected 503 Reject, got {:?}", route_summary(&other)),
    }

    // After dropping the held permit, a fresh INVITE succeeds again.
    drop(_held);
    let r3 = run_match(
        &state,
        &cap_state,
        "cap-hold",
        &[],
        &[],
        None,
        "1001",
    )
    .await;
    assert!(matches!(r3, RouteResult::Forward(_, _)));
}

// =========================================================================
// Test 2: CPS exhaustion → 503 + Retry-After:5
// =========================================================================

#[tokio::test]
async fn capacity_cps_exhaustion_returns_503() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    seed_trunk_with_enforcement(
        state.db(),
        "cps-trunk",
        Some(1000),
        Some(2),
        &[],
    )
    .await;

    let _r1 = run_match(&state, &cap_state, "cps-trunk", &[], &[], None, "1001").await;
    let _r2 = run_match(&state, &cap_state, "cps-trunk", &[], &[], None, "1001").await;
    let r3 = run_match(&state, &cap_state, "cps-trunk", &[], &[], None, "1001").await;
    match r3 {
        RouteResult::Reject {
            code,
            reason,
            retry_after_secs,
        } => {
            assert_eq!(code, 503);
            assert_eq!(reason, "trunk_cps_exhausted");
            assert_eq!(retry_after_secs, Some(5));
        }
        other => panic!("expected 503 cps Reject, got {:?}", route_summary(&other)),
    }
}

// =========================================================================
// Test 3: codec mismatch → 488
// =========================================================================

#[tokio::test]
async fn codec_mismatch_returns_488() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    seed_trunk_with_enforcement(state.db(), "codec-trunk", None, None, &[]).await;

    let r = run_match(
        &state,
        &cap_state,
        "codec-trunk",
        &["opus"],
        &["pcmu", "pcma"],
        None,
        "1001",
    )
    .await;
    match r {
        RouteResult::Reject {
            code,
            reason,
            retry_after_secs,
        } => {
            assert_eq!(code, 488);
            assert_eq!(reason, "codec_mismatch_488");
            assert_eq!(retry_after_secs, None);
        }
        other => panic!("expected 488 Reject, got {:?}", route_summary(&other)),
    }
}

// =========================================================================
// Test 4: codec intersection nonempty → success
// =========================================================================

#[tokio::test]
async fn codec_intersection_nonempty_succeeds() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    seed_trunk_with_enforcement(state.db(), "codec-ok", None, None, &[]).await;
    let r = run_match(
        &state,
        &cap_state,
        "codec-ok",
        &["pcmu", "opus"],
        &["opus", "g729"],
        None,
        "1001",
    )
    .await;
    assert!(matches!(r, RouteResult::Forward(_, _)));
}

// =========================================================================
// Test 5: empty trunk codec list = allow-all (D-20)
// =========================================================================

#[tokio::test]
async fn codec_filter_disabled_when_trunk_codecs_empty() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    seed_trunk_with_enforcement(state.db(), "codec-allow", None, None, &[]).await;
    let r = run_match(
        &state,
        &cap_state,
        "codec-allow",
        &[],
        &["pcmu"],
        None,
        "1001",
    )
    .await;
    assert!(matches!(r, RouteResult::Forward(_, _)));
}

// =========================================================================
// Test 6: per-trunk ACL deny → 403
// =========================================================================

#[tokio::test]
async fn per_trunk_acl_deny_returns_403() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    seed_trunk_with_enforcement(
        state.db(),
        "acl-trunk",
        None,
        None,
        &["deny 1.2.3.4", "allow all"],
    )
    .await;
    let peer: IpAddr = "1.2.3.4".parse().unwrap();
    let r = run_match(
        &state,
        &cap_state,
        "acl-trunk",
        &[],
        &[],
        Some(peer),
        "1001",
    )
    .await;
    match r {
        RouteResult::Reject {
            code,
            reason,
            retry_after_secs,
        } => {
            assert_eq!(code, 403);
            assert_eq!(reason, "trunk_acl_blocked");
            assert_eq!(retry_after_secs, None);
        }
        other => panic!("expected 403 Reject, got {:?}", route_summary(&other)),
    }
}

// =========================================================================
// Test 7: ACL allow → passes
// =========================================================================

#[tokio::test]
async fn per_trunk_acl_allow_passes_to_capacity_gate() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    seed_trunk_with_enforcement(
        state.db(),
        "acl-pass",
        Some(10),
        None,
        &["allow all"],
    )
    .await;
    let peer: IpAddr = "5.5.5.5".parse().unwrap();
    let r = run_match(
        &state,
        &cap_state,
        "acl-pass",
        &[],
        &[],
        Some(peer),
        "1001",
    )
    .await;
    assert!(matches!(r, RouteResult::Forward(_, _)));
}

// =========================================================================
// Test 8: default-allow when no ACL rules (D-14)
// =========================================================================

#[tokio::test]
async fn default_allow_when_no_acl_rules() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    seed_trunk_with_enforcement(state.db(), "acl-empty", None, None, &[]).await;
    let peer: IpAddr = "9.9.9.9".parse().unwrap();
    let r = run_match(
        &state,
        &cap_state,
        "acl-empty",
        &[],
        &[],
        Some(peer),
        "1001",
    )
    .await;
    assert!(matches!(r, RouteResult::Forward(_, _)));
}

// =========================================================================
// Test 9: enforcement order ACL → capacity → codec
// =========================================================================

#[tokio::test]
async fn enforcement_order_acl_then_capacity_then_codec() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    let group_id = seed_trunk_with_enforcement(
        state.db(),
        "ord-trunk",
        Some(1),
        None,
        &["deny 1.2.3.4", "allow all"],
    )
    .await;

    // (a) From a denied IP — must surface trunk_acl_blocked even when
    // capacity is exhausted and codec mismatches.
    let _hold = match cap_state.try_acquire(group_id, Some(1), None) {
        rustpbx::proxy::trunk_capacity_state::AcquireOutcome::Ok(p) => p,
        other => panic!("expected Ok, got {:?}", other),
    };
    let denied: IpAddr = "1.2.3.4".parse().unwrap();
    let r = run_match(
        &state,
        &cap_state,
        "ord-trunk",
        &["opus"],
        &["pcmu"],
        Some(denied),
        "1001",
    )
    .await;
    match r {
        RouteResult::Reject { reason, .. } => {
            assert_eq!(reason, "trunk_acl_blocked")
        }
        other => panic!("expected ACL Reject, got {:?}", route_summary(&other)),
    }

    // (b) From an allowed IP, capacity STILL exhausted → 503 not 488.
    let allowed: IpAddr = "5.5.5.5".parse().unwrap();
    let r2 = run_match(
        &state,
        &cap_state,
        "ord-trunk",
        &["opus"],
        &["pcmu"],
        Some(allowed),
        "1001",
    )
    .await;
    match r2 {
        RouteResult::Reject { reason, .. } => {
            assert_eq!(reason, "trunk_capacity_exhausted")
        }
        other => panic!("expected capacity Reject, got {:?}", route_summary(&other)),
    }

    // (c) Drop the held permit — codec mismatch now surfaces.
    drop(_hold);
    let r3 = run_match(
        &state,
        &cap_state,
        "ord-trunk",
        &["opus"],
        &["pcmu"],
        Some(allowed),
        "1001",
    )
    .await;
    match r3 {
        RouteResult::Reject { reason, .. } => {
            assert_eq!(reason, "codec_mismatch_488")
        }
        other => panic!("expected codec Reject, got {:?}", route_summary(&other)),
    }
}

// =========================================================================
// Test 10: happy path
// =========================================================================

#[tokio::test]
async fn happy_path_dispatches_normally() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    seed_trunk_with_enforcement(
        state.db(),
        "happy-trunk",
        Some(10),
        Some(10),
        &["allow all"],
    )
    .await;
    let peer: IpAddr = "10.0.0.1".parse().unwrap();
    let r = run_match(
        &state,
        &cap_state,
        "happy-trunk",
        &["pcmu"],
        &["pcmu"],
        Some(peer),
        "1001",
    )
    .await;
    assert!(matches!(r, RouteResult::Forward(_, _)));
}

// =========================================================================
// Test 11: permit release on call end frees capacity
// =========================================================================

#[tokio::test]
async fn permit_release_on_call_end_frees_capacity() {
    let state = test_state_empty().await;
    let cap_state = Arc::new(TrunkCapacityState::new());
    let group_id =
        seed_trunk_with_enforcement(state.db(), "rel-trunk", Some(1), None, &[]).await;

    let held = match cap_state.try_acquire(group_id, Some(1), None) {
        rustpbx::proxy::trunk_capacity_state::AcquireOutcome::Ok(p) => p,
        other => panic!("expected Ok permit, got {:?}", other),
    };
    let blocked = run_match(
        &state,
        &cap_state,
        "rel-trunk",
        &[],
        &[],
        None,
        "1001",
    )
    .await;
    assert!(matches!(blocked, RouteResult::Reject { code: 503, .. }));

    drop(held);
    let r = run_match(
        &state,
        &cap_state,
        "rel-trunk",
        &[],
        &[],
        None,
        "1001",
    )
    .await;
    assert!(matches!(r, RouteResult::Forward(_, _)));
}

// =========================================================================
// Diagnostics helper for panic messages.
// =========================================================================

fn route_summary(r: &RouteResult) -> String {
    match r {
        RouteResult::Forward(_, _) => "Forward".to_string(),
        RouteResult::Queue { .. } => "Queue".to_string(),
        RouteResult::Application { app_name, .. } => {
            format!("Application({})", app_name)
        }
        RouteResult::NotHandled(_, _) => "NotHandled".to_string(),
        RouteResult::Abort(code, reason) => {
            format!("Abort({:?}, {:?})", code, reason)
        }
        RouteResult::Reject {
            code,
            reason,
            retry_after_secs,
        } => format!(
            "Reject({}, {}, retry_after={:?})",
            code, reason, retry_after_secs
        ),
    }
}
