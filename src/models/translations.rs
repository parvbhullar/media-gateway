//! `supersip_translations` — Phase 8 number-translation rules (TRN-01).
//!
//! Phase 8 Plan 08-01 storage. One row per translation rule. Per CONTEXT.md
//! D-01..D-04: this NEW table follows the project-wide `supersip_` prefix
//! convention. Forward-only migration (Phase 6 D-05 convention).
//!
//! Columns are locked by D-02:
//!   - `id` — UUID v4 string (primary key)
//!   - `name` — UNIQUE, lowercase + dashes (handler-validated in 08-02)
//!   - `description` — nullable free text
//!   - `caller_pattern` — nullable regex source (length cap enforced in handler per D-21)
//!   - `destination_pattern` — nullable regex source
//!   - `caller_replacement` — nullable replacement template
//!   - `destination_replacement` — nullable replacement template
//!   - `direction` — text ("inbound" | "outbound" | "both"), default "both" (D-22)
//!   - `priority` — i32 ASC (lower first), default 100
//!   - `is_active` — default true
//!   - `created_at` / `updated_at` — DateTimeUtc with default current_timestamp
//!
//! `direction` reuses `crate::models::routing::RoutingDirection` (D-22) at
//! the application layer; the column itself stores the lowercase string
//! variant so that "both" (no enum variant in `RoutingDirection`) can also
//! be represented for the always-apply case.

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_translations")]
pub struct Model {
    #[sea_orm(
        primary_key,
        auto_increment = false,
        column_type = "String(StringLen::N(64))"
    )]
    pub id: String,
    #[sea_orm(unique, column_type = "String(StringLen::N(128))")]
    pub name: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub description: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub caller_pattern: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub destination_pattern: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub caller_replacement: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub destination_replacement: Option<String>,
    /// "inbound" | "outbound" | "both" (D-22). Stored as text — handler
    /// validates the enum at PUT/POST.
    pub direction: String,
    /// Lower = first. Default 100.
    pub priority: i32,
    pub is_active: bool,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    /// Phase 13 Plan 01a (TEN-01) — owning sub-account; defaults to 'root'.
    #[sea_orm(default_value = "root")]
    pub account_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Parse the `direction` column into the runtime enum (D-22).
    ///
    /// Returns `None` for the "both" sentinel — the call-site applies the
    /// rule for either direction in that case. Returns `Some(variant)` for
    /// "inbound"/"outbound". Any other value is treated as `None` (defensive
    /// — handlers in 08-02 validate input on write).
    pub fn direction_enum(&self) -> Option<crate::models::routing::RoutingDirection> {
        match self.direction.as_str() {
            "inbound" => Some(crate::models::routing::RoutingDirection::Inbound),
            "outbound" => Some(crate::models::routing::RoutingDirection::Outbound),
            _ => None,
        }
    }
}

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
                            .string_len(64)
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Column::Name)
                            .string_len(128)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Column::Description).text().null())
                    .col(ColumnDef::new(Column::CallerPattern).text().null())
                    .col(ColumnDef::new(Column::DestinationPattern).text().null())
                    .col(ColumnDef::new(Column::CallerReplacement).text().null())
                    .col(ColumnDef::new(Column::DestinationReplacement).text().null())
                    .col(
                        ColumnDef::new(Column::Direction)
                            .string()
                            .not_null()
                            .default("both"),
                    )
                    .col(
                        ColumnDef::new(Column::Priority)
                            .integer()
                            .not_null()
                            .default(100),
                    )
                    .col(
                        ColumnDef::new(Column::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
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
                    .name("idx_supersip_translations_name")
                    .table(Entity)
                    .col(Column::Name)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only per Phase 6 D-05 convention. Operator rollbacks must
        // not drop translation config rows.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Database, EntityTrait, Set};
    use sea_orm_migration::MigratorTrait;

    /// Minimal in-test Migrator that runs ONLY this migration.
    struct TestMigrator;

    #[async_trait::async_trait]
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![Box::new(Migration)]
        }
    }

    async fn fresh_sqlite() -> sea_orm::DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("open sqlite memory db");
        TestMigrator::up(&db, None)
            .await
            .expect("run translations migration");
        db
    }

    #[tokio::test]
    async fn supersip_translations_migration_creates_table() {
        let db = fresh_sqlite().await;
        let now = chrono::Utc::now();
        let am = ActiveModel {
            id: Set("trn-1".to_string()),
            name: Set("strip-plus".to_string()),
            description: Set(Some("strip leading +".to_string())),
            caller_pattern: Set(Some(r"^\+(\d+)$".to_string())),
            destination_pattern: Set(None),
            caller_replacement: Set(Some("$1".to_string())),
            destination_replacement: Set(None),
            direction: Set("both".to_string()),
            priority: Set(100),
            is_active: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            account_id: Set("root".to_string()),
        };
        let inserted = am.insert(&db).await.expect("insert translation row");
        assert_eq!(inserted.name, "strip-plus");
        assert_eq!(inserted.priority, 100);
        assert!(inserted.is_active);

        let found = Entity::find_by_id("trn-1".to_string())
            .one(&db)
            .await
            .expect("query")
            .expect("row present");
        assert_eq!(found.caller_pattern.as_deref(), Some(r"^\+(\d+)$"));
        // direction "both" maps to None in the runtime enum.
        assert!(found.direction_enum().is_none());
    }

    #[tokio::test]
    async fn supersip_translations_name_is_unique() {
        let db = fresh_sqlite().await;
        let now = chrono::Utc::now();
        let make = |id: &str| ActiveModel {
            id: Set(id.to_string()),
            name: Set("dup".to_string()),
            description: Set(None),
            caller_pattern: Set(None),
            destination_pattern: Set(None),
            caller_replacement: Set(None),
            destination_replacement: Set(None),
            direction: Set("both".to_string()),
            priority: Set(100),
            is_active: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            account_id: Set("root".to_string()),
        };
        make("a").insert(&db).await.expect("first row inserts");
        let dup_err = make("b").insert(&db).await;
        assert!(
            dup_err.is_err(),
            "duplicate name should violate UNIQUE index"
        );
    }

    #[tokio::test]
    async fn direction_enum_parses_inbound_outbound() {
        let now = chrono::Utc::now();
        let inbound = Model {
            id: "i".into(),
            name: "i".into(),
            description: None,
            caller_pattern: None,
            destination_pattern: None,
            caller_replacement: None,
            destination_replacement: None,
            direction: "inbound".into(),
            priority: 100,
            is_active: true,
            created_at: now,
            updated_at: now,
            account_id: "root".to_string(),
        };
        assert_eq!(
            inbound.direction_enum(),
            Some(crate::models::routing::RoutingDirection::Inbound)
        );
        let outbound = Model {
            direction: "outbound".into(),
            ..inbound.clone()
        };
        assert_eq!(
            outbound.direction_enum(),
            Some(crate::models::routing::RoutingDirection::Outbound)
        );
        let both = Model {
            direction: "both".into(),
            ..inbound
        };
        assert!(both.direction_enum().is_none());
    }
}
