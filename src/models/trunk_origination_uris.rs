//! `supersip_trunk_origination_uris` — ordered list of origination SIP URIs
//! for a trunk group, keyed on (trunk_group_id, uri).
//!
//! Phase 3 Plan 03-01 — TSUB-02 storage. UNIQUE (trunk_group_id, uri) per
//! D-06. Position is auto-assigned by the handler from row-insertion
//! order; SELECT ordering uses `ORDER BY position`.
//!
//! The `supersip_` prefix follows D-00. FK to `rustpbx_trunk_groups.id`
//! crosses prefixes intentionally — rename handled by v2.1 milestone.

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{integer, string, timestamp};
use sea_orm_migration::sea_query::{ColumnDef, ForeignKey, ForeignKeyAction};
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_trunk_origination_uris")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    pub trunk_group_id: i64,
    pub uri: String,
    pub position: i32,
    pub created_at: DateTimeUtc,
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
                    .col(string(Column::Uri).char_len(500))
                    .col(integer(Column::Position).default(0))
                    .col(
                        timestamp(Column::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_supersip_trunk_origination_uris_group_id")
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
                    .name("idx_supersip_trunk_origination_uris_group_uri")
                    .table(Entity)
                    .col(Column::TrunkGroupId)
                    .col(Column::Uri)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_trunk_origination_uris_group_pos")
                    .table(Entity)
                    .col(Column::TrunkGroupId)
                    .col(Column::Position)
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
