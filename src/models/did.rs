use chrono::Utc;
use phonenumber::{Mode, country, parse};
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ActiveValue::Set, ColumnTrait, DatabaseConnection, PaginatorTrait, QueryFilter};
use sea_orm_migration::prelude::{ColumnDef as MigrationColumnDef, *};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
pub enum DidError {
    #[error("DID input was empty")]
    Empty,
    #[error("DID is in local format and no default region is configured")]
    MissingRegion,
    #[error("invalid phone number: {0}")]
    InvalidNumber(String),
    #[error("unknown country code: {0}")]
    UnknownCountry(String),
}

/// Normalize a DID into canonical E.164 form (`+<cc><national>`).
///
/// `default_region` is an ISO 3166-1 alpha-2 country code (e.g. "US", "IN").
/// When `None`, the input must start with `+`.
pub fn normalize_did(raw: &str, default_region: Option<&str>) -> Result<String, DidError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DidError::Empty);
    }

    let region = match default_region {
        Some(code) => Some(
            country::Id::from_str(code)
                .map_err(|_| DidError::UnknownCountry(code.to_string()))?,
        ),
        None => None,
    };

    if region.is_none() && !trimmed.starts_with('+') {
        return Err(DidError::MissingRegion);
    }

    let parsed =
        parse(region, trimmed).map_err(|e| DidError::InvalidNumber(e.to_string()))?;

    if !parsed.is_valid() {
        return Err(DidError::InvalidNumber(format!(
            "not a valid number: {trimmed}"
        )));
    }

    Ok(parsed.format().mode(Mode::E164).to_string())
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "rustpbx_dids")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub number: String,
    pub trunk_name: Option<String>,
    pub extension_number: Option<String>,
    pub failover_trunk: Option<String>,
    pub label: Option<String>,
    pub enabled: bool,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub trunk_group_name: Option<String>,
    /// Owning org (task 3.1 tenant isolation). DB default `'default'` until 3.1b
    /// threads the real org_id from request context; `upsert` leaves it NotSet
    /// so the column default applies and an upsert never rewrites it.
    pub org_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Input for creating or updating a DID. `number` must already be normalized E.164.
#[derive(Debug, Clone)]
pub struct NewDid {
    pub number: String,
    pub trunk_name: Option<String>,
    pub extension_number: Option<String>,
    pub failover_trunk: Option<String>,
    pub label: Option<String>,
    pub enabled: bool,
}

impl Model {
    /// Insert or update a DID by primary-key number.
    pub async fn upsert(db: &DatabaseConnection, new: NewDid) -> Result<(), DbErr> {
        let now = Utc::now();
        let active = ActiveModel {
            number: Set(new.number),
            trunk_name: Set(new.trunk_name),
            extension_number: Set(new.extension_number),
            failover_trunk: Set(new.failover_trunk),
            label: Set(new.label),
            enabled: Set(new.enabled),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()   // trunk_group_name stays NotSet → NULL
        };
        Entity::insert(active)
            .on_conflict(
                OnConflict::column(Column::Number)
                    .update_columns([
                        Column::TrunkName,
                        Column::ExtensionNumber,
                        Column::FailoverTrunk,
                        Column::Label,
                        Column::Enabled,
                        Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(db)
            .await?;
        Ok(())
    }

    pub async fn get(db: &DatabaseConnection, number: &str) -> Result<Option<Self>, DbErr> {
        Entity::find_by_id(number.to_owned()).one(db).await
    }

    pub async fn list_all(db: &DatabaseConnection) -> Result<Vec<Self>, DbErr> {
        Entity::find().all(db).await
    }

    /// List DIDs owned by `org_id` (task 3.1 tenant isolation).
    pub async fn list_by_org(
        db: &DatabaseConnection,
        org_id: &str,
    ) -> Result<Vec<Self>, DbErr> {
        Entity::find().filter(Column::OrgId.eq(org_id)).all(db).await
    }

    /// Get a DID by number, scoped to `org_id` — `None` when the number belongs
    /// to a different org (cross-tenant lookups never resolve).
    pub async fn get_by_org(
        db: &DatabaseConnection,
        org_id: &str,
        number: &str,
    ) -> Result<Option<Self>, DbErr> {
        Entity::find_by_id(number.to_owned())
            .filter(Column::OrgId.eq(org_id))
            .one(db)
            .await
    }

    pub async fn list_by_trunk(
        db: &DatabaseConnection,
        trunk_name: &str,
    ) -> Result<Vec<Self>, DbErr> {
        Entity::find()
            .filter(Column::TrunkName.eq(trunk_name))
            .all(db)
            .await
    }

    pub async fn count_by_trunk(
        db: &DatabaseConnection,
        trunk_name: &str,
    ) -> Result<u64, DbErr> {
        Entity::find()
            .filter(Column::TrunkName.eq(trunk_name))
            .count(db)
            .await
    }

    /// Count DIDs with no owning trunk (parked / unassigned).
    pub async fn count_unassigned(db: &DatabaseConnection) -> Result<u64, DbErr> {
        Entity::find()
            .filter(Column::TrunkName.is_null())
            .count(db)
            .await
    }

    /// List DIDs with no owning trunk (parked / unassigned).
    pub async fn list_unassigned(db: &DatabaseConnection) -> Result<Vec<Self>, DbErr> {
        Entity::find()
            .filter(Column::TrunkName.is_null())
            .all(db)
            .await
    }

    pub async fn count_by_failover_trunk(
        db: &DatabaseConnection,
        trunk_name: &str,
    ) -> Result<u64, DbErr> {
        Entity::find()
            .filter(Column::FailoverTrunk.eq(trunk_name))
            .count(db)
            .await
    }

    /// Clear `extension_number` on all rows currently referencing `extension_number`.
    pub async fn null_extension(
        db: &DatabaseConnection,
        extension_number: &str,
    ) -> Result<u64, DbErr> {
        let res = Entity::update_many()
            .col_expr(
                Column::ExtensionNumber,
                Expr::value(Option::<String>::None),
            )
            .col_expr(Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(Column::ExtensionNumber.eq(extension_number))
            .exec(db)
            .await?;
        Ok(res.rows_affected)
    }

    pub async fn delete(db: &DatabaseConnection, number: &str) -> Result<(), DbErr> {
        Entity::delete_by_id(number.to_owned()).exec(db).await?;
        Ok(())
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
                        MigrationColumnDef::new(Column::Number)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(MigrationColumnDef::new(Column::TrunkName).text().null())
                    .col(MigrationColumnDef::new(Column::ExtensionNumber).text().null())
                    .col(MigrationColumnDef::new(Column::FailoverTrunk).text().null())
                    .col(MigrationColumnDef::new(Column::Label).text().null())
                    .col(
                        MigrationColumnDef::new(Column::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        MigrationColumnDef::new(Column::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_dids_trunk_name")
                    .table(Entity)
                    .col(Column::TrunkName)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_dids_extension_number")
                    .table(Entity)
                    .col(Column::ExtensionNumber)
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

#[cfg(test)]
mod org_id_tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Database};
    use sea_orm_migration::MigratorTrait;

    /// Runs the base table + the org_id ADD migration (task 3.1).
    struct TestMigrator;
    #[async_trait::async_trait]
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![
                Box::new(Migration),
                Box::new(crate::models::add_org_id_did::Migration),
            ]
        }
    }

    async fn fresh() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.expect("sqlite");
        TestMigrator::up(&db, None).await.expect("migrate");
        db
    }

    async fn insert_did(db: &DatabaseConnection, number: &str, org: &str) {
        let now = Utc::now();
        ActiveModel {
            number: Set(number.to_string()),
            org_id: Set(org.to_string()),
            enabled: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert did");
    }

    #[tokio::test]
    async fn list_by_org_isolates_tenants() {
        let db = fresh().await;
        insert_did(&db, "+15551110000", "org_a").await;
        insert_did(&db, "+15552220000", "org_b").await;
        let a = Model::list_by_org(&db, "org_a").await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].number, "+15551110000");
        assert_eq!(a[0].org_id, "org_a");
    }

    #[tokio::test]
    async fn get_by_org_rejects_foreign_tenant() {
        let db = fresh().await;
        insert_did(&db, "+15551110000", "org_a").await;
        // The number exists but belongs to org_a → a cross-tenant get is None.
        assert!(
            Model::get_by_org(&db, "org_b", "+15551110000")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            Model::get_by_org(&db, "org_a", "+15551110000")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn upsert_defaults_org_id() {
        // upsert leaves org_id NotSet → the column default ('default') applies,
        // so existing write paths keep working until 3.1b threads a real org_id.
        let db = fresh().await;
        Model::upsert(
            &db,
            NewDid {
                number: "+15553330000".to_string(),
                trunk_name: None,
                extension_number: None,
                failover_trunk: None,
                label: None,
                enabled: true,
            },
        )
        .await
        .unwrap();
        let row = Model::get(&db, "+15553330000").await.unwrap().unwrap();
        assert_eq!(row.org_id, "default");
    }
}
