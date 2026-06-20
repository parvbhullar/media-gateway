//! `supersip_webhook_outbox` — durable webhook delivery queue (task 2.1).
//!
//! The broadcast-channel processor drops events on `RecvError::Lagged`, and any
//! in-flight retry in `deliver_webhook` dies on process restart — so a
//! `call.completed` (the revenue event) can be lost. This table is the durable
//! record: the processor inserts one `pending` row per (webhook, event) fan-out
//! *before* spawning delivery; `deliver_webhook` advances it to `delivered` or
//! `failed`; and a redelivery worker re-drives `pending` rows whose
//! `next_retry_at` lease has expired (including rows orphaned by a crash).
//!
//! Redelivery is safe to repeat: the stable `event_id` (task 2.2) lets the
//! downstream receiver (unpod, task 2.5) dedup, so a double-send is absorbed
//! rather than double-counted. `next_retry_at` acts as a lease only to minimise
//! wasteful re-sends, not for correctness.
//!
//! Migration is FORWARD-ONLY (Phase 6 D-05): `up()` creates the table + a
//! `(status, next_retry_at)` scan index; `down()` is a no-op so a rollback
//! cannot lose undelivered rows.

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

/// Status values (stored as text, validated by the application layer).
pub const STATUS_PENDING: &str = "pending";
pub const STATUS_DELIVERED: &str = "delivered";
pub const STATUS_FAILED: &str = "failed";

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_webhook_outbox")]
pub struct Model {
    /// Outbox row id (uuid v4) — distinct from `event_id` (one event fans out
    /// to N webhooks, hence N rows sharing one `event_id`).
    #[sea_orm(
        primary_key,
        auto_increment = false,
        column_type = "String(StringLen::N(64))"
    )]
    pub id: String,
    #[sea_orm(column_type = "String(StringLen::N(64))")]
    pub webhook_id: String,
    /// Stable per-event id (task 2.2); the downstream dedup key.
    #[sea_orm(column_type = "String(StringLen::N(128))")]
    pub event_id: String,
    #[sea_orm(column_type = "String(StringLen::N(64))")]
    pub event_name: String,
    /// Serialized D-07 envelope body, stored so redelivery needs no replay of
    /// the original event.
    #[sea_orm(column_type = "Text")]
    pub envelope: String,
    /// `pending` | `delivered` | `failed`.
    #[sea_orm(column_type = "String(StringLen::N(16))")]
    pub status: String,
    pub attempt_count: i32,
    #[sea_orm(column_type = "Text", nullable)]
    pub last_error: Option<String>,
    /// Lease: the worker only re-drives a `pending` row once this passes.
    pub next_retry_at: DateTimeUtc,
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
                            .string_len(64)
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Column::WebhookId)
                            .string_len(64)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Column::EventId)
                            .string_len(128)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Column::EventName)
                            .string_len(64)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Column::Envelope).text().not_null())
                    .col(
                        ColumnDef::new(Column::Status)
                            .string_len(16)
                            .not_null()
                            .default("pending"),
                    )
                    .col(
                        ColumnDef::new(Column::AttemptCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(Column::LastError).text().null())
                    .col(
                        ColumnDef::new(Column::NextRetryAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
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

        // Worker scan index: `WHERE status = 'pending' AND next_retry_at <= now`.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_webhook_outbox_status_next_retry")
                    .table(Entity)
                    .col(Column::Status)
                    .col(Column::NextRetryAt)
                    .to_owned(),
            )
            .await?;

        // Dedup/audit lookups by the stable event id.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_supersip_webhook_outbox_event_id")
                    .table(Entity)
                    .col(Column::EventId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only per Phase 6 D-05: rollbacks must not drop undelivered rows.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Database, EntityTrait, Set};
    use sea_orm_migration::MigratorTrait;

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
            .expect("run webhook_outbox migration");
        db
    }

    fn row(id: &str, event_id: &str, status: &str) -> ActiveModel {
        let now = chrono::Utc::now();
        ActiveModel {
            id: Set(id.to_string()),
            webhook_id: Set("wh-1".to_string()),
            event_id: Set(event_id.to_string()),
            event_name: Set("call.completed".to_string()),
            envelope: Set(r#"{"event":"call.completed"}"#.to_string()),
            status: Set(status.to_string()),
            attempt_count: Set(0),
            last_error: Set(None),
            next_retry_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
        }
    }

    #[tokio::test]
    async fn migration_creates_table_and_round_trips() {
        let db = fresh_sqlite().await;
        let inserted = row("ob-1", "evt_abc", STATUS_PENDING)
            .insert(&db)
            .await
            .expect("insert outbox row");
        assert_eq!(inserted.status, STATUS_PENDING);
        assert_eq!(inserted.attempt_count, 0);

        let found = Entity::find_by_id("ob-1".to_string())
            .one(&db)
            .await
            .expect("query")
            .expect("row present");
        assert_eq!(found.event_id, "evt_abc");
        assert_eq!(found.event_name, "call.completed");
    }

    #[tokio::test]
    async fn multiple_rows_share_one_event_id() {
        // One event fans out to N webhooks → N rows, same event_id, distinct ids.
        let db = fresh_sqlite().await;
        row("ob-a", "evt_shared", STATUS_PENDING)
            .insert(&db)
            .await
            .expect("first");
        row("ob-b", "evt_shared", STATUS_PENDING)
            .insert(&db)
            .await
            .expect("second (no PK/unique conflict on event_id)");
        let count = Entity::find().all(&db).await.expect("all").len();
        assert_eq!(count, 2);
    }
}
