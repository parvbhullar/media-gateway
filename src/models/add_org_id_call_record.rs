//! org-level multi-tenancy — add `org_id` to `rustpbx_call_records`.
//!
//! Populated at call-write time from whichever resource resolved the call
//! (DID for inbound, trunk for outbound, extension for extension-originated).
//! Nullable — NULL for calls with no resolvable org (matches the legacy
//! `org_id = "default"` sentinel used on `did`/`trunk`/`extension`; this
//! column stores true NULL instead, since it's new and has no legacy-row
//! problem to preserve). Additive, idempotent, no-op `down`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_call_records";

        if !manager.has_column(table_name, "org_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(crate::models::call_record::Entity)
                        .add_column(
                            ColumnDef::new(crate::models::call_record::Column::OrgId)
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
                    .name("idx_rustpbx_call_records_org_id")
                    .table(crate::models::call_record::Entity)
                    .col(crate::models::call_record::Column::OrgId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
