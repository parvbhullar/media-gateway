//! task 3.1 reconcile — backfill `org_id` from the legacy `owner` column on
//! `rustpbx_routes`, then drop `owner`.
//!
//! Runs AFTER `add_org_id_routing` (which adds the `org_id` column). Existing
//! rows take their old `owner` value as `org_id` (`NULL` → `'default'`); the
//! free-text `owner` column is then dropped, leaving `org_id` as the canonical
//! tenant key. Idempotent: guarded on `has_column("rustpbx_routes","owner")`,
//! so a re-run after the drop is a no-op. No-op `down` per the codebase
//! convention (additive/forward-only).
//!
//! `COALESCE(owner, 'default')` is portable across SQLite / MySQL / Postgres,
//! and `rustpbx_routes` / `org_id` / `owner` are plain identifiers needing no
//! dialect-specific quoting. The column drop uses the schema builder so
//! sea-orm emits the dialect-appropriate `ALTER TABLE ... DROP COLUMN`.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table = "rustpbx_routes";

        // Only reconcile while the legacy column still exists (idempotent).
        if !manager.has_column(table, "owner").await? {
            return Ok(());
        }

        let conn = manager.get_connection();
        let backend = conn.get_database_backend();

        // org_id already exists (add_org_id_routing); seed it from owner.
        conn.execute(Statement::from_string(
            backend,
            "UPDATE rustpbx_routes SET org_id = COALESCE(owner, 'default')".to_owned(),
        ))
        .await?;

        // Drop the superseded free-text owner column.
        manager
            .alter_table(
                Table::alter()
                    .table(crate::models::routing::Entity)
                    .drop_column(sea_query::Alias::new("owner"))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
