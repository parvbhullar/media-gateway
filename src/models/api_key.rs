use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::{ColumnDef as MigrationColumnDef, *};
use serde::{Deserialize, Serialize};

/// SeaORM entity backing the `/api/v1` Bearer-token authentication table.
///
/// API keys are issued by the CLI as `rpbx_<64-hex>` strings. Only the
/// lowercase hex SHA-256 of the plaintext token is stored — the plaintext
/// is displayed once at creation time and never again.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "rustpbx_api_keys")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    #[sea_orm(unique)]
    pub name: String,
    /// Lowercase hex SHA-256 of the plaintext token. 64 chars.
    #[sea_orm(unique)]
    pub hash_sha256: String,
    pub description: Option<String>,
    pub created_at: DateTimeUtc,
    pub last_used_at: Option<DateTimeUtc>,
    pub revoked_at: Option<DateTimeUtc>,
    /// Phase 13 Plan 01a (TEN-01) — owning sub-account; defaults to 'root'.
    #[sea_orm(default_value = "root")]
    pub account_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

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
                        MigrationColumnDef::new(Column::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::Name)
                            .string_len(120)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::HashSha256)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::Description)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::LastUsedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::RevokedAt)
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
                    .name("idx_rustpbx_api_keys_hash")
                    .table(Entity)
                    .col(Column::HashSha256)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_api_keys_name")
                    .table(Entity)
                    .col(Column::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Entity).to_owned())
            .await
    }
}
