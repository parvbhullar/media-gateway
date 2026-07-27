//! Data-repair migration: convert integer 0/1 to JSON `false`/`true` for the
//! boolean fields in `rustpbx_trunks.kind_config` on rows with `kind = 'sip'`.
//!
//! Background
//! ----------
//! `migrate_sip_trunks_to_trunks_unified` packed the previously-typed SIP
//! columns into `kind_config` via SQL `json_object('register_enabled',
//! register_enabled, ...)`. SQLite stores `BOOLEAN` as INTEGER, and
//! `json_object` emits the raw numeric value, so every migrated row ended up
//! with `"register_enabled": 0` instead of `"register_enabled": false`. The
//! Rust `SipTrunkConfig` declared these as `bool`, so serde refused to
//! deserialize and every code path that loads the typed view (health probe,
//! DID backfill, route resolver, console detail page) silently dropped the
//! affected trunks.
//!
//! Fix
//! ---
//! For each `kind = 'sip'` row, rewrite `kind_config` with `json_set` so the
//! two boolean fields are JSON booleans. Idempotent: rows already in the
//! correct shape are no-ops (json_type returns `'true'`/`'false'` for real
//! booleans vs `'integer'` for the broken shape).
//!
//! Dialects: SQLite (json1) and MySQL 5.7+. Postgres uses jsonb_set.

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const BOOL_FIELDS: &[&str] = &["register_enabled", "rewrite_hostport"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("rustpbx_trunks").await? {
            return Ok(());
        }
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();

        for field in BOOL_FIELDS {
            let sql = match backend {
                DbBackend::Sqlite => format!(
                    "UPDATE \"rustpbx_trunks\" \
                       SET kind_config = json_set( \
                             kind_config, \
                             '$.{field}', \
                             json(CASE \
                                    WHEN json_extract(kind_config, '$.{field}') IS NULL THEN 'false' \
                                    WHEN CAST(json_extract(kind_config, '$.{field}') AS INTEGER) <> 0 THEN 'true' \
                                    ELSE 'false' \
                                  END)) \
                     WHERE kind = 'sip' \
                       AND kind_config IS NOT NULL \
                       AND json_type(kind_config, '$.{field}') = 'integer'"
                ),
                DbBackend::MySql => format!(
                    "UPDATE `rustpbx_trunks` \
                       SET kind_config = JSON_SET( \
                             kind_config, \
                             '$.{field}', \
                             CASE WHEN JSON_EXTRACT(kind_config, '$.{field}') <> 0 \
                                  THEN CAST('true' AS JSON) \
                                  ELSE CAST('false' AS JSON) END) \
                     WHERE kind = 'sip' \
                       AND kind_config IS NOT NULL \
                       AND JSON_TYPE(JSON_EXTRACT(kind_config, '$.{field}')) = 'INTEGER'"
                ),
                DbBackend::Postgres => format!(
                    "UPDATE \"rustpbx_trunks\" \
                       SET kind_config = jsonb_set( \
                             kind_config::jsonb, \
                             '{{{field}}}', \
                             to_jsonb((kind_config->>'{field}')::int <> 0)) \
                     WHERE kind = 'sip' \
                       AND kind_config IS NOT NULL \
                       AND jsonb_typeof((kind_config::jsonb)->'{field}') = 'number'"
                ),
            };
            conn.execute(Statement::from_string(backend, sql)).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
