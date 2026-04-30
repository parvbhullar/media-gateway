//! `supersip_manipulations` — Phase 9 SIP manipulation rules (MAN-01).
//!
//! Phase 9 Plan 09-01 storage. One row per manipulation class. Per CONTEXT.md
//! D-01..D-04: this NEW table follows the project-wide `supersip_` prefix
//! convention. Forward-only migration (Phase 6 D-05 / Phase 8 convention).
//!
//! Columns are locked by D-03:
//!   - `id` — UUID v4 string (primary key)
//!   - `name` — UNIQUE, lowercase + dashes (handler-validated in 09-02)
//!   - `description` — nullable free text
//!   - `direction` — text ("inbound" | "outbound" | "both"), default "both"
//!   - `priority` — i32 ASC (lower first), default 100
//!   - `is_active` — default true
//!   - `rules` — JSON column storing `Vec<Rule>` (D-02, D-05)
//!   - `created_at` / `updated_at` — DateTimeUtc with default current_timestamp
//!
//! `direction` reuses `crate::models::routing::RoutingDirection` (D-22 from
//! Phase 8) at the application layer; the column itself stores the lowercase
//! string variant so that "both" (no enum variant in `RoutingDirection`) can
//! also be represented for the always-apply case.

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_manipulations")]
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
    /// "inbound" | "outbound" | "both" (D-22). Stored as text — handler
    /// validates the enum at PUT/POST.
    pub direction: String,
    /// Lower = first. Default 100.
    pub priority: i32,
    pub is_active: bool,
    /// Embedded `Vec<Rule>` JSON payload (D-02, D-05). Default `[]`.
    #[sea_orm(column_type = "Json")]
    pub rules: serde_json::Value,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Parse the `direction` column into the runtime enum.
    ///
    /// Returns `None` for the "both" sentinel — the call-site applies the
    /// rule for either direction in that case. Returns `Some(variant)` for
    /// "inbound"/"outbound". Any other value is treated as `None` (defensive
    /// — handlers in 09-02 validate input on write).
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
                        ColumnDef::new(Column::Rules)
                            .json()
                            .not_null()
                            .default(Expr::value(serde_json::json!([]))),
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
                    .name("idx_supersip_manipulations_name")
                    .table(Entity)
                    .col(Column::Name)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only per Phase 6 D-05 / Phase 8 convention. Operator
        // rollbacks must not drop manipulation config rows.
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
            .expect("run manipulations migration");
        db
    }

    #[tokio::test]
    async fn supersip_manipulations_migration_creates_table() {
        let db = fresh_sqlite().await;
        let now = chrono::Utc::now();
        let am = ActiveModel {
            id: Set("man-1".to_string()),
            name: Set("strip-pai".to_string()),
            description: Set(Some("strip PAI on outbound".to_string())),
            direction: Set("outbound".to_string()),
            priority: Set(100),
            is_active: Set(true),
            rules: Set(serde_json::json!([])),
            created_at: Set(now),
            updated_at: Set(now),
        };
        let inserted = am.insert(&db).await.expect("insert manipulation row");
        assert_eq!(inserted.name, "strip-pai");
        assert_eq!(inserted.priority, 100);
        assert!(inserted.is_active);

        let found = Entity::find_by_id("man-1".to_string())
            .one(&db)
            .await
            .expect("query")
            .expect("row present");
        assert_eq!(found.direction, "outbound");
        assert_eq!(
            found.direction_enum(),
            Some(crate::models::routing::RoutingDirection::Outbound)
        );
        assert_eq!(found.rules, serde_json::json!([]));
    }

    #[tokio::test]
    async fn supersip_manipulations_name_is_unique() {
        let db = fresh_sqlite().await;
        let now = chrono::Utc::now();
        let make = |id: &str| ActiveModel {
            id: Set(id.to_string()),
            name: Set("dup".to_string()),
            description: Set(None),
            direction: Set("both".to_string()),
            priority: Set(100),
            is_active: Set(true),
            rules: Set(serde_json::json!([])),
            created_at: Set(now),
            updated_at: Set(now),
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
            direction: "inbound".into(),
            priority: 100,
            is_active: true,
            rules: serde_json::json!([]),
            created_at: now,
            updated_at: now,
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

    #[tokio::test]
    async fn rules_json_roundtrip() {
        let db = fresh_sqlite().await;
        let now = chrono::Utc::now();
        let payload = serde_json::json!([
            {
                "name": "block-anon",
                "conditions": [
                    {"source": "caller_number", "op": "equals", "value": "anonymous"}
                ],
                "condition_mode": "and",
                "actions": [
                    {"type": "hangup", "sip_code": 603, "reason": "Decline"}
                ],
                "anti_actions": []
            }
        ]);
        let am = ActiveModel {
            id: Set("man-rules".to_string()),
            name: Set("rules-test".to_string()),
            description: Set(None),
            direction: Set("inbound".to_string()),
            priority: Set(50),
            is_active: Set(true),
            rules: Set(payload.clone()),
            created_at: Set(now),
            updated_at: Set(now),
        };
        let inserted = am.insert(&db).await.expect("insert");
        assert_eq!(inserted.rules, payload);
    }
}
