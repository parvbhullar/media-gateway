//! Additive migration: adds `trunk_group_name` (nullable `string(120)`) to
//! `rustpbx_dids`.
//!
//! Forward reference for Phase 3+ DID → trunk_group routing. The column
//! exists in Phase 2 so the TRK-04 engagement check has a real target to
//! scan, but no Phase 2 handler populates it. Phase 1 DID handlers are
//! untouched because the column is nullable and the `NewDid` struct does
//! not carry it — existing upsert `update_columns(...)` deliberately
//! leaves the column alone.
//!
//! Strictly additive: MIG-01 hard constraint. Only touches `rustpbx_dids`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_dids";

        if !manager.has_column(table_name, "trunk_group_name").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(super::did::Entity)
                        .add_column(
                            ColumnDef::new(super::did::Column::TrunkGroupName)
                                .string_len(120)
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
