//! Phase 3 Plan 03-01 — D-02 LOCKED. Drops the Phase 2 `credentials` JSON
//! column from the trunk-groups table. Sub-resource at
//! supersip_trunk_credentials replaces it. Destructive but safe: Phase 2
//! is unmerged on the sip_fix branch and no production rows exist with
//! credentials populated.
//!
//! Runs LAST in the Phase 3 schema migration batch so any in-flight
//! reads of the column succeed during deploy until the Migrator catches
//! up.
//!
//! Idempotent via has_column guard.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_trunk_groups";
        if manager.has_column(table_name, "credentials").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(super::trunk_group::Entity)
                        .drop_column(Alias::new("credentials"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    // Forward-only. Re-creating credentials column would require restoring
    // the JSON shape; v2.1 rollback documentation handles this.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
