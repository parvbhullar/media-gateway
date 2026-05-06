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
    /// Forward reference for Phase 3+ DID → trunk_group routing.
    /// Phase 2 Plan 02-01 introduces the column so the TRK-04 engagement
    /// check has a real target to scan. Phase 1 DID handlers do not
    /// touch this column (the `NewDid` struct does not carry it and the
    /// upsert `update_columns(...)` list deliberately excludes it).
    pub trunk_group_name: Option<String>,
    /// Phase 13 Plan 01a (TEN-01) — owning sub-account; defaults to 'root'.
    #[sea_orm(default_value = "root")]
    pub account_id: String,
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
            // Phase 2 Plan 02-01: new column forward-referenced by the
            // trunk_group routing work. Phase 1 upsert path deliberately
            // does not touch it (NotSet so existing rows keep their value
            // and new rows default to NULL).
            ..Default::default()
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
