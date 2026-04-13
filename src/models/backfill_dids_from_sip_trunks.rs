use crate::config_merge::read_default_country;
use crate::models::{did, sip_trunk};
use anyhow::Result;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::Value;
use tracing::{info, warn};

/// One-shot backfill from `sip_trunks.did_numbers` JSON into `rustpbx_dids`.
///
/// Idempotent: re-running after a successful backfill does nothing (upserts are skipped
/// when the row already exists with the same owning trunk).
pub async fn run(db: &DatabaseConnection) -> Result<BackfillReport> {
    let region = read_default_country(db).await;
    let trunks = sip_trunk::Entity::find().all(db).await?;
    let mut report = BackfillReport::default();

    for trunk in trunks {
        let Some(items) = trunk.did_numbers.as_ref().and_then(|v| v.as_array()) else {
            continue;
        };
        for item in items {
            let raw = match item {
                Value::String(s) => s.clone(),
                Value::Object(o) => match o
                    .get("number")
                    .or_else(|| o.get("did"))
                    .or_else(|| o.get("value"))
                    .and_then(|v| v.as_str())
                {
                    Some(s) => s.to_owned(),
                    None => {
                        warn!(
                            trunk = %trunk.name,
                            "backfill skipping object DID entry with no number/did/value key"
                        );
                        report.invalid += 1;
                        continue;
                    }
                },
                _ => continue,
            };
            if raw.trim().is_empty() {
                continue;
            }

            match did::normalize_did(&raw, region.as_deref()) {
                Ok(number) => {
                    if let Ok(Some(existing)) = did::Model::get(db, &number).await {
                        if existing.trunk_name.as_deref() != Some(trunk.name.as_str()) {
                            warn!(
                                did = %number,
                                existing_owner = ?existing.trunk_name,
                                attempted_owner = %trunk.name,
                                "backfill collision: DID already owned by another trunk, skipping",
                            );
                            report.collisions += 1;
                        } else {
                            report.idempotent += 1;
                        }
                        continue;
                    }
                    let new = did::NewDid {
                        number: number.clone(),
                        trunk_name: Some(trunk.name.clone()),
                        extension_number: None,
                        failover_trunk: None,
                        label: None,
                        enabled: true,
                    };
                    if let Err(e) = did::Model::upsert(db, new).await {
                        warn!(did = %number, error = %e, "backfill upsert failed");
                        report.errors += 1;
                    } else {
                        report.inserted += 1;
                    }
                }
                Err(e) => {
                    warn!(
                        input = %raw,
                        trunk = %trunk.name,
                        error = %e,
                        "backfill skipping invalid DID"
                    );
                    report.invalid += 1;
                }
            }
        }
    }
    info!(
        inserted = report.inserted,
        idempotent = report.idempotent,
        collisions = report.collisions,
        invalid = report.invalid,
        errors = report.errors,
        "DID backfill complete"
    );
    Ok(report)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BackfillReport {
    pub inserted: u64,
    pub idempotent: u64,
    pub collisions: u64,
    pub invalid: u64,
    pub errors: u64,
}
