//! Unit + integration tests for trunk_group distribution dispatch.
//!
//! Phase 2 Plan 02-03 Task 3 (TRK-05).
//!
//! Tests 1-9: resolver-level unit tests (distribution mode translation,
//! error paths, parallel feature gate).
//! Tests 10-12: hash determinism via select_gateway_for_trunk_group.
//! Test 13: matcher-level integration test proving end-to-end dispatch
//! through match_invite with a real SeaORM DB + RoutingState::new_with_db.

use chrono::Utc;
use rustpbx::call::RoutingState;
use rustpbx::models::sip_trunk::{
    self, SipTrunkDirection, SipTrunkStatus, SipTransport,
};
use rustpbx::models::trunk_group::{self, TrunkGroupDistributionMode};
use rustpbx::models::trunk_group_member;
use rustpbx::proxy::routing::trunk_group_resolver::*;
use rustpbx::proxy::routing::DestConfig;
use sea_orm::{ActiveModelTrait, Set};
use std::sync::Arc;

mod common;
use common::test_state_empty;

// =========================================================================
// Helpers
// =========================================================================

/// Insert a gateway (sip_trunk) row so we can reference it as a member.
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

/// Seed a trunk_group with given distribution mode and member gateways.
/// Inserts the gateways first, then creates the group + members.
async fn seed_group(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    mode: TrunkGroupDistributionMode,
    gateway_names: &[&str],
) -> trunk_group::Model {
    // Insert gateway rows
    for gw_name in gateway_names {
        insert_gateway(db, gw_name).await;
    }
    seed_group_no_gateways(db, name, mode, gateway_names).await
}

/// Seed a trunk_group + members without inserting gateway rows (for when
/// gateways are already present or not needed).
async fn seed_group_no_gateways(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    mode: TrunkGroupDistributionMode,
    gateway_names: &[&str],
) -> trunk_group::Model {
    let now = Utc::now();
    let group_am = trunk_group::ActiveModel {
        name: Set(name.to_string()),
        display_name: Set(Some(format!("{} display", name))),
        direction: Set(SipTrunkDirection::Outbound),
        distribution_mode: Set(mode),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let group = group_am.insert(db).await.expect("insert trunk group");

    for (i, gw_name) in gateway_names.iter().enumerate() {
        let member_am = trunk_group_member::ActiveModel {
            trunk_group_id: Set(group.id),
            gateway_name: Set(gw_name.to_string()),
            weight: Set(100),
            priority: Set(0),
            position: Set(i as i32),
            ..Default::default()
        };
        member_am
            .insert(db)
            .await
            .expect("insert trunk group member");
    }

    group
}

/// Build a minimal InviteOption for testing, following the pattern in
/// src/proxy/routing/tests.rs.
fn test_invite_option(
    caller: &str,
    callee: &str,
) -> rsipstack::dialog::invitation::InviteOption {
    rsipstack::dialog::invitation::InviteOption {
        caller: caller.try_into().expect("Invalid caller URI"),
        callee: callee.try_into().expect("Invalid callee URI"),
        contact: "sip:user@192.168.1.1:5060"
            .try_into()
            .expect("Invalid contact URI"),
        ..Default::default()
    }
}

// =========================================================================
// Tests 1-5: Distribution mode translation
// =========================================================================

#[tokio::test]
async fn resolves_round_robin_to_rr_method() {
    let state = test_state_empty().await;
    let db = state.db();
    seed_group(db, "tg-rr", TrunkGroupDistributionMode::RoundRobin, &["gw-a", "gw-b"]).await;

    let resolved = resolve_trunk_group_to_dest_config(db, "tg-rr")
        .await
        .expect("resolve");
    assert_eq!(resolved.select_method, "rr");
    assert!(resolved.hash_key.is_none());
}

#[tokio::test]
async fn resolves_weight_based_to_weighted_method() {
    let state = test_state_empty().await;
    let db = state.db();
    seed_group(db, "tg-wb", TrunkGroupDistributionMode::WeightBased, &["gw-a", "gw-b"]).await;

    let resolved = resolve_trunk_group_to_dest_config(db, "tg-wb")
        .await
        .expect("resolve");
    assert_eq!(resolved.select_method, "weighted");
    assert!(resolved.hash_key.is_none());
}

#[tokio::test]
async fn resolves_hash_callid_to_hash_with_callid_key() {
    let state = test_state_empty().await;
    let db = state.db();
    seed_group(db, "tg-hc", TrunkGroupDistributionMode::HashCallid, &["gw-a", "gw-b"]).await;

    let resolved = resolve_trunk_group_to_dest_config(db, "tg-hc")
        .await
        .expect("resolve");
    assert_eq!(resolved.select_method, "hash");
    assert_eq!(resolved.hash_key.as_deref(), Some("call-id"));
}

#[tokio::test]
async fn resolves_hash_src_ip_to_hash_with_from_user_key() {
    let state = test_state_empty().await;
    let db = state.db();
    seed_group(db, "tg-hsi", TrunkGroupDistributionMode::HashSrcIp, &["gw-a", "gw-b"]).await;

    let resolved = resolve_trunk_group_to_dest_config(db, "tg-hsi")
        .await
        .expect("resolve");
    assert_eq!(resolved.select_method, "hash");
    assert_eq!(resolved.hash_key.as_deref(), Some("from.user"));
}

#[tokio::test]
async fn resolves_hash_destination_to_hash_with_to_user_key() {
    let state = test_state_empty().await;
    let db = state.db();
    seed_group(db, "tg-hd", TrunkGroupDistributionMode::HashDestination, &["gw-a", "gw-b"]).await;

    let resolved = resolve_trunk_group_to_dest_config(db, "tg-hd")
        .await
        .expect("resolve");
    assert_eq!(resolved.select_method, "hash");
    assert_eq!(resolved.hash_key.as_deref(), Some("to.user"));
}

// =========================================================================
// Test 6: Member ordering by position
// =========================================================================

#[tokio::test]
async fn resolves_returns_members_in_position_order() {
    let state = test_state_empty().await;
    let db = state.db();

    // Insert gateways first
    for gw in &["gw-z", "gw-a", "gw-m"] {
        insert_gateway(db, gw).await;
    }

    // Insert group
    let now = Utc::now();
    let group_am = trunk_group::ActiveModel {
        name: Set("tg-order".to_string()),
        display_name: Set(None),
        direction: Set(SipTrunkDirection::Outbound),
        distribution_mode: Set(TrunkGroupDistributionMode::RoundRobin),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let group = group_am.insert(db).await.expect("insert group");

    // Insert members OUT of alphabetical order but WITH explicit positions
    // position 0 = gw-m, position 1 = gw-z, position 2 = gw-a
    for (pos, gw_name) in [(0, "gw-m"), (1, "gw-z"), (2, "gw-a")] {
        let member_am = trunk_group_member::ActiveModel {
            trunk_group_id: Set(group.id),
            gateway_name: Set(gw_name.to_string()),
            weight: Set(100),
            priority: Set(0),
            position: Set(pos),
            ..Default::default()
        };
        member_am.insert(db).await.expect("insert member");
    }

    let resolved = resolve_trunk_group_to_dest_config(db, "tg-order")
        .await
        .expect("resolve");
    match resolved.dest_config {
        DestConfig::Multiple(names) => {
            assert_eq!(names, vec!["gw-m", "gw-z", "gw-a"]);
        }
        DestConfig::Single(_) => panic!("expected Multiple"),
    }
}

// =========================================================================
// Tests 7-8: Error paths
// =========================================================================

#[tokio::test]
async fn resolves_unknown_group_returns_not_found() {
    let state = test_state_empty().await;
    let db = state.db();

    let err = resolve_trunk_group_to_dest_config(db, "no-such-group")
        .await
        .unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("not found"),
        "expected NotFound, got: {}",
        msg
    );
}

#[tokio::test]
async fn resolves_empty_group_returns_no_members() {
    let state = test_state_empty().await;
    let db = state.db();

    // Insert group with zero members (direct SeaORM, bypasses handler validation)
    let now = Utc::now();
    let group_am = trunk_group::ActiveModel {
        name: Set("tg-empty".to_string()),
        display_name: Set(None),
        direction: Set(SipTrunkDirection::Outbound),
        distribution_mode: Set(TrunkGroupDistributionMode::RoundRobin),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    group_am.insert(db).await.expect("insert group");

    let err = resolve_trunk_group_to_dest_config(db, "tg-empty")
        .await
        .unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("no members"),
        "expected NoMembers, got: {}",
        msg
    );
}

// =========================================================================
// Test 9: Parallel feature gate
// =========================================================================

#[cfg(not(feature = "parallel-trunk-dial"))]
#[tokio::test]
async fn resolves_parallel_without_feature_returns_error() {
    let state = test_state_empty().await;
    let db = state.db();

    // Direct SeaORM insert with Parallel mode (handler rejects this,
    // but direct insert bypasses validation).
    let now = Utc::now();
    let group_am = trunk_group::ActiveModel {
        name: Set("tg-parallel".to_string()),
        display_name: Set(None),
        direction: Set(SipTrunkDirection::Outbound),
        distribution_mode: Set(TrunkGroupDistributionMode::Parallel),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    group_am.insert(db).await.expect("insert group");

    // Add a member so we don't hit NoMembers first
    let member_am = trunk_group_member::ActiveModel {
        trunk_group_id: Set(1), // the group we just inserted
        gateway_name: Set("gw-parallel".to_string()),
        weight: Set(100),
        priority: Set(0),
        position: Set(0),
        ..Default::default()
    };
    // We also need the gateway to exist for seed purposes, but the
    // resolver doesn't validate gateway existence — it just reads
    // gateway_name from the member table.
    member_am.insert(db).await.expect("insert member");

    let err = resolve_trunk_group_to_dest_config(db, "tg-parallel")
        .await
        .unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("parallel-trunk-dial"),
        "expected parallel feature error, got: {}",
        msg
    );
}

// =========================================================================
// Tests 10-12: Hash determinism
// =========================================================================

#[tokio::test]
async fn select_gateway_hash_callid_is_deterministic_across_three_calls() {
    // NOTE: matcher.rs:1036 currently hardcodes the Call-ID hash key string
    // to "default" regardless of the actual Call-ID header. That makes this
    // test deterministic on a constant rather than on real call-id data.
    // TODO(phase-6): replace the hardcoded "default" with a real Call-ID
    // header extraction.
    let state = test_state_empty().await;
    let db = state.db();
    seed_group(
        db,
        "tg-hash-cid",
        TrunkGroupDistributionMode::HashCallid,
        &["gw-h1", "gw-h2", "gw-h3", "gw-h4"],
    )
    .await;

    let option = test_invite_option("sip:alice@rustpbx.com", "sip:1001@rustpbx.com");
    let routing_state = Arc::new(RoutingState::new_with_db(Some(db.clone())));

    let r1 = select_gateway_for_trunk_group(db, "tg-hash-cid", &option, routing_state.clone(), None)
        .await
        .expect("call 1");
    let r2 = select_gateway_for_trunk_group(db, "tg-hash-cid", &option, routing_state.clone(), None)
        .await
        .expect("call 2");
    let r3 = select_gateway_for_trunk_group(db, "tg-hash-cid", &option, routing_state.clone(), None)
        .await
        .expect("call 3");

    assert_eq!(r1, r2, "hash determinism broken between call 1 and 2");
    assert_eq!(r2, r3, "hash determinism broken between call 2 and 3");
}

#[tokio::test]
async fn select_gateway_hash_src_ip_is_deterministic() {
    let state = test_state_empty().await;
    let db = state.db();
    seed_group(
        db,
        "tg-hash-src",
        TrunkGroupDistributionMode::HashSrcIp,
        &["gw-s1", "gw-s2", "gw-s3", "gw-s4"],
    )
    .await;

    let option = test_invite_option("sip:alice@rustpbx.com", "sip:1001@rustpbx.com");
    let routing_state = Arc::new(RoutingState::new_with_db(Some(db.clone())));

    let r1 = select_gateway_for_trunk_group(db, "tg-hash-src", &option, routing_state.clone(), None)
        .await
        .expect("call 1");
    let r2 = select_gateway_for_trunk_group(db, "tg-hash-src", &option, routing_state.clone(), None)
        .await
        .expect("call 2");
    let r3 = select_gateway_for_trunk_group(db, "tg-hash-src", &option, routing_state.clone(), None)
        .await
        .expect("call 3");

    assert_eq!(r1, r2);
    assert_eq!(r2, r3);
}

#[tokio::test]
async fn select_gateway_hash_destination_is_deterministic() {
    let state = test_state_empty().await;
    let db = state.db();
    seed_group(
        db,
        "tg-hash-dst",
        TrunkGroupDistributionMode::HashDestination,
        &["gw-d1", "gw-d2", "gw-d3", "gw-d4"],
    )
    .await;

    let option = test_invite_option("sip:alice@rustpbx.com", "sip:1001@rustpbx.com");
    let routing_state = Arc::new(RoutingState::new_with_db(Some(db.clone())));

    let r1 = select_gateway_for_trunk_group(db, "tg-hash-dst", &option, routing_state.clone(), None)
        .await
        .expect("call 1");
    let r2 = select_gateway_for_trunk_group(db, "tg-hash-dst", &option, routing_state.clone(), None)
        .await
        .expect("call 2");
    let r3 = select_gateway_for_trunk_group(db, "tg-hash-dst", &option, routing_state.clone(), None)
        .await
        .expect("call 3");

    assert_eq!(r1, r2);
    assert_eq!(r2, r3);
}

// =========================================================================
// Test 13: Matcher-level integration test
// =========================================================================

#[tokio::test]
async fn matcher_level_trunk_group_dispatch() {
    // Goal: prove TRK-05 at the dispatch boundary. Seed a real SeaORM
    // sqlite DB with a trunk_group named "tg-prod" containing two gateway
    // members ["gw-alpha", "gw-beta"], build a RoutingState that carries
    // the DB via RoutingState::new_with_db(Some(db.clone())), and call
    // match_invite which reaches the Forward select_trunk call site.
    // Assert the returned trunk name is one of {"gw-alpha", "gw-beta"}.
    //
    // This test MUST fail if: (a) RoutingState drops the db field,
    // (b) try_select_via_trunk_group is bypassed, (c) the resolver returns
    // a name not present in the seeded members, or (d) select_trunk is
    // invoked with DestConfig::Single("tg-prod") directly (which would
    // return "tg-prod" itself -- the wrong answer).

    use rustpbx::call::DialDirection;
    use rustpbx::proxy::routing::matcher::{RouteTrace, match_invite_with_trace};
    use rustpbx::proxy::routing::{RouteAction, RouteRule, TrunkConfig, MatchConditions};
    use std::collections::HashMap;

    let state = test_state_empty().await;
    let db = state.db();
    seed_group(
        db,
        "tg-prod",
        TrunkGroupDistributionMode::RoundRobin,
        &["gw-alpha", "gw-beta"],
    )
    .await;

    // Build a routing rule whose dest is the trunk_group name "tg-prod".
    // action.select is set to "rr" but the resolver overrides it — the
    // rule-level select is irrelevant once trunk_group detection fires.
    let routes = vec![RouteRule {
        name: "route-to-tg".to_string(),
        priority: 100,
        match_conditions: MatchConditions {
            to_user: Some("1001".to_string()),
            ..Default::default()
        },
        action: RouteAction {
            dest: Some(DestConfig::Single("tg-prod".to_string())),
            select: "rr".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }];

    // Build trunks config — we need entries for the member gateways so
    // the trunk lookup after selection can find them (though matcher.rs
    // doesn't require it for the selection itself).
    let mut trunks: HashMap<String, TrunkConfig> = HashMap::new();
    trunks.insert(
        "gw-alpha".to_string(),
        TrunkConfig {
            dest: "sip.example.com:5060".to_string(),
            ..Default::default()
        },
    );
    trunks.insert(
        "gw-beta".to_string(),
        TrunkConfig {
            dest: "sip.example2.com:5060".to_string(),
            ..Default::default()
        },
    );

    let option = test_invite_option("sip:alice@rustpbx.com", "sip:1001@rustpbx.com");
    let routing_state = Arc::new(RoutingState::new_with_db(Some(db.clone())));

    // Build a minimal SIP INVITE request for the origin parameter
    let origin = rsipstack::sip::Request {
        method: rsipstack::sip::Method::Invite,
        uri: "sip:1001@rustpbx.com"
            .try_into()
            .expect("Invalid request URI"),
        version: rsipstack::sip::Version::V2,
        headers: vec![
            rsipstack::sip::Header::Via(
                "SIP/2.0/UDP 192.168.1.1:5060;branch=z9hG4bKtest123"
                    .try_into()
                    .expect("Invalid Via"),
            ),
            rsipstack::sip::Header::From(
                "Alice <sip:alice@rustpbx.com>;tag=abc123"
                    .try_into()
                    .expect("Invalid From"),
            ),
            rsipstack::sip::Header::To(
                "Bob <sip:1001@rustpbx.com>"
                    .try_into()
                    .expect("Invalid To"),
            ),
            rsipstack::sip::Header::CallId("test-call-id-001".into()),
            rsipstack::sip::Header::CSeq(
                "1 INVITE".try_into().expect("Invalid CSeq"),
            ),
            rsipstack::sip::Header::MaxForwards(70.into()),
        ]
        .into(),
        body: Vec::new(),
    };

    let mut trace = RouteTrace::default();
    let _result = match_invite_with_trace(
        Some(&trunks),
        Some(&routes),
        None,
        option,
        &origin,
        None,
        routing_state,
        &DialDirection::Outbound,
        &mut trace,
    )
    .await
    .expect("match_invite should succeed");

    // The trace records the selected trunk name. It must be one of our
    // seeded member gateways, NOT the trunk_group name "tg-prod".
    let selected = trace
        .selected_trunk
        .as_deref()
        .expect("trace should have selected_trunk set");
    assert!(
        selected == "gw-alpha" || selected == "gw-beta",
        "Expected selected trunk to be gw-alpha or gw-beta, got: {}",
        selected
    );
    assert_ne!(
        selected, "tg-prod",
        "Trunk group name 'tg-prod' should have been resolved \
         to a member gateway, but was returned as-is"
    );
}
