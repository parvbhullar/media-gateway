use chrono::Utc;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use sea_orm_migration::prelude::{ColumnDef as MigrationColumnDef, *};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "system_config")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    /// JSON-encoded value (string, number, bool, array, or object)
    pub value: String,
    /// When true, auto-detection (e.g. external_ip) is skipped for this key
    pub is_override: bool,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Fetch all rows.
    pub async fn get_all(db: &DatabaseConnection) -> Result<Vec<Self>, DbErr> {
        Entity::find().all(db).await
    }

    /// Insert or update a single key.
    pub async fn upsert(
        db: &DatabaseConnection,
        key: &str,
        value: &str,
        is_override: bool,
    ) -> Result<(), DbErr> {
        let active = ActiveModel {
            key: Set(key.to_owned()),
            value: Set(value.to_owned()),
            is_override: Set(is_override),
            updated_at: Set(Utc::now()),
        };
        Entity::insert(active)
            .on_conflict(
                OnConflict::column(Column::Key)
                    .update_columns([Column::Value, Column::IsOverride, Column::UpdatedAt])
                    .to_owned(),
            )
            .exec(db)
            .await?;
        Ok(())
    }

    /// Fetch a single key.
    pub async fn get(db: &DatabaseConnection, key: &str) -> Result<Option<Self>, DbErr> {
        Entity::find_by_id(key.to_owned()).one(db).await
    }

    /// True when the table has no rows (first-run detection).
    pub async fn is_empty(db: &DatabaseConnection) -> Result<bool, DbErr> {
        use sea_orm::PaginatorTrait;
        Ok(Entity::find().count(db).await? == 0)
    }
}

// ─── Migration ───────────────────────────────────────────────────────────────

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260411_000001_create_system_config"
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
                        MigrationColumnDef::new(Column::Key)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::Value)
                            .text()
                            .not_null(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::IsOverride)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        MigrationColumnDef::new(Column::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
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
