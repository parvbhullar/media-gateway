//! Adds the missing `last_health_check_at` column to the `rustpbx_trunks`
//! table. The model and the gateway-health monitor have always referenced
//! this column, but no prior migration created it — fresh databases (e.g.
//! the integration-test SQLite fixtures) fail with `no such column` when
//! the proxy data context tries to load trunks.
//!
//! Idempotent: only adds the column if it isn't already present. Targets
//! the post-rename table name (`rustpbx_trunks`); registered after
//! `migrate_sip_trunks_to_trunks_unified`.

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

const TABLE: &str = "rustpbx_trunks";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table(TABLE).await? {
            return Ok(());
        }
        if manager.has_column(TABLE, "last_health_check_at").await? {
            return Ok(());
        }
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        let sql = match backend {
            DbBackend::Sqlite | DbBackend::Postgres => format!(
                "ALTER TABLE \"{TABLE}\" ADD COLUMN \"last_health_check_at\" TIMESTAMP WITH TIME ZONE"
            ),
            DbBackend::MySql => format!(
                "ALTER TABLE `{TABLE}` ADD COLUMN `last_health_check_at` TIMESTAMP NULL"
            ),
        };
        conn.execute(Statement::from_string(backend, sql)).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
