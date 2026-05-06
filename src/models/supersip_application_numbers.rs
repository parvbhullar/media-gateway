//! `supersip_application_numbers` — DID↔Application join table (Phase 13 Plan 13-03).
//!
//! Composite PK (application_id, did_id). UNIQUE INDEX on did_id enforces
//! that each DID belongs to at most one application at a time.
//!
//! FK application_id → supersip_applications.id ON DELETE CASCADE.
//! FK did_id → rustpbx_dids.number ON DELETE CASCADE.
//!
//! Note: rustpbx_dids uses `number` (TEXT) as its PK, not a UUID.

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{string, timestamp};
use sea_orm_migration::sea_query::{ColumnDef, ForeignKey, ForeignKeyAction};
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_application_numbers")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub application_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub did_id: String,
    pub account_id: String,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::supersip_applications::Entity",
        from = "Column::ApplicationId",
        to = "super::supersip_applications::Column::Id",
        on_delete = "Cascade",
        on_update = "Cascade"
    )]
    Application,
    #[sea_orm(
        belongs_to = "super::did::Entity",
        from = "Column::DidId",
        to = "super::did::Column::Number",
        on_delete = "Cascade",
        on_update = "Cascade"
    )]
    Did,
}

impl Related<super::supersip_applications::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Application.def()
    }
}

impl Related<super::did::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Did.def()
    }
}

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
                    .col(ColumnDef::new(Column::ApplicationId).string_len(36).not_null())
                    .col(ColumnDef::new(Column::DidId).text().not_null())
                    .primary_key(
                        sea_query::Index::create()
                            .col(Column::ApplicationId)
                            .col(Column::DidId)
                            .primary(),
                    )
                    .col(string(Column::AccountId).char_len(64))
                    .col(
                        timestamp(Column::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_supersip_app_numbers_application_id")
                            .from(Entity, Column::ApplicationId)
                            .to(
                                super::supersip_applications::Entity,
                                super::supersip_applications::Column::Id,
                            )
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_supersip_app_numbers_did_id")
                            .from(Entity, Column::DidId)
                            .to(
                                super::did::Entity,
                                super::did::Column::Number,
                            )
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // UNIQUE on did_id — one DID per application at most
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_application_numbers_did_id")
                    .table(Entity)
                    .col(Column::DidId)
                    .unique()
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
