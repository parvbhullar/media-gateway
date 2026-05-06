//! `supersip_security_blocks` — Phase 10 auto-block registry (SEC-03).
//!
//! Phase 10 Plan 10-01 storage. One row per (ip, realm) blocked tuple. Per
//! CONTEXT.md D-09/D-11: brute-force tracker writes a row on threshold breach
//! and operator manually unblocks via `DELETE /api/v1/security/blocks/{ip}`.
//! Forward-only migration (Phase 6 D-05 / Phase 8 convention).
//!
//! Columns:
//!   - `id` — i64 auto_increment primary key
//!   - `ip` — text (canonical string form)
//!   - `realm` — text, default "" (auth realm or empty for non-auth blocks)
//!   - `block_reason` — text ("brute_force" | future reasons)
//!   - `blocked_at` — DateTimeUtc when block was written
//!   - `unblocked_at` — nullable; set on DELETE for audit retention
//!   - `auto_unblock_at` — nullable; reserved for future TTL feature
//!
//! UNIQUE (ip, realm) ensures idempotent writes.

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_query::ColumnDef;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, serde::Serialize, serde::Deserialize)]
#[sea_orm(table_name = "supersip_security_blocks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    pub ip: String,
    pub realm: String,
    pub block_reason: String,
    pub blocked_at: DateTimeUtc,
    #[sea_orm(nullable)]
    pub unblocked_at: Option<DateTimeUtc>,
    #[sea_orm(nullable)]
    pub auto_unblock_at: Option<DateTimeUtc>,
    /// Phase 13 Plan 01a (TEN-01) — owning sub-account; defaults to 'root'.
    #[sea_orm(default_value = "root")]
    pub account_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

// ─── Migration ───────────────────────────────────────────────────────────────

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Entity)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Column::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Column::Ip).string().not_null())
                    .col(
                        ColumnDef::new(Column::Realm)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .col(ColumnDef::new(Column::BlockReason).string().not_null())
                    .col(
                        ColumnDef::new(Column::BlockedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Column::UnblockedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Column::AutoUnblockAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_security_blocks_ip_realm")
                    .table(Entity)
                    .col(Column::Ip)
                    .col(Column::Realm)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only per Phase 6 D-05 / Phase 8 convention.
        Ok(())
    }
}
