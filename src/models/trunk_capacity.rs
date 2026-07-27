//! `supersip_trunk_capacity` — single-row capacity limit sub-resource for a
//! trunk group, keyed on a UNIQUE FK to `rustpbx_trunk_groups.id`.
//!
//! Phase 5 Plan 05-01 — TSUB-04 storage. UNIQUE (trunk_group_id) per D-01:
//! at most one capacity row per trunk_group. `max_calls` and `max_cps` are
//! NULL-able (D-04: NULL = unlimited).
//!
//! The `supersip_` prefix follows D-00. FK references the legacy-prefixed
//! parent `rustpbx_trunk_groups.id` — cross-prefix FK, identical to Phase 3
//! sub-resource pattern.

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::timestamp;
use sea_orm_migration::sea_query::{ColumnDef, ForeignKey, ForeignKeyAction};
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_trunk_capacity")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    pub trunk_group_id: i64,
    pub max_calls: Option<i32>,
    pub max_cps: Option<i32>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
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
                    .col(ColumnDef::new(Column::MaxCalls).integer().null())
                    .col(ColumnDef::new(Column::MaxCps).integer().null())
                    .col(
                        timestamp(Column::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        timestamp(Column::UpdatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_supersip_trunk_capacity_group_id")
                            .from(Entity, Column::TrunkGroupId)
                            .to(
                                super::trunk_group::Entity,
                                super::trunk_group::Column::Id,
                            )
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
                    .name("idx_supersip_trunk_capacity_group_id")
                    .table(Entity)
                    .col(Column::TrunkGroupId)
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
