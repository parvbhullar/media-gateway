//! `supersip_applications` — SIP application registry (Phase 13 Plan 13-03).
//!
//! Each row represents one application belonging to a sub-account. An
//! application owns a set of webhook URLs (answer_url, hangup_url, message_url)
//! and auth_headers used when invoking those URLs. DIDs are attached to
//! applications via the `supersip_application_numbers` join table.
//!
//! UUID primary key (string len 36). UNIQUE (account_id, name).

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{boolean, integer, string, string_null, timestamp};
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_applications")]
pub struct Model {
    #[sea_orm(
        primary_key,
        auto_increment = false,
        column_type = "String(StringLen::N(36))"
    )]
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub answer_url: String,
    pub hangup_url: Option<String>,
    pub message_url: Option<String>,
    /// JSON object of auth headers sent with webhook requests.
    #[sea_orm(column_type = "Json")]
    pub auth_headers: Json,
    pub answer_timeout_ms: i32,
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
                    .col(string(Column::Name).char_len(255))
                    .col(ColumnDef::new(Column::AnswerUrl).text().not_null())
                    .col(string_null(Column::HangupUrl))
                    .col(string_null(Column::MessageUrl))
                    .col(
                        ColumnDef::new(Column::AuthHeaders)
                            .json()
                            .not_null()
                            .default(Expr::cust("'{}'")),
                    )
                    .col(
                        integer(Column::AnswerTimeoutMs).default(5000),
                    )
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

        // UNIQUE (account_id, name) — one name per account
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_applications_account_name")
                    .table(Entity)
                    .col(Column::AccountId)
                    .col(Column::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Non-unique index on account_id for list queries
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_applications_account_id")
                    .table(Entity)
                    .col(Column::AccountId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop application_numbers first (FK dependency), then applications.
        manager
            .drop_table(
                Table::drop()
                    .table(super::supersip_application_numbers::Entity)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Entity).to_owned())
            .await
    }
}
