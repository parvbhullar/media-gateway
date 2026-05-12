use rustpbx::models::{did, migration};
use rustpbx::proxy::routing::did_index::{DidEntry, DidIndex};
use rustpbx::proxy::routing::matcher::{DidLookup, did_lookup_result};
use sea_orm::Database;
use sea_orm_migration::MigratorTrait;
use std::collections::HashMap;

fn idx_with(entries: Vec<DidEntry>) -> DidIndex {
    let mut map = HashMap::new();
    for e in entries {
        map.insert(e.number.clone(), e);
    }
    DidIndex::from_map_for_test(map)
}

async fn mem_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    migration::Migrator::up(&db, None).await.unwrap();
    db
}

#[tokio::test]
async fn loads_and_looks_up_by_number() {
    let db = mem_db().await;
    did::Model::upsert(
        &db,
        did::NewDid {
            number: "+14158675309".into(),
            trunk_name: Some("trunk-a".into()),
            extension_number: Some("1001".into()),
            failover_trunk: None,
            label: None,
            enabled: true,
        },
    )
    .await
    .unwrap();

    let idx = DidIndex::load(&db).await.unwrap();
    let hit = idx.lookup("+14158675309").unwrap();
    assert_eq!(hit.trunk_name.as_deref(), Some("trunk-a"));
    assert_eq!(hit.extension_number.as_deref(), Some("1001"));
    assert!(idx.lookup("+19999999999").is_none());
}

#[tokio::test]
async fn disabled_rows_kept_in_index_for_hard_reject() {
    // Per commit caa27f5 (hard-reject disabled DIDs), disabled rows must
    // remain in the index — the matcher reads `enabled` and returns a
    // 403 reject. Filtering them out would cause silent fall-through to
    // rule-based routing.
    let db = mem_db().await;
    did::Model::upsert(
        &db,
        did::NewDid {
            number: "+14158675309".into(),
            trunk_name: Some("t".into()),
            extension_number: None,
            failover_trunk: None,
            label: None,
            enabled: false,
        },
    )
    .await
    .unwrap();
    let idx = DidIndex::load(&db).await.unwrap();
    let hit = idx.lookup("+14158675309").expect("disabled row must be present");
    assert!(!hit.enabled, "disabled row must carry enabled=false");
}

#[tokio::test]
async fn load_empty_db_returns_empty_index() {
    let db = mem_db().await;
    let idx = DidIndex::load(&db).await.unwrap();
    assert!(idx.is_empty());
    assert_eq!(idx.len(), 0);
}

#[test]
fn known_did_with_matching_trunk_and_extension_short_circuits() {
    let idx = idx_with(vec![DidEntry {
        number: "+14158675309".into(),
        trunk_name: Some("trunk-a".into()),
        extension_number: Some("1001".into()),
        failover_trunk: None,
        enabled: true,
    }]);
    let r = did_lookup_result(&idx, Some("US"), "+14158675309", "trunk-a", false);
    assert!(matches!(r, DidLookup::ShortCircuitExtension(ref ext) if ext == "1001"));
}

#[test]
fn known_did_wrong_trunk_in_strict_mode_rejects() {
    let idx = idx_with(vec![DidEntry {
        number: "+14158675309".into(),
        trunk_name: Some("trunk-a".into()),
        extension_number: None,
        failover_trunk: None,
        enabled: true,
    }]);
    let r = did_lookup_result(&idx, Some("US"), "+14158675309", "trunk-b", true);
    assert!(matches!(r, DidLookup::Reject(_)));
}

#[test]
fn known_did_wrong_trunk_in_loose_mode_falls_through() {
    let idx = idx_with(vec![DidEntry {
        number: "+14158675309".into(),
        trunk_name: Some("trunk-a".into()),
        extension_number: Some("1001".into()),
        failover_trunk: None,
        enabled: true,
    }]);
    let r = did_lookup_result(&idx, Some("US"), "+14158675309", "trunk-b", false);
    assert!(matches!(r, DidLookup::FallThrough));
}

#[test]
fn known_did_correct_trunk_no_extension_falls_through_with_context() {
    let idx = idx_with(vec![DidEntry {
        number: "+14158675309".into(),
        trunk_name: Some("trunk-a".into()),
        extension_number: None,
        failover_trunk: None,
        enabled: true,
    }]);
    let r = did_lookup_result(&idx, Some("US"), "+14158675309", "trunk-a", true);
    assert!(matches!(
        r,
        DidLookup::KnownNoExtension { ref number, .. } if number == "+14158675309"
    ));
}

#[test]
fn unknown_did_falls_through() {
    let idx = idx_with(vec![]);
    let r = did_lookup_result(&idx, Some("US"), "+19995550000", "trunk-a", true);
    assert!(matches!(r, DidLookup::FallThrough));
}

#[test]
fn unparseable_to_user_falls_through() {
    let idx = idx_with(vec![]);
    let r = did_lookup_result(&idx, Some("US"), "anonymous", "trunk-a", true);
    assert!(matches!(r, DidLookup::FallThrough));
}

#[test]
fn unassigned_did_falls_through_even_in_strict_mode() {
    let idx = idx_with(vec![DidEntry {
        number: "+14158675309".into(),
        trunk_name: None,
        extension_number: Some("1001".into()),
        failover_trunk: None,
        enabled: true,
    }]);
    let r = did_lookup_result(&idx, Some("US"), "+14158675309", "any-trunk", true);
    assert!(matches!(r, DidLookup::FallThrough));
}

#[test]
fn local_format_normalizes_to_match_index() {
    let idx = idx_with(vec![DidEntry {
        number: "+14158675309".into(),
        trunk_name: Some("trunk-a".into()),
        extension_number: Some("1001".into()),
        failover_trunk: None,
        enabled: true,
    }]);
    // callee_user is local-format; default country upgrades it to E.164 for lookup.
    let r = did_lookup_result(&idx, Some("US"), "4158675309", "trunk-a", false);
    assert!(matches!(r, DidLookup::ShortCircuitExtension(ref ext) if ext == "1001"));
}
