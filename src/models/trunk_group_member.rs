//! `rustpbx_trunk_group_members` — join table linking trunk groups to their
//! member gateway names.
//!
//! Phase 2 Plan 02-01 ships this entity alongside
//! [`crate::models::trunk_group`]. `gateway_name` is a soft reference to
//! the sip-trunk name — no FK at the DB layer, validated at the
//! handler layer on write, to decouple from sip-trunk id churn and to
//! keep the legacy table untouched (TRK-01 / MIG-01).

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{integer, string};
use sea_orm_migration::sea_query::{ColumnDef, ForeignKey, ForeignKeyAction};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "rustpbx_trunk_group_members")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    pub trunk_group_id: i64,
    pub gateway_name: String,
    pub weight: i32,
    pub priority: i32,
    pub position: i32,
    /// Phase 13 Plan 01a (TEN-01) — owning sub-account; defaults to 'root'.
    #[sea_orm(default_value = "root")]
    pub account_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::trunk_group::Entity",
        from = "Column::TrunkGroupId",
        to = "super::trunk_group::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    TrunkGroup,
}

impl Related<super::trunk_group::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TrunkGroup.def()
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
                    .col(
                        ColumnDef::new(Column::Id)
                            .big_integer()
                            .primary_key()
                            .auto_increment(),
                    )
                    .col(
                        ColumnDef::new(Column::TrunkGroupId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(string(Column::GatewayName).char_len(120))
                    .col(integer(Column::Weight).default(100))
                    .col(integer(Column::Priority).default(0))
                    .col(integer(Column::Position).default(0))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_rustpbx_trunk_group_members_group_id")
                            .from(Entity, Column::TrunkGroupId)
                            .to(super::trunk_group::Entity, super::trunk_group::Column::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_tg_members_group_gateway")
                    .table(Entity)
                    .col(Column::TrunkGroupId)
                    .col(Column::GatewayName)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_tg_members_gateway_name")
                    .table(Entity)
                    .col(Column::GatewayName)
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
