//! Unit tests for the `HealthTally` state machine (Plan 1, Task 2).

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use rustpbx::models::sip_trunk::{
    self, Entity as TrunkEntity, SipTransport, SipTrunkDirection, SipTrunkStatus,
};
use rustpbx::proxy::gateway_health::{
    GatewayHealthMonitor, HealthTally, HealthThresholds, ProbeOutcome, Transition,
};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

fn t() -> HealthThresholds {
    HealthThresholds {
        failure: 3,
        recovery: 2,
    }
}

#[test]
fn healthy_stays_healthy_on_success() {
    let mut tally = HealthTally::new(SipTrunkStatus::Healthy);
    assert_eq!(tally.record_success(&t()), Transition::NoChange);
}

#[test]
fn healthy_flips_to_offline_after_n_failures() {
    let mut tally = HealthTally::new(SipTrunkStatus::Healthy);
    assert_eq!(tally.record_failure(&t()), Transition::NoChange); // 1
    assert_eq!(tally.record_failure(&t()), Transition::NoChange); // 2
    assert_eq!(
        tally.record_failure(&t()),
        Transition::To(SipTrunkStatus::Offline)
    ); // 3
}

#[test]
fn offline_recovers_after_m_successes() {
    let mut tally = HealthTally::new(SipTrunkStatus::Offline);
    assert_eq!(tally.record_success(&t()), Transition::NoChange); // 1
    assert_eq!(
        tally.record_success(&t()),
        Transition::To(SipTrunkStatus::Healthy)
    ); // 2
}

async fn make_test_db() -> sea_orm::DatabaseConnection {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "rustpbx-gw-health-unit-{pid}-{n}.sqlite3"
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}", path.display());
    rustpbx::models::create_db(&url).await.expect("create_db")
}

#[tokio::test]
async fn tick_with_probe_flips_to_offline_after_threshold() {
    let db = make_test_db().await;
    let now = Utc::now();
    let trunk = sip_trunk::ActiveModel {
        name: Set("flippy".into()),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Healthy),
        sip_server: Set(Some("127.0.0.1:1".into())),
        sip_transport: Set(SipTransport::Udp),
        is_active: Set(true),
        register_enabled: Set(false),
        rewrite_hostport: Set(true),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        // 0 forces a probe on every tick
        health_check_interval_secs: Set(Some(0)),
        failure_threshold: Set(Some(3)),
        recovery_threshold: Set(Some(2)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let inserted = trunk.insert(&db).await.expect("insert trunk");

    let monitor = GatewayHealthMonitor::new(db.clone(), None);
    // Inject a failing probe.
    let always_fail = |_t: sip_trunk::Model| async move {
        ProbeOutcome {
            ok: false,
            latency_ms: 1,
            detail: "mock fail".into(),
        }
    };

    for _ in 0..3 {
        monitor
            .tick_with_probe(always_fail)
            .await
            .expect("tick_with_probe");
    }

    let row = TrunkEntity::find_by_id(inserted.id)
        .one(&db)
        .await
        .expect("query")
        .expect("row exists");
    assert_eq!(row.status, SipTrunkStatus::Offline);
    assert!(row.last_health_check_at.is_some());
}

#[tokio::test]
async fn tick_with_probe_recovers_after_successes() {
    let db = make_test_db().await;
    let now = Utc::now();
    let trunk = sip_trunk::ActiveModel {
        name: Set("recovery".into()),
        direction: Set(SipTrunkDirection::Outbound),
        status: Set(SipTrunkStatus::Offline),
        sip_server: Set(Some("127.0.0.1:1".into())),
        sip_transport: Set(SipTransport::Udp),
        is_active: Set(true),
        register_enabled: Set(false),
        rewrite_hostport: Set(true),
        consecutive_failures: Set(0),
        consecutive_successes: Set(0),
        health_check_interval_secs: Set(Some(0)),
        failure_threshold: Set(Some(3)),
        recovery_threshold: Set(Some(2)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let inserted = trunk.insert(&db).await.expect("insert trunk");

    let monitor = GatewayHealthMonitor::new(db.clone(), None);
    let always_ok = |_t: sip_trunk::Model| async move {
        ProbeOutcome {
            ok: true,
            latency_ms: 1,
            detail: "200".into(),
        }
    };

    for _ in 0..2 {
        monitor.tick_with_probe(always_ok).await.expect("tick");
    }

    let row = TrunkEntity::find_by_id(inserted.id)
        .one(&db)
        .await
        .expect("query")
        .expect("row exists");
    assert_eq!(row.status, SipTrunkStatus::Healthy);
}

#[test]
fn failure_count_resets_on_intermediate_success() {
    let mut tally = HealthTally::new(SipTrunkStatus::Healthy);
    tally.record_failure(&t());
    tally.record_failure(&t());
    assert_eq!(tally.record_success(&t()), Transition::NoChange);
    assert_eq!(tally.record_failure(&t()), Transition::NoChange);
    assert_eq!(tally.record_failure(&t()), Transition::NoChange);
    assert_eq!(
        tally.record_failure(&t()),
        Transition::To(SipTrunkStatus::Offline)
    );
}
