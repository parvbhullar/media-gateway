use rustpbx::models::{did, migration, sip_trunk};
use sea_orm::{ActiveValue::Set, Database, EntityTrait};
use sea_orm_migration::MigratorTrait;
use serde_json::json;

async fn mem_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    db
}

async fn insert_legacy_trunk(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    did_numbers_json: serde_json::Value,
) {
    let now = chrono::Utc::now();
    let trunk = sip_trunk::ActiveModel {
        name: Set(name.to_string()),
        status: Set(sip_trunk::SipTrunkStatus::Healthy),
        direction: Set(sip_trunk::SipTrunkDirection::Bidirectional),
        sip_transport: Set(sip_trunk::SipTransport::Udp),
        did_numbers: Set(Some(did_numbers_json)),
        is_active: Set(true),
        register_enabled: Set(false),
        rewrite_hostport: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    sip_trunk::Entity::insert(trunk).exec(db).await.unwrap();
}

async fn set_default_country(db: &sea_orm::DatabaseConnection, code: &str) {
    rustpbx::models::system_config::Model::upsert(
        db,
        rustpbx::config_merge::ROUTING_DEFAULT_COUNTRY_KEY,
        &format!("\"{code}\""),
        false,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn backfill_copies_mixed_format_dids() {
    let db = mem_db().await;
    set_default_country(&db, "US").await;

    insert_legacy_trunk(
        &db,
        "legacy",
        json!([
            "+14158675309",
            "4155551234",
            "(415) 555-9876",
            "not-a-number",
            { "number": "+14089996789" }
        ]),
    )
    .await;

    let report = rustpbx::models::backfill_dids_from_sip_trunks::run(&db).await.unwrap();
    assert!(report.inserted >= 2, "should insert at least the two obviously-valid numbers");
    assert!(report.invalid >= 1, "'not-a-number' should be counted as invalid");

    let rows = did::Model::list_all(&db).await.unwrap();
    let numbers: std::collections::HashSet<_> = rows.iter().map(|r| r.number.clone()).collect();
    assert!(numbers.contains("+14158675309"));
    assert!(numbers.contains("+14089996789"));
    assert!(!numbers.contains("not-a-number"));
    for r in &rows {
        assert_eq!(r.trunk_name.as_deref(), Some("legacy"));
    }
}

#[tokio::test]
async fn backfill_is_idempotent() {
    let db = mem_db().await;
    set_default_country(&db, "US").await;

    insert_legacy_trunk(&db, "t", json!(["+14158675309"])).await;

    let r1 = rustpbx::models::backfill_dids_from_sip_trunks::run(&db).await.unwrap();
    assert_eq!(r1.inserted, 1);
    assert_eq!(r1.idempotent, 0);

    let r2 = rustpbx::models::backfill_dids_from_sip_trunks::run(&db).await.unwrap();
    assert_eq!(r2.inserted, 0);
    assert_eq!(r2.idempotent, 1);

    assert_eq!(did::Model::list_all(&db).await.unwrap().len(), 1);
}

#[tokio::test]
async fn backfill_detects_cross_trunk_collision() {
    let db = mem_db().await;
    set_default_country(&db, "US").await;

    insert_legacy_trunk(&db, "a", json!(["+14158675309"])).await;
    insert_legacy_trunk(&db, "b", json!(["+14158675309"])).await;

    let report = rustpbx::models::backfill_dids_from_sip_trunks::run(&db).await.unwrap();
    // First insertion succeeds, second is a collision.
    // SQLite returns rows in insertion (rowid) order, so trunk "a" (inserted first)
    // wins and trunk "b" is the collision. If this is ever run against PostgreSQL,
    // run() needs an explicit ORDER BY clause on sip_trunk::Entity::find() to keep
    // the test deterministic.
    assert_eq!(report.inserted, 1);
    assert_eq!(report.collisions, 1);

    // Only one row should exist.
    assert_eq!(did::Model::list_all(&db).await.unwrap().len(), 1);
}

#[tokio::test]
async fn backfill_handles_empty_db() {
    let db = mem_db().await;
    let report = rustpbx::models::backfill_dids_from_sip_trunks::run(&db).await.unwrap();
    assert_eq!(report.inserted, 0);
    assert_eq!(report.invalid, 0);
}

#[tokio::test]
async fn backfill_counts_objects_without_recognized_keys_as_invalid() {
    let db = mem_db().await;
    set_default_country(&db, "US").await;

    insert_legacy_trunk(
        &db,
        "t",
        serde_json::json!([
            { "phone": "+14155551234" },           // unknown key -> invalid
            { "number": "+14158675309" }           // valid
        ]),
    )
    .await;

    let report = rustpbx::models::backfill_dids_from_sip_trunks::run(&db).await.unwrap();
    assert_eq!(report.inserted, 1);
    assert_eq!(report.invalid, 1);
}
