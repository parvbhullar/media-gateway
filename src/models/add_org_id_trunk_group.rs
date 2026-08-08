//! org-level multi-tenancy — add `org_id` to `rustpbx_trunk_groups`.
//!
//! `trunk_group` is the model behind `/api/v1/trunks` (gateway-based SIP
//! trunk groups) — a separate, unrelated concept from `trunk::Model`
//! (`rustpbx_trunks`, behind `/api/v1/gateways`, the one the org-disable/
//! CPS/concurrent-call enforcement gate reads). `org_id` here is label/
//! attribution only — no relation to enforcement.
//!
//! Nullable (no legacy-row problem, this column never existed before), so
//! no `'default'` sentinel needed — unlike `did`/`extension`/`trunk`/
//! `routing`/`routing_tables`, which keep their existing NOT NULL DEFAULT
//! 'default' columns for backward compatibility. Additive, idempotent
//! (`has_column` guard), no-op `down` per the codebase convention.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_trunk_groups";

        if !manager.has_column(table_name, "org_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(crate::models::trunk_group::Entity)
                        .add_column(
                            ColumnDef::new(crate::models::trunk_group::Column::OrgId)
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_trunk_groups_org_id")
                    .table(crate::models::trunk_group::Entity)
                    .col(crate::models::trunk_group::Column::OrgId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
