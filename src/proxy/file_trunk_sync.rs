//! Promote file-loaded trunks into the `rustpbx_trunks` table.
//!
//! File-trunks (`proxy.trunks_files` glob) historically lived only in memory.
//! The health prober iterates DB rows, so those trunks were never probed and
//! their console badges were stuck at their seed `Healthy` value. This module
//! closes that gap with **insert-only** semantics:
//!
//! - File-trunks not present in the DB get inserted, tagged with
//!   `metadata.source = "file"` for later sweeps.
//! - Existing DB rows are left alone (no update, no delete) — console edits
//!   and live status columns (`status`, `consecutive_*`, `last_health_check_at`)
//!   are never clobbered by a reload.
//! - Trunks whose source file ends in `.generated.toml` are skipped (the
//!   generated file is a DB export, re-promoting would loop).
//! - Embedded (programmatic) trunks are skipped.
//!
//! Individual failures are logged and counted; the sync never aborts the
//! caller's reload.

use std::collections::HashMap;

use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::models::kind_schemas;
use crate::models::sip_trunk::{
    ActiveModel as TrunkActiveModel, Column as TrunkColumn, Entity as TrunkEntity, SipTransport,
    SipTrunkConfig, SipTrunkDirection, SipTrunkStatus,
};
use crate::proxy::routing::{ConfigOrigin, TrunkConfig, TrunkDirection};

#[derive(Debug, Default, Clone)]
pub struct PromoteReport {
    pub considered: usize,
    pub inserted: usize,
    pub skipped_existing: usize,
    pub skipped_embedded: usize,
    pub skipped_generated: usize,
    pub skipped_invalid: usize,
}

pub async fn promote_missing_file_trunks(
    db: &DatabaseConnection,
    trunks: &HashMap<String, TrunkConfig>,
) -> PromoteReport {
    let mut report = PromoteReport::default();

    for (name, trunk) in trunks {
        report.considered += 1;

        let source_path = match &trunk.origin {
            ConfigOrigin::Embedded => {
                report.skipped_embedded += 1;
                continue;
            }
            ConfigOrigin::File(p) => p.clone(),
        };
        if is_generated_path(&source_path) {
            report.skipped_generated += 1;
            continue;
        }

        match TrunkEntity::find()
            .filter(TrunkColumn::Name.eq(name.clone()))
            .one(db)
            .await
        {
            Ok(Some(_)) => {
                report.skipped_existing += 1;
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                warn!(error = %e, name = %name, "file_trunk_sync: lookup failed; skipping");
                report.skipped_invalid += 1;
                continue;
            }
        }

        let kind = trunk.kind.clone();
        let kind_config_json = match build_kind_config(&kind, trunk) {
            Ok(v) => v,
            Err(detail) => {
                warn!(
                    name = %name,
                    kind = %kind,
                    detail = %detail,
                    "file_trunk_sync: kind_config build failed; skipping"
                );
                report.skipped_invalid += 1;
                continue;
            }
        };

        if let Err(e) = kind_schemas::validate(&kind, &kind_config_json) {
            warn!(
                name = %name,
                kind = %kind,
                error = %e,
                "file_trunk_sync: kind_schemas validation failed; skipping"
            );
            report.skipped_invalid += 1;
            continue;
        }

        let direction = match trunk.direction {
            Some(TrunkDirection::Inbound) => SipTrunkDirection::Inbound,
            Some(TrunkDirection::Outbound) => SipTrunkDirection::Outbound,
            Some(TrunkDirection::Bidirectional) | None => SipTrunkDirection::Bidirectional,
        };

        let metadata = json!({ "source": "file", "source_path": source_path });
        let now = Utc::now();
        let am = TrunkActiveModel {
            name: Set(name.clone()),
            kind: Set(kind.clone()),
            display_name: Set(None),
            direction: Set(direction),
            status: Set(SipTrunkStatus::default()),
            is_active: Set(!trunk.disabled.unwrap_or(false)),
            metadata: Set(Some(metadata)),
            consecutive_failures: Set(0),
            consecutive_successes: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            kind_config: Set(kind_config_json),
            ..Default::default()
        };

        match am.insert(db).await {
            Ok(_) => {
                info!(name = %name, kind = %kind, "file_trunk_sync: promoted file trunk into DB");
                report.inserted += 1;
            }
            Err(e) => {
                warn!(
                    name = %name,
                    error = %e,
                    "file_trunk_sync: insert failed; skipping"
                );
                report.skipped_invalid += 1;
            }
        }
    }

    report
}

fn is_generated_path(path: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.ends_with(".generated.toml"))
}

fn build_kind_config(kind: &str, trunk: &TrunkConfig) -> Result<Value, String> {
    if kind == "sip" {
        let dest_host = trunk
            .dest
            .strip_prefix("sip:")
            .or_else(|| trunk.dest.strip_prefix("sips:"))
            .unwrap_or(&trunk.dest)
            .to_string();
        let transport = match trunk.transport.as_deref() {
            Some("tcp") => SipTransport::Tcp,
            Some("tls") => SipTransport::Tls,
            _ => SipTransport::Udp,
        };
        let cfg = SipTrunkConfig {
            sip_server: Some(dest_host),
            sip_transport: transport,
            outbound_proxy: None,
            auth_username: trunk.username.clone(),
            auth_password: trunk.password.clone(),
            register_enabled: trunk.register_enabled.unwrap_or(false),
            register_expires: trunk.register_expires.map(|v| v as i32),
            register_extra_headers: None,
            rewrite_hostport: trunk.rewrite_hostport,
            did_numbers: None,
            incoming_from_user_prefix: trunk.incoming_from_user_prefix.clone(),
            incoming_to_user_prefix: trunk.incoming_to_user_prefix.clone(),
            default_route_label: None,
            billing_snapshot: None,
            analytics: None,
            carrier: None,
        };
        serde_json::to_value(&cfg).map_err(|e| e.to_string())
    } else {
        trunk
            .kind_config
            .clone()
            .ok_or_else(|| format!("trunk kind={} missing kind_config in file", kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::migration::Migrator;
    use sea_orm::Database;
    use sea_orm_migration::MigratorTrait;
    use serde_json::json;

    async fn fresh_db() -> DatabaseConnection {
        crate::proxy::bridge::signaling::register_builtins();
        crate::models::kind_schemas::register_builtins();
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect sqlite memory");
        Migrator::up(&db, None).await.expect("run migrations");
        db
    }

    fn sip_trunk_from_file(_name: &str, path: &str) -> TrunkConfig {
        TrunkConfig {
            dest: "sip:carrier.example.com:5060".to_string(),
            origin: ConfigOrigin::File(path.to_string()),
            kind: "sip".to_string(),
            ..TrunkConfig::default()
        }
    }

    fn webrtc_trunk_from_file(_name: &str, path: &str) -> TrunkConfig {
        let kind_config = json!({
            "signaling": "http_json",
            "endpoint_url": "http://127.0.0.1:7861/offer",
            "health_check_url": "http://127.0.0.1:7861/health",
            "audio_codec": "opus",
            "protocol": {
                "request_body_template": "{\"sdp\":\"{offer_sdp}\",\"type\":\"offer\"}",
                "response_answer_path": "$.sdp",
                "response_session_path": "$.pc_id"
            }
        });
        TrunkConfig {
            dest: "http://127.0.0.1:7861/offer".to_string(),
            origin: ConfigOrigin::File(path.to_string()),
            kind: "webrtc".to_string(),
            kind_config: Some(kind_config),
            ..TrunkConfig::default()
        }
    }

    #[tokio::test]
    async fn inserts_missing_file_trunks() {
        let db = fresh_db().await;
        let mut trunks = HashMap::new();
        trunks.insert(
            "carrier_a".to_string(),
            sip_trunk_from_file("carrier_a", "config/trunks/carriers.toml"),
        );
        trunks.insert(
            "pipecat_bot".to_string(),
            webrtc_trunk_from_file("pipecat_bot", "config/trunks/bots.toml"),
        );

        let report = promote_missing_file_trunks(&db, &trunks).await;
        assert_eq!(report.inserted, 2, "{:?}", report);
        assert_eq!(report.considered, 2);

        let rows = TrunkEntity::find()
            .all(&db)
            .await
            .expect("query trunks");
        assert_eq!(rows.len(), 2);
        let pipecat = rows.iter().find(|r| r.name == "pipecat_bot").unwrap();
        assert_eq!(pipecat.kind, "webrtc");
        assert_eq!(
            pipecat
                .metadata
                .as_ref()
                .and_then(|m| m.get("source"))
                .and_then(|v| v.as_str()),
            Some("file")
        );
    }

    #[tokio::test]
    async fn skips_existing_db_rows_without_overwriting() {
        let db = fresh_db().await;
        let mut trunks = HashMap::new();
        trunks.insert(
            "pipecat_bot".to_string(),
            webrtc_trunk_from_file("pipecat_bot", "config/trunks/bots.toml"),
        );

        // First run inserts.
        let r1 = promote_missing_file_trunks(&db, &trunks).await;
        assert_eq!(r1.inserted, 1);

        // Simulate the prober flipping status to Offline + 5 failures.
        use sea_orm::IntoActiveModel;
        let row = TrunkEntity::find()
            .filter(TrunkColumn::Name.eq("pipecat_bot"))
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        let mut am = row.into_active_model();
        am.status = Set(SipTrunkStatus::Offline);
        am.consecutive_failures = Set(5);
        am.update(&db).await.unwrap();

        // Second run must not touch the row.
        let r2 = promote_missing_file_trunks(&db, &trunks).await;
        assert_eq!(r2.inserted, 0);
        assert_eq!(r2.skipped_existing, 1);

        let after = TrunkEntity::find()
            .filter(TrunkColumn::Name.eq("pipecat_bot"))
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, SipTrunkStatus::Offline);
        assert_eq!(after.consecutive_failures, 5);
    }

    #[tokio::test]
    async fn skips_generated_toml_source() {
        let db = fresh_db().await;
        let mut trunks = HashMap::new();
        trunks.insert(
            "from_db".to_string(),
            sip_trunk_from_file(
                "from_db",
                "config/trunks/trunks.generated.toml",
            ),
        );

        let report = promote_missing_file_trunks(&db, &trunks).await;
        assert_eq!(report.inserted, 0);
        assert_eq!(report.skipped_generated, 1);
        assert_eq!(
            TrunkEntity::find().all(&db).await.unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn skips_embedded_trunks() {
        let db = fresh_db().await;
        let mut trunks = HashMap::new();
        trunks.insert(
            "embedded".to_string(),
            TrunkConfig {
                dest: "sip:x".to_string(),
                origin: ConfigOrigin::Embedded,
                kind: "sip".to_string(),
                ..TrunkConfig::default()
            },
        );
        let report = promote_missing_file_trunks(&db, &trunks).await;
        assert_eq!(report.skipped_embedded, 1);
        assert_eq!(report.inserted, 0);
    }

    #[tokio::test]
    async fn invalid_kind_config_is_logged_and_skipped_but_others_proceed() {
        let db = fresh_db().await;
        let mut trunks = HashMap::new();
        trunks.insert(
            "broken_webrtc".to_string(),
            TrunkConfig {
                dest: "http://x".into(),
                origin: ConfigOrigin::File("config/trunks/bots.toml".into()),
                kind: "webrtc".into(),
                kind_config: None,
                ..TrunkConfig::default()
            },
        );
        trunks.insert(
            "good_sip".to_string(),
            sip_trunk_from_file("good_sip", "config/trunks/carriers.toml"),
        );
        let report = promote_missing_file_trunks(&db, &trunks).await;
        assert_eq!(report.inserted, 1);
        assert_eq!(report.skipped_invalid, 1);
        assert!(
            TrunkEntity::find()
                .filter(TrunkColumn::Name.eq("good_sip"))
                .one(&db)
                .await
                .unwrap()
                .is_some()
        );
    }
}
