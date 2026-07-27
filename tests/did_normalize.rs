use rustpbx::models::did::{DidError, normalize_did};

#[test]
fn normalizes_e164_with_plus_prefix() {
    let out = normalize_did("+14158675309", None).unwrap();
    assert_eq!(out, "+14158675309");
}

#[test]
fn normalizes_local_format_with_default_region() {
    let out = normalize_did("4158675309", Some("US")).unwrap();
    assert_eq!(out, "+14158675309");
}

#[test]
fn strips_punctuation_and_spaces() {
    let out = normalize_did("(415) 867-5309", Some("US")).unwrap();
    assert_eq!(out, "+14158675309");
}

#[test]
fn rejects_empty_input() {
    assert!(matches!(normalize_did("", None), Err(DidError::Empty)));
    assert!(matches!(normalize_did("   ", None), Err(DidError::Empty)));
}

#[test]
fn rejects_local_format_without_default_region() {
    assert!(matches!(
        normalize_did("4158675309", None),
        Err(DidError::MissingRegion)
    ));
}

#[test]
fn rejects_invalid_number() {
    assert!(matches!(
        normalize_did("+1234", Some("US")),
        Err(DidError::InvalidNumber(_))
    ));
}

#[test]
fn round_trip_stable() {
    let once = normalize_did("+14158675309", None).unwrap();
    let twice = normalize_did(&once, None).unwrap();
    assert_eq!(once, twice);
}

#[test]
fn rejects_unknown_region_code() {
    assert!(matches!(
        normalize_did("4158675309", Some("ZZ")),
        Err(DidError::UnknownCountry(_))
    ));
}

// --- CRUD helpers (in-memory sqlite) ------------------------------------

use rustpbx::models::did::{self, Entity as DidEntity, NewDid};
use sea_orm::{Database, DatabaseConnection, EntityTrait};
use sea_orm_migration::MigratorTrait;

async fn mem_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    rustpbx::models::migration::Migrator::up(&db, None).await.unwrap();
    db
}

#[tokio::test]
async fn upsert_then_fetch() {
    let db = mem_db().await;
    did::Model::upsert(
        &db,
        NewDid {
            number: "+14158675309".into(),
            trunk_name: Some("trunk-a".into()),
            extension_number: Some("1001".into()),
            failover_trunk: None,
            label: Some("Main".into()),
            enabled: true,
        },
    )
    .await
    .unwrap();

    let row = DidEntity::find_by_id("+14158675309".to_string())
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.trunk_name.as_deref(), Some("trunk-a"));
    assert_eq!(row.extension_number.as_deref(), Some("1001"));
    assert_eq!(row.label.as_deref(), Some("Main"));
    assert!(row.enabled);
}

#[tokio::test]
async fn upsert_is_idempotent_and_updates_fields() {
    let db = mem_db().await;
    let base = NewDid {
        number: "+14158675309".into(),
        trunk_name: Some("trunk-a".into()),
        extension_number: None,
        failover_trunk: None,
        label: Some("v1".into()),
        enabled: true,
    };
    did::Model::upsert(&db, base.clone()).await.unwrap();
    let updated = NewDid { label: Some("v2".into()), ..base };
    did::Model::upsert(&db, updated).await.unwrap();

    let row = did::Model::get(&db, "+14158675309").await.unwrap().unwrap();
    assert_eq!(row.label.as_deref(), Some("v2"));
    assert_eq!(did::Model::list_all(&db).await.unwrap().len(), 1);
}

#[tokio::test]
async fn list_by_trunk_and_counts() {
    let db = mem_db().await;
    for (n, t, f) in [
        ("+14158675301", "a", None),
        ("+14158675302", "a", None),
        ("+14158675303", "b", Some("a".to_string())),
    ] {
        did::Model::upsert(
            &db,
            NewDid {
                number: n.into(),
                trunk_name: Some(t.into()),
                extension_number: None,
                failover_trunk: f,
                label: None,
                enabled: true,
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(did::Model::list_by_trunk(&db, "a").await.unwrap().len(), 2);
    assert_eq!(did::Model::count_by_trunk(&db, "a").await.unwrap(), 2);
    assert_eq!(did::Model::count_by_trunk(&db, "b").await.unwrap(), 1);
    assert_eq!(did::Model::count_by_failover_trunk(&db, "a").await.unwrap(), 1);
    assert_eq!(did::Model::count_by_failover_trunk(&db, "b").await.unwrap(), 0);
}

#[tokio::test]
async fn null_extension_clears_matching_rows() {
    let db = mem_db().await;
    for n in ["+14158675301", "+14158675302"] {
        did::Model::upsert(
            &db,
            NewDid {
                number: n.into(),
                trunk_name: Some("t".into()),
                extension_number: Some("1001".into()),
                failover_trunk: None,
                label: None,
                enabled: true,
            },
        )
        .await
        .unwrap();
    }
    did::Model::upsert(
        &db,
        NewDid {
            number: "+14158675303".into(),
            trunk_name: Some("t".into()),
            extension_number: Some("1002".into()),
            failover_trunk: None,
            label: None,
            enabled: true,
        },
    )
    .await
    .unwrap();

    let affected = did::Model::null_extension(&db, "1001").await.unwrap();
    assert_eq!(affected, 2);

    let remaining: Vec<_> = did::Model::list_all(&db)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.extension_number.is_some())
        .collect();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].extension_number.as_deref(), Some("1002"));
}

#[tokio::test]
async fn upsert_with_null_trunk() {
    let db = mem_db().await;
    did::Model::upsert(
        &db,
        NewDid {
            number: "+14158675309".into(),
            trunk_name: None,
            extension_number: None,
            failover_trunk: None,
            label: Some("parked".into()),
            enabled: true,
        },
    )
    .await
    .unwrap();
    let row = did::Model::get(&db, "+14158675309").await.unwrap().unwrap();
    assert!(row.trunk_name.is_none());
    assert_eq!(row.label.as_deref(), Some("parked"));
}

#[tokio::test]
async fn count_unassigned_returns_null_trunk_rows() {
    let db = mem_db().await;
    did::Model::upsert(
        &db,
        NewDid {
            number: "+14158675301".into(),
            trunk_name: None,
            extension_number: None,
            failover_trunk: None,
            label: None,
            enabled: true,
        },
    )
    .await
    .unwrap();
    did::Model::upsert(
        &db,
        NewDid {
            number: "+14158675302".into(),
            trunk_name: Some("t".into()),
            extension_number: None,
            failover_trunk: None,
            label: None,
            enabled: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(did::Model::count_unassigned(&db).await.unwrap(), 1);
    assert_eq!(did::Model::count_by_trunk(&db, "t").await.unwrap(), 1);
    assert_eq!(did::Model::list_unassigned(&db).await.unwrap().len(), 1);
}

#[tokio::test]
async fn delete_removes_row() {
    let db = mem_db().await;
    did::Model::upsert(
        &db,
        NewDid {
            number: "+14158675309".into(),
            trunk_name: Some("t".into()),
            extension_number: None,
            failover_trunk: None,
            label: None,
            enabled: true,
        },
    )
    .await
    .unwrap();
    did::Model::delete(&db, "+14158675309").await.unwrap();
    assert!(did::Model::get(&db, "+14158675309").await.unwrap().is_none());
}

// --- settings readers ------------------------------------------------------

#[tokio::test]
async fn read_default_country_returns_none_when_unset() {
    let db = mem_db().await;
    assert!(
        rustpbx::config_merge::read_default_country(&db)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn read_default_country_round_trip() {
    let db = mem_db().await;
    rustpbx::models::system_config::Model::upsert(
        &db,
        rustpbx::config_merge::ROUTING_DEFAULT_COUNTRY_KEY,
        "\"us\"",
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        rustpbx::config_merge::read_default_country(&db)
            .await
            .as_deref(),
        Some("US")
    );
}

#[tokio::test]
async fn read_default_country_returns_none_for_blank() {
    let db = mem_db().await;
    rustpbx::models::system_config::Model::upsert(
        &db,
        rustpbx::config_merge::ROUTING_DEFAULT_COUNTRY_KEY,
        "\"\"",
        false,
    )
    .await
    .unwrap();
    assert!(
        rustpbx::config_merge::read_default_country(&db)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn read_did_strict_mode_defaults_false() {
    let db = mem_db().await;
    assert!(!rustpbx::config_merge::read_did_strict_mode(&db).await);
    rustpbx::models::system_config::Model::upsert(
        &db,
        rustpbx::config_merge::ROUTING_DID_STRICT_MODE_KEY,
        "true",
        false,
    )
    .await
    .unwrap();
    assert!(rustpbx::config_merge::read_did_strict_mode(&db).await);
}

#[tokio::test]
async fn read_did_strict_mode_ignores_garbage() {
    let db = mem_db().await;
    rustpbx::models::system_config::Model::upsert(
        &db,
        rustpbx::config_merge::ROUTING_DID_STRICT_MODE_KEY,
        "not-a-bool",
        false,
    )
    .await
    .unwrap();
    assert!(!rustpbx::config_merge::read_did_strict_mode(&db).await);
}
