//! `supersip_security_rules` — Phase 10 global firewall rules (SEC-01).
//!
//! Phase 10 Plan 10-01 storage. One row per CIDR rule. Per CONTEXT.md D-01:
//! follows the project-wide `supersip_` prefix convention. Forward-only
//! migration (Phase 6 D-05 / Phase 8 convention). Rule grammar reuses Phase 5
//! trunk ACL semantics: `allow <CIDR|IP|all>` / `deny <CIDR|IP|all>`.
//!
//! Columns are locked by 10-CONTEXT.md "Claude's Discretion":
//!   - `id` — i64 auto_increment primary key
//!   - `position` — i32 ASC ordering (lower first), default 0
//!   - `action` — text ("allow" | "deny")
//!   - `cidr` — text ("all" | IP | CIDR)
//!   - `description` — nullable free text
//!   - `created_at` / `updated_at` — DateTimeUtc with default current_timestamp
//!
//! Evaluation order is by position ASC; first match wins. Default policy is
//! `allow all` when no rules match (Phase 5 D-14 parity).

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_security_rules")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    pub position: i32,
    pub action: String,
    pub cidr: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub description: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
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
                    .col(
                        ColumnDef::new(Column::Position)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(Column::Action).string().not_null())
                    .col(ColumnDef::new(Column::Cidr).string().not_null())
                    .col(ColumnDef::new(Column::Description).text().null())
                    .col(
                        ColumnDef::new(Column::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Column::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_security_rules_position")
                    .table(Entity)
                    .col(Column::Position)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only per Phase 6 D-05 / Phase 8 convention.
        Ok(())
    }
}
