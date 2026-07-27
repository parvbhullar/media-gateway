//! `organizations` — org-level multi-tenancy. Each row is created/updated by
//! an external system that owns org lifecycle; media-gateway never mints an
//! `org_id` itself. `org_id` is the primary key (no internal auto-increment
//! id) because the external system identifies orgs by this value and never
//! sees an internal id.
//!
//! `org_id = "default"` (see `UNASSIGNED_ORG_ID`) is the sentinel used by the
//! 5 pre-existing `org_id` columns (`did`, `extension`, `trunk`, `routing`,
//! `routing_tables`) for "no org assigned" — those columns are `NOT NULL
//! DEFAULT 'default'` and are never migrated to true SQL NULL (SQLite can't
//! relax that constraint without a table rebuild). No row in this table is
//! ever created with `org_id = "default"`, so resources carrying the
//! sentinel simply never match a row here and are treated as unassigned.

use chrono::Utc;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::ActiveValue::Set;
use sea_orm_migration::prelude::{ColumnDef as MigrationColumnDef, *};
use sea_orm_migration::schema::timestamp;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

/// Sentinel value used by the legacy `org_id` columns on `did`/`extension`/
/// `trunk`/`routing`/`routing_tables` to mean "no org assigned". No real
/// `organizations` row is ever created with this key.
pub const UNASSIGNED_ORG_ID: &str = "default";

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "rustpbx_organizations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub org_id: String,
    pub name: String,
    pub enabled: bool,
    pub max_cps: Option<i32>,
    pub max_calls: Option<i32>,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    pub notes: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub struct NewOrganization {
    pub org_id: String,
    pub name: String,
    pub enabled: bool,
    pub max_cps: Option<i32>,
    pub max_calls: Option<i32>,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    pub notes: Option<String>,
}

impl Model {
    pub async fn get(db: &DatabaseConnection, org_id: &str) -> Result<Option<Self>, DbErr> {
        Entity::find_by_id(org_id.to_owned()).one(db).await
    }

    pub async fn list_all(db: &DatabaseConnection) -> Result<Vec<Self>, DbErr> {
        Entity::find().all(db).await
    }

    /// Insert or update an organization by `org_id`.
    pub async fn upsert(db: &DatabaseConnection, new: NewOrganization) -> Result<(), DbErr> {
        let now = Utc::now();
        let active = ActiveModel {
            org_id: Set(new.org_id),
            name: Set(new.name),
            enabled: Set(new.enabled),
            max_cps: Set(new.max_cps),
            max_calls: Set(new.max_calls),
            contact_name: Set(new.contact_name),
            contact_email: Set(new.contact_email),
            notes: Set(new.notes),
            created_at: Set(now),
            updated_at: Set(now),
        };
        Entity::insert(active)
            .on_conflict(
                OnConflict::column(Column::OrgId)
                    .update_columns([
                        Column::Name,
                        Column::Enabled,
                        Column::MaxCps,
                        Column::MaxCalls,
                        Column::ContactName,
                        Column::ContactEmail,
                        Column::Notes,
                        Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(db)
            .await?;
        Ok(())
    }

    /// Returns `false` if `org_id` has no row (caller should 404).
    pub async fn set_enabled(
        db: &DatabaseConnection,
        org_id: &str,
        enabled: bool,
    ) -> Result<bool, DbErr> {
        let Some(existing) = Self::get(db, org_id).await? else {
            return Ok(false);
        };
        let mut active: ActiveModel = existing.into();
        active.enabled = Set(enabled);
        active.updated_at = Set(Utc::now());
        active.update(db).await?;
        Ok(true)
    }
}

// ─── Migration ──────────────────────────────────────────────────────────────

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
                        MigrationColumnDef::new(Column::OrgId)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(MigrationColumnDef::new(Column::Name).string().not_null())
                    .col(
                        MigrationColumnDef::new(Column::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(MigrationColumnDef::new(Column::MaxCps).integer().null())
                    .col(MigrationColumnDef::new(Column::MaxCalls).integer().null())
                    .col(MigrationColumnDef::new(Column::ContactName).string().null())
                    .col(MigrationColumnDef::new(Column::ContactEmail).string().null())
                    .col(MigrationColumnDef::new(Column::Notes).text().null())
                    .col(timestamp(Column::CreatedAt).default(Expr::current_timestamp()))
                    .col(timestamp(Column::UpdatedAt).default(Expr::current_timestamp()))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // No-op down, matching the other org_id-era migrations (additive,
        // never reversed).
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Database;
    use sea_orm_migration::MigratorTrait;

    struct TestMigrator;
    #[async_trait::async_trait]
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![Box::new(Migration)]
        }
    }

    async fn setup_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        TestMigrator::up(&db, None).await.unwrap();
        db
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips() {
        let db = setup_db().await;
        Model::upsert(
            &db,
            NewOrganization {
                org_id: "acme".into(),
                name: "Acme Corp".into(),
                enabled: true,
                max_cps: Some(5),
                max_calls: Some(20),
                contact_name: None,
                contact_email: None,
                notes: None,
            },
        )
        .await
        .unwrap();

        let row = Model::get(&db, "acme").await.unwrap().unwrap();
        assert_eq!(row.name, "Acme Corp");
        assert!(row.enabled);
        assert_eq!(row.max_cps, Some(5));
        assert_eq!(row.max_calls, Some(20));
    }

    #[tokio::test]
    async fn upsert_twice_updates_in_place() {
        let db = setup_db().await;
        let make = |name: &str| NewOrganization {
            org_id: "acme".into(),
            name: name.into(),
            enabled: true,
            max_cps: None,
            max_calls: None,
            contact_name: None,
            contact_email: None,
            notes: None,
        };
        Model::upsert(&db, make("Acme v1")).await.unwrap();
        Model::upsert(&db, make("Acme v2")).await.unwrap();

        let all = Model::list_all(&db).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Acme v2");
    }

    #[tokio::test]
    async fn set_enabled_toggles_and_reports_missing() {
        let db = setup_db().await;
        Model::upsert(
            &db,
            NewOrganization {
                org_id: "acme".into(),
                name: "Acme".into(),
                enabled: true,
                max_cps: None,
                max_calls: None,
                contact_name: None,
                contact_email: None,
                notes: None,
            },
        )
        .await
        .unwrap();

        assert!(Model::set_enabled(&db, "acme", false).await.unwrap());
        let row = Model::get(&db, "acme").await.unwrap().unwrap();
        assert!(!row.enabled);

        assert!(!Model::set_enabled(&db, "no-such-org", false).await.unwrap());
    }
}
