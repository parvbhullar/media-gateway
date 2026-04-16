//! Additive migration: adds `media_config` (nullable `json`) to
//! the trunk-groups table.
//!
//! Phase 3 Plan 03-01 — TSUB-03 storage. Additive only; no data loss
//! risk. Column appears on the Model in trunk_group.rs so a freshly-
//! created DB already has it (trunk_group::Migration's CREATE TABLE
//! includes the column); this migration is the upgrade path for any DB
//! created under Phase 2 schema. Idempotent via has_column guard.
//!
//! Strictly additive: MIG-01 hard constraint. Only touches the
//! trunk-groups table.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_trunk_groups";

        if !manager.has_column(table_name, "media_config").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(super::trunk_group::Entity)
                        .add_column(
                            ColumnDef::new(
                                super::trunk_group::Column::MediaConfig,
                            )
                            .json()
                            .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
