//! `supersip_endpoints` — SIP user endpoint registry (Phase 13 Plan 13-02).
//!
//! Each row represents one SIP user endpoint belonging to a sub-account. The
//! `username` + `realm` pair identifies the SIP principal. The `ha1` column
//! stores md5(username:realm:password) — the plaintext password is accepted
//! on write and immediately discarded; it is never stored or returned
//! (D-10). The UUID primary key is exposed on the REST surface (D-11).
//!
//! The `supersip_` prefix follows D-00. No FK to `supersip_sub_accounts` is
//! declared here — Phase 13 avoids cross-prefix FKs in the initial schema
//! and enforces referential integrity at the handler layer.
//!
//! Per D-09: UNIQUE composite index on (account_id, username).

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{boolean, string, string_null, timestamp};
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

/// HA1 = md5(username:realm:password) per D-10.
pub fn compute_ha1(username: &str, realm: &str, password: &str) -> String {
    use md5::{Digest, Md5};
    let input = format!("{}:{}:{}", username, realm, password);
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_endpoints")]
pub struct Model {
    #[sea_orm(
        primary_key,
        auto_increment = false,
        column_type = "String(StringLen::N(36))"
    )]
    pub id: String,
    pub account_id: String,
    pub username: String,
    pub alias: Option<String>,
    pub realm: String,
    /// HA1 = md5(username:realm:password). Never returned in responses (D-10).
    pub ha1: String,
    pub application_id: Option<String>,
    pub enabled: bool,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
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
                            .string_len(36)
                            .not_null()
                            .primary_key(),
                    )
                    .col(string(Column::AccountId).char_len(64))
                    .col(string(Column::Username).char_len(128))
                    .col(string_null(Column::Alias).char_len(128))
                    .col(string(Column::Realm).char_len(255))
                    .col(string(Column::Ha1).char_len(32))
                    .col(string_null(Column::ApplicationId).char_len(36))
                    .col(boolean(Column::Enabled).default(true))
                    .col(
                        timestamp(Column::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        timestamp(Column::UpdatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // UNIQUE (account_id, username) per D-09
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_endpoints_account_username")
                    .table(Entity)
                    .col(Column::AccountId)
                    .col(Column::Username)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Non-unique index on account_id for list queries
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_endpoints_account_id")
                    .table(Entity)
                    .col(Column::AccountId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Entity).to_owned())
            .await
    }
}

