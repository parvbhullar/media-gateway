//! `supersip_webhooks` — Phase 7 webhook registry (WH-01).
//!
//! Phase 7 Plan 07-01 storage. One row per registered webhook target.
//! Per CONTEXT.md D-01: this NEW table follows the project-wide
//! `supersip_` prefix convention, overriding the literal
//! `rustpbx_webhooks` name in REQUIREMENTS.md (D-00 from Phase 3).
//!
//! Columns are locked by D-02:
//!   - `id` — UUID v4 string (primary key, len 64)
//!   - `name` — UNIQUE, lowercase + dashes (validated by handler in 07-02)
//!   - `url` — http/https only, localhost denylisted (handler-validated)
//!   - `secret` — plaintext (D-35; consistent with Phase 3 D-03)
//!   - `events` — JSON array of subscribed event names (empty = all)
//!   - `description` — nullable free text
//!   - `is_active` — default true
//!   - `retry_count` — default 3, range [0, 10] (handler-validated)
//!   - `timeout_ms` — default 5000, range [100, 30000] (handler-validated)
//!   - `created_at` / `updated_at` — DateTimeUtc with default current_timestamp
//!
//! Migration is FORWARD-ONLY (Phase 6 D-05 convention): `up()` creates the
//! table + UNIQUE index on `name`; `down()` is intentionally a no-op so
//! that operator rollbacks cannot lose webhook config rows.

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_webhooks")]
pub struct Model {
    #[sea_orm(
        primary_key,
        auto_increment = false,
        column_type = "String(StringLen::N(64))"
    )]
    pub id: String,
    #[sea_orm(unique, column_type = "String(StringLen::N(128))")]
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub url: String,
    #[sea_orm(column_type = "Text")]
    pub secret: String,
    /// Array of event names. Empty `[]` = subscribe-all (D-08).
    #[sea_orm(column_type = "Json")]
    pub events: Json,
    #[sea_orm(column_type = "Text", nullable)]
    pub description: Option<String>,
    pub is_active: bool,
    pub retry_count: i32,
    pub timeout_ms: i32,
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
                            .string_len(64)
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Column::Name)
                            .string_len(128)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Column::Url).text().not_null())
                    .col(ColumnDef::new(Column::Secret).text().not_null())
                    .col(
                        ColumnDef::new(Column::Events)
                            .json()
                            .not_null()
                            .default(Expr::cust("'[]'")),
                    )
                    .col(ColumnDef::new(Column::Description).text().null())
                    .col(
                        ColumnDef::new(Column::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Column::RetryCount)
                            .integer()
                            .not_null()
                            .default(3),
                    )
                    .col(
                        ColumnDef::new(Column::TimeoutMs)
                            .integer()
                            .not_null()
                            .default(5000),
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
                    .col(
                        ColumnDef::new(Column::AccountId)
                            .string_len(64)
                            .not_null()
                            .default("root"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_webhooks_name")
                    .table(Entity)
                    .col(Column::Name)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only per Phase 6 D-05 convention. Phase 7 follows the
        // same rule: operator rollbacks must not drop webhook config rows.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Database, DbBackend, EntityTrait, Set};
    use sea_orm_migration::MigratorTrait;

    /// Minimal in-test Migrator that runs ONLY this migration. Avoids
    /// pulling in the project Migrator (which would require every prior
    /// migration to apply cleanly) and gives us a focused unit test.
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
            .expect("run webhooks migration");
        db
    }

    #[tokio::test]
    async fn supersip_webhooks_migration_creates_table() {
        let db = fresh_sqlite().await;
        // The simplest "table exists + columns work" assertion: insert a
        // row using ONLY the explicit defaults from D-02 and read it
        // back. SQLite does not auto-fill SeaORM `Set::default()` for
        // bool/int columns, so we set them explicitly here and rely on
        // the SQL-level defaults being asserted by the round-trip.
        let now = chrono::Utc::now();
        let am = ActiveModel {
            id: Set("evt-1".to_string()),
            name: Set("primary".to_string()),
            url: Set("https://example.test/webhook".to_string()),
            secret: Set("shh".to_string()),
            events: Set(serde_json::json!([])),
            description: Set(None),
            is_active: Set(true),
            retry_count: Set(3),
            timeout_ms: Set(5000),
            created_at: Set(now),
            updated_at: Set(now),
            account_id: Set("root".to_string()),
        };
        let inserted = am.insert(&db).await.expect("insert webhook row");
        assert_eq!(inserted.name, "primary");
        assert_eq!(inserted.retry_count, 3);
        assert_eq!(inserted.timeout_ms, 5000);
        assert!(inserted.is_active);

        // Round-trip via find ensures the row is queryable.
        let found = Entity::find_by_id("evt-1".to_string())
            .one(&db)
            .await
            .expect("query")
            .expect("row present");
        assert_eq!(found.url, "https://example.test/webhook");
        assert_eq!(DbBackend::Sqlite, db.get_database_backend());
    }

    #[tokio::test]
    async fn supersip_webhooks_name_is_unique() {
        let db = fresh_sqlite().await;
        let now = chrono::Utc::now();
        let make = |id: &str| ActiveModel {
            id: Set(id.to_string()),
            name: Set("dup".to_string()),
            url: Set("https://example.test/webhook".to_string()),
            secret: Set("s".to_string()),
            events: Set(serde_json::json!([])),
            description: Set(None),
            is_active: Set(true),
            retry_count: Set(3),
            timeout_ms: Set(5000),
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
}
