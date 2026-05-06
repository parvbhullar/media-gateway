//! Integration tests for `TranslationEngine` (Phase 8 Plan 08-04 — IT-TRN-06).
//!
//! Coverage matrix per `08-04-PLAN.md` <behavior> (D-29 cases 1-5):
//!
//!   1. UK normalize on inbound INVITE: 02079460123 → +442079460123.
//!   2. US normalize on inbound INVITE: 4155551234 → +14155551234.
//!   3. Direction filter (TRN-05 negative): inbound-only rule does NOT
//!      fire on an outbound INVITE; trace empty.
//!   4. Cascade in priority order: rule 2 sees rule 1's output; trace
//!      records both AppliedRules in order.
//!   5. Per-field independence: caller-only rule leaves destination URI
//!      unchanged.
//!
//! These mirror the in-tree `#[cfg(test)] mod tests` in
//! `src/proxy/translation/engine.rs`, but exercise the engine from a
//! separate test binary against the real public entity layer — proving
//! the `models::translations` + `proxy::translation::engine` round-trip
//! works through the crate's public API surface.

use rsipstack::dialog::invitation::InviteOption;
use rsipstack::sip::Uri;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use sea_orm_migration::prelude::*;
use sea_orm_migration::MigratorTrait;

use rustpbx::call::DialDirection;
use rustpbx::models::translations::{ActiveModel, Migration as TranslationsMigration};
use rustpbx::proxy::translation::TranslationEngine;

// ─── Helpers ────────────────────────────────────────────────────────────

/// Minimal in-test Migrator that runs ONLY the translations migration.
struct TestMigrator;

#[async_trait::async_trait]
impl MigratorTrait for TestMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(TranslationsMigration)]
    }
}

async fn fresh_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("open sqlite memory db");
    TestMigrator::up(&db, None)
        .await
        .expect("run translations migration");
    db
}

#[allow(clippy::too_many_arguments)]
async fn seed_translation(
    db: &DatabaseConnection,
    name: &str,
    caller_pattern: Option<&str>,
    destination_pattern: Option<&str>,
    caller_replacement: Option<&str>,
    destination_replacement: Option<&str>,
    direction: &str,
    priority: i32,
) -> String {
    let now = chrono::Utc::now();
    let id = format!("trn-{}", name);
    let am = ActiveModel {
        id: Set(id.clone()),
        name: Set(name.to_string()),
        description: Set(None),
        caller_pattern: Set(caller_pattern.map(|s| s.to_string())),
        destination_pattern: Set(destination_pattern.map(|s| s.to_string())),
        caller_replacement: Set(caller_replacement.map(|s| s.to_string())),
        destination_replacement: Set(destination_replacement.map(|s| s.to_string())),
        direction: Set(direction.to_string()),
        priority: Set(priority),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        account_id: Set("root".to_string()),
    };
    let inserted = am.insert(db).await.expect("insert translation row");
    inserted.id
}

fn make_invite_option(caller_user: &str, callee_user: &str) -> InviteOption {
    let caller_str = format!("sip:{}@example.com", caller_user);
    let callee_str = format!("sip:{}@example.com", callee_user);
    let contact_str = format!("sip:{}@192.168.1.1:5060", caller_user);
    InviteOption {
        caller: Uri::try_from(caller_str.as_str()).expect("caller uri"),
        callee: Uri::try_from(callee_str.as_str()).expect("callee uri"),
        contact: Uri::try_from(contact_str.as_str()).expect("contact uri"),
        ..Default::default()
    }
}

fn caller_user(opt: &InviteOption) -> String {
    opt.caller.user().unwrap_or_default().to_string()
}

fn callee_user(opt: &InviteOption) -> String {
    opt.callee.user().unwrap_or_default().to_string()
}

// =========================================================================
// D-29 #1: UK normalization on inbound INVITE
// =========================================================================

#[tokio::test]
async fn it_trn_06_uk_normalize_inbound() {
    let db = fresh_db().await;
    seed_translation(
        &db,
        "uk-norm",
        Some(r"^0(\d+)$"),
        None,
        Some("+44$1"),
        None,
        "inbound",
        100,
    )
    .await;
    let engine = TranslationEngine::new();
    let mut opt = make_invite_option("02079460123", "callee");
    let trace = engine
        .translate(&mut opt, DialDirection::Inbound, &db)
        .await
        .expect("translate");

    assert_eq!(caller_user(&opt), "+442079460123");
    assert_eq!(trace.applied_rules.len(), 1);
    assert_eq!(trace.applied_rules[0].field, "caller");
    assert_eq!(trace.applied_rules[0].before, "02079460123");
    assert_eq!(trace.applied_rules[0].after, "+442079460123");
}

// =========================================================================
// D-29 #2: US normalization on inbound INVITE
// =========================================================================

#[tokio::test]
async fn it_trn_06_us_normalize_inbound() {
    let db = fresh_db().await;
    seed_translation(
        &db,
        "us-norm",
        Some(r"^([2-9]\d{9})$"),
        None,
        Some("+1$1"),
        None,
        "inbound",
        100,
    )
    .await;
    let engine = TranslationEngine::new();
    let mut opt = make_invite_option("4155551234", "callee");
    let trace = engine
        .translate(&mut opt, DialDirection::Inbound, &db)
        .await
        .expect("translate");

    assert_eq!(caller_user(&opt), "+14155551234");
    assert_eq!(trace.applied_rules.len(), 1);
    assert_eq!(trace.applied_rules[0].before, "4155551234");
    assert_eq!(trace.applied_rules[0].after, "+14155551234");
}

// =========================================================================
// D-29 #3 / TRN-05: direction filter — inbound rule on outbound call
// =========================================================================

#[tokio::test]
async fn it_trn_06_direction_filter_outbound_call_inbound_rule() {
    let db = fresh_db().await;
    // Same UK + US rules as cases 1 + 2, both flagged "inbound".
    seed_translation(
        &db,
        "uk-norm",
        Some(r"^0(\d+)$"),
        None,
        Some("+44$1"),
        None,
        "inbound",
        100,
    )
    .await;
    seed_translation(
        &db,
        "us-norm",
        Some(r"^([2-9]\d{9})$"),
        None,
        Some("+1$1"),
        None,
        "inbound",
        110,
    )
    .await;
    let engine = TranslationEngine::new();
    let mut opt = make_invite_option("02079460123", "callee");
    let trace = engine
        .translate(&mut opt, DialDirection::Outbound, &db)
        .await
        .expect("translate");

    // Pattern matches but rule is direction-skipped → no rewrite, no trace.
    assert_eq!(caller_user(&opt), "02079460123");
    assert!(
        trace.applied_rules.is_empty(),
        "expected empty trace, got {:?}",
        trace.applied_rules
    );
}

// =========================================================================
// D-29 #4: cascade in priority order
// =========================================================================

#[tokio::test]
async fn it_trn_06_cascade_priority_order() {
    let db = fresh_db().await;
    // priority 10: strip leading zero (^0(\d+)$ → $1).
    seed_translation(
        &db,
        "strip-leading-zero",
        Some(r"^0(\d+)$"),
        None,
        Some("$1"),
        None,
        "inbound",
        10,
    )
    .await;
    // priority 20: prepend +44 to a non-zero-leading number.
    seed_translation(
        &db,
        "prepend-uk",
        Some(r"^([1-9]\d+)$"),
        None,
        Some("+44$1"),
        None,
        "inbound",
        20,
    )
    .await;
    let engine = TranslationEngine::new();
    let mut opt = make_invite_option("02079460123", "callee");
    let trace = engine
        .translate(&mut opt, DialDirection::Inbound, &db)
        .await
        .expect("translate");

    assert_eq!(caller_user(&opt), "+442079460123");
    assert_eq!(trace.applied_rules.len(), 2);
    // Order matches priority ASC: strip first, then prepend.
    assert_eq!(trace.applied_rules[0].rule_name, "strip-leading-zero");
    assert_eq!(trace.applied_rules[0].before, "02079460123");
    assert_eq!(trace.applied_rules[0].after, "2079460123");
    assert_eq!(trace.applied_rules[1].rule_name, "prepend-uk");
    assert_eq!(trace.applied_rules[1].before, "2079460123");
    assert_eq!(trace.applied_rules[1].after, "+442079460123");
}

// =========================================================================
// D-29 #5: per-field independence — caller-only rule leaves destination alone
// =========================================================================

#[tokio::test]
async fn it_trn_06_per_field_independence() {
    let db = fresh_db().await;
    seed_translation(
        &db,
        "caller-only",
        Some(r"^0(\d+)$"),
        None, // destination_pattern explicitly NULL
        Some("+44$1"),
        None, // destination_replacement explicitly NULL
        "inbound",
        100,
    )
    .await;
    let engine = TranslationEngine::new();
    let mut opt = make_invite_option("02079460123", "1234");
    engine
        .translate(&mut opt, DialDirection::Inbound, &db)
        .await
        .expect("translate");

    assert_eq!(caller_user(&opt), "+442079460123");
    // Destination URI bytes unchanged — pattern was None for this field.
    assert_eq!(callee_user(&opt), "1234");
}
