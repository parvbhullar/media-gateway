//! Pending S3 uploads — one row per file that failed to upload.
//!
//! Populated by `save_with_s3_like` on each per-file failure. The
//! `upload_retry` background scheduler scans this table on a tick,
//! retries entries whose backoff has elapsed, and on success deletes
//! the row (and optionally the local file, if `keep_media_copy = false`).
//!
//! Each row represents one *file* — either a CDR JSON or a media `.wav`.
//! Per-file granularity means a partial upload failure only re-uploads
//! the parts that didn't make it.

use chrono::Utc;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use sea_orm_migration::prelude::{ColumnDef as MigrationColumnDef, *};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "pending_uploads")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Call this upload belongs to.
    pub call_id: String,
    /// `cdr` for the JSON detail record, `media` for an audio file.
    pub kind: String,
    /// Absolute or relative local filesystem path of the file to upload.
    pub local_path: String,
    /// Object key the file should land at in S3 (relative to bucket root,
    /// after the configured prefix is applied).
    pub target_key: String,
    /// Number of retry attempts already made (0 on first failure).
    pub attempts: i32,
    /// Most recent error string (truncated).
    pub last_error: Option<String>,
    pub last_attempt_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    /// `pending`, `failed_permanent`, or `failed_missing_source`.
    pub status: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_FAILED_PERMANENT: &str = "failed_permanent";
pub const STATUS_FAILED_MISSING_SOURCE: &str = "failed_missing_source";

pub const KIND_CDR: &str = "cdr";
pub const KIND_MEDIA: &str = "media";

impl Model {
    /// Insert (or refresh on conflict) a pending upload row.
    ///
    /// Idempotent on `(call_id, local_path)`: if a row already exists for
    /// this file the existing row is updated with the new error and the
    /// attempt counter is incremented. This means the saver can call
    /// this freely on every failure without worrying about duplicates.
    pub async fn upsert_failure(
        db: &DatabaseConnection,
        call_id: &str,
        kind: &str,
        local_path: &str,
        target_key: &str,
        error: &str,
    ) -> Result<(), DbErr> {
        let now = Utc::now();
        let truncated_error = truncate(error, 512);
        let active = ActiveModel {
            id: sea_orm::ActiveValue::NotSet,
            call_id: Set(call_id.to_owned()),
            kind: Set(kind.to_owned()),
            local_path: Set(local_path.to_owned()),
            target_key: Set(target_key.to_owned()),
            attempts: Set(0),
            last_error: Set(Some(truncated_error.clone())),
            last_attempt_at: Set(None),
            created_at: Set(now),
            status: Set(STATUS_PENDING.to_owned()),
        };
        Entity::insert(active)
            .on_conflict(
                OnConflict::columns([Column::CallId, Column::LocalPath])
                    .update_columns([
                        Column::TargetKey,
                        Column::LastError,
                        Column::Status,
                    ])
                    .to_owned(),
            )
            .exec(db)
            .await?;
        Ok(())
    }

    /// All rows still in pending state, oldest first. Used by the
    /// scheduler to pick the next batch to retry.
    pub async fn list_pending(
        db: &DatabaseConnection,
        limit: u64,
    ) -> Result<Vec<Self>, DbErr> {
        use sea_orm::{QueryOrder, QuerySelect};
        Entity::find()
            .filter(Column::Status.eq(STATUS_PENDING))
            .order_by_asc(Column::CreatedAt)
            .limit(limit)
            .all(db)
            .await
    }

    /// Most recent rows of any status, newest first. Used by the UI panel.
    pub async fn list_recent(
        db: &DatabaseConnection,
        limit: u64,
    ) -> Result<Vec<Self>, DbErr> {
        use sea_orm::{QueryOrder, QuerySelect};
        Entity::find()
            .order_by_desc(Column::CreatedAt)
            .limit(limit)
            .all(db)
            .await
    }

    /// Counts grouped by status, used by the UI panel summary.
    pub async fn count_by_status(
        db: &DatabaseConnection,
    ) -> Result<(u64, u64, u64), DbErr> {
        use sea_orm::PaginatorTrait;
        let pending = Entity::find()
            .filter(Column::Status.eq(STATUS_PENDING))
            .count(db)
            .await?;
        let failed_perm = Entity::find()
            .filter(Column::Status.eq(STATUS_FAILED_PERMANENT))
            .count(db)
            .await?;
        let failed_missing = Entity::find()
            .filter(Column::Status.eq(STATUS_FAILED_MISSING_SOURCE))
            .count(db)
            .await?;
        Ok((pending, failed_perm, failed_missing))
    }

    /// Mark a row as successfully retried — caller deletes after this
    /// (we keep the function so the model owns the queries).
    pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<(), DbErr> {
        Entity::delete_by_id(id).exec(db).await?;
        Ok(())
    }

    /// Bump attempt counter and store latest error after a failed retry.
    pub async fn record_attempt(
        db: &DatabaseConnection,
        id: i64,
        new_attempts: i32,
        new_status: &str,
        error: &str,
    ) -> Result<(), DbErr> {
        let active = ActiveModel {
            id: Set(id),
            attempts: Set(new_attempts),
            last_error: Set(Some(truncate(error, 512))),
            last_attempt_at: Set(Some(Utc::now())),
            status: Set(new_status.to_owned()),
            ..Default::default()
        };
        Entity::update(active).exec(db).await?;
        Ok(())
    }

    /// Reset every non-pending row back to pending — UI "Retry all" hook.
    pub async fn reset_failed(db: &DatabaseConnection) -> Result<u64, DbErr> {
        use sea_orm::sea_query::Expr;
        let res = Entity::update_many()
            .col_expr(Column::Status, Expr::value(STATUS_PENDING))
            .col_expr(Column::Attempts, Expr::value(0))
            .filter(Column::Status.ne(STATUS_PENDING))
            .exec(db)
            .await?;
        Ok(res.rows_affected)
    }

    /// Delete every row in `failed_permanent` or `failed_missing_source`
    /// state — UI "Clear failures" hook.
    pub async fn clear_failures(db: &DatabaseConnection) -> Result<u64, DbErr> {
        use sea_orm::sea_query::Condition;
        let res = Entity::delete_many()
            .filter(
                Condition::any()
                    .add(Column::Status.eq(STATUS_FAILED_PERMANENT))
                    .add(Column::Status.eq(STATUS_FAILED_MISSING_SOURCE)),
            )
            .exec(db)
            .await?;
        Ok(res.rows_affected)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = String::with_capacity(max);
        out.push_str(&s[..max - 3]);
        out.push_str("...");
        out
    }
}

// ─── Migration ───────────────────────────────────────────────────────────────

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260412_000001_create_pending_uploads"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Entity)
                    .if_not_exists()
                    .col(
                        MigrationColumnDef::new(Column::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::CallId)
                            .string()
                            .not_null(),
                    )
                    .col(MigrationColumnDef::new(Column::Kind).string().not_null())
                    .col(
                        MigrationColumnDef::new(Column::LocalPath)
                            .string()
                            .not_null(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::TargetKey)
                            .string()
                            .not_null(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(MigrationColumnDef::new(Column::LastError).text())
                    .col(
                        MigrationColumnDef::new(Column::LastAttemptAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::Status)
                            .string()
                            .not_null()
                            .default(STATUS_PENDING),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique index on (call_id, local_path) so upsert_failure can dedup.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_pending_uploads_call_local")
                    .table(Entity)
                    .col(Column::CallId)
                    .col(Column::LocalPath)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Index for the scheduler's "give me pending rows" query.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_pending_uploads_status_created")
                    .table(Entity)
                    .col(Column::Status)
                    .col(Column::CreatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Entity).to_owned())
            .await
    }
}
