//! task 3.1 — add `org_id` to `rustpbx_extensions` for structural tenant
//! isolation.
//!
//! Additive + idempotent (`has_column` guard). Existing rows get `'default'`
//! via the column default; 3.1b threads the real org_id at write time. Index
//! `(org_id, extension)` backs the `list_by_org`/`get_by_org` lookups. No-op
//! `down` per the codebase convention (additive, never reversed).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table = "rustpbx_extensions";

        if !manager.has_column(table, "org_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(crate::models::extension::Entity)
                        .add_column(
                            ColumnDef::new(crate::models::extension::Column::OrgId)
                                .text()
                                .not_null()
                                .default("default"),
                        )
                        .to_owned(),
                )
                .await?;
        }

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_extensions_org_id")
                    .table(crate::models::extension::Entity)
                    .col(crate::models::extension::Column::OrgId)
                    .col(crate::models::extension::Column::Extension)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
