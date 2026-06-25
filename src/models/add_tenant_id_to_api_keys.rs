//! task 3.2 — add `tenant_id` to `rustpbx_api_keys` to scope API keys per
//! organization.
//!
//! Additive + idempotent (`has_column` guard). Existing keys seed to `1` (the
//! bootstrap admin tenant) via the column default. The auth middleware reads
//! this column and attaches it to request extensions as the tenant/org source.
//!
//! DEVIATION FROM DESIGN: the doc called for dropping the single-field unique
//! indexes on `(name)` / `(hash_sha256)` and replacing them with composite
//! `(tenant_id, name)` / `(tenant_id, hash_sha256)` uniques. That is not
//! SQLite-safe here — the base table (`api_key.rs`) declares `name` and
//! `hash_sha256` with column-level `UNIQUE`, and SQLite cannot drop a
//! column-level constraint without rebuilding the table. So we keep `name`
//! and `hash_sha256` GLOBALLY unique (a random 32-byte token hash is globally
//! unique anyway, and a globally-unique key *name* is an acceptable operator
//! constraint) and add a non-unique `(tenant_id, name)` index for the
//! list-by-tenant lookup. Per-tenant *name reuse* (same name in two tenants)
//! is therefore not supported yet; relaxing it needs a table-rebuild migration.
//!
//! No-op `down` per the codebase convention (additive, never reversed).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table = "rustpbx_api_keys";

        if !manager.has_column(table, "tenant_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(crate::models::api_key::Entity)
                        .add_column(
                            ColumnDef::new(crate::models::api_key::Column::TenantId)
                                .big_integer()
                                .not_null()
                                .default(1),
                        )
                        .to_owned(),
                )
                .await?;
        }

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_api_keys_tenant_name")
                    .table(crate::models::api_key::Entity)
                    .col(crate::models::api_key::Column::TenantId)
                    .col(crate::models::api_key::Column::Name)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
