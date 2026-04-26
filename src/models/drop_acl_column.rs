//! Phase 5 Plan 05-01 — D-11 LOCKED. Drops the legacy `acl` JSON column
//! from `rustpbx_trunk_groups`. Multi-row sub-resource at
//! `supersip_trunk_acl_entries` replaces it (D-10). Forward-only,
//! mirrors `drop_credentials_column.rs` (Phase 3 D-02 precedent).
//!
//! Runs LAST in the Phase 5 schema migration batch so any in-flight
//! reads of the column succeed during deploy until the Migrator catches
//! up.
//!
//! Idempotent via `has_column` guard. The `Column::Acl` enum variant has
//! been removed alongside the Model field, so the alter references the
//! column by name via `Alias::new("acl")`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_trunk_groups";
        if manager.has_column(table_name, "acl").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(super::trunk_group::Entity)
                        .drop_column(Alias::new("acl"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    // Forward-only. Re-creating the acl column would require restoring the
    // JSON shape; v2.1 rollback documentation handles this.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
