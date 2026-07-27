//! `supersip_routing_tables` — Phase 6 NEW routing tables with embedded
//! records (mongo-style JSON column per D-01).
//!
//! Phase 6 Plan 06-01 — RTE-01/RTE-02 storage. One row per routing table;
//! `records` is a `JSON` array of record objects whose shape is locked by
//! D-03 (record_id UUIDv4, position, match{type,…}, target{kind,…},
//! is_default, is_active). The shape is enforced by handlers in 06-02 /
//! 06-03 — this entity only enforces the table-level columns.
//!
//! `direction` is text (`"inbound" | "outbound" | "both"`) per D-21;
//! `priority` is i32 ASC (lower first) per D-22.
//!
//! The `supersip_` prefix follows D-00. Legacy `rustpbx_routes` is left
//! UNTOUCHED in Phase 6 (D-05).

use sea_orm::entity::prelude::*;
use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, QueryFilter};
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_routing_tables")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    #[sea_orm(unique)]
    pub name: String,
    pub description: Option<String>,
    /// "inbound" | "outbound" | "both" (D-21). Stored as text — handler
    /// validates the enum at PUT/POST.
    pub direction: String,
    /// Lower = first (D-22). Default 100.
    pub priority: i32,
    pub is_active: bool,
    /// Embedded array of record objects (D-01, D-03). Empty array `[]` by
    /// default; never NULL. Shape validated in handlers (06-02 / 06-03).
    pub records: Json,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    /// Owning org (task 3.1 tenant isolation). DB default `'default'` until 3.1b
    /// threads the real org_id from request context.
    pub org_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// List routing tables owned by `org_id` (task 3.1 tenant isolation).
    pub async fn list_by_org(
        db: &DatabaseConnection,
        org_id: &str,
    ) -> Result<Vec<Self>, DbErr> {
        Entity::find().filter(Column::OrgId.eq(org_id)).all(db).await
    }

    /// Get a routing table by name, scoped to `org_id` — `None` when the table
    /// belongs to a different org (cross-tenant lookups never resolve).
    pub async fn get_by_org(
        db: &DatabaseConnection,
        org_id: &str,
        name: &str,
    ) -> Result<Option<Self>, DbErr> {
        Entity::find()
            .filter(Column::Name.eq(name))
            .filter(Column::OrgId.eq(org_id))
            .one(db)
            .await
    }
}

#[cfg(test)]
mod org_id_tests {
    use super::*;
    use crate::models::migration::Migrator;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, Database};
    use sea_orm_migration::MigratorTrait;

    async fn fresh() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.expect("sqlite");
        Migrator::up(&db, None).await.expect("migrate");
        db
    }

    async fn insert_table(db: &DatabaseConnection, name: &str, org: Option<&str>) {
        let mut am = ActiveModel {
            name: Set(name.to_string()),
            ..Default::default()
        };
        // Leave org_id NotSet when None so the column default ('default') applies.
        if let Some(org) = org {
            am.org_id = Set(org.to_string());
        }
        am.insert(db).await.expect("insert routing table");
    }

    #[tokio::test]
    async fn list_by_org_isolates_tenants() {
        let db = fresh().await;
        insert_table(&db, "table_a", Some("org_a")).await;
        insert_table(&db, "table_b", Some("org_b")).await;
        let a = Model::list_by_org(&db, "org_a").await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "table_a");
        assert_eq!(a[0].org_id, "org_a");
    }

    #[tokio::test]
    async fn get_by_org_rejects_foreign_tenant() {
        let db = fresh().await;
        insert_table(&db, "table_a", Some("org_a")).await;
        // The table exists but belongs to org_a → a cross-tenant get is None.
        assert!(
            Model::get_by_org(&db, "org_b", "table_a")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            Model::get_by_org(&db, "org_a", "table_a")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn insert_defaults_org_id() {
        // org_id NotSet → the column default ('default') applies, so existing
        // write paths keep working until 3.1b threads a real org_id.
        let db = fresh().await;
        insert_table(&db, "table_c", None).await;
        let row = Model::get_by_org(&db, "default", "table_c")
            .await
            .unwrap()
            .expect("row under default org");
        assert_eq!(row.org_id, "default");
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
                            .big_integer()
                            .primary_key()
                            .auto_increment(),
                    )
                    .col(ColumnDef::new(Column::Name).string().not_null())
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
                        ColumnDef::new(Column::Records)
                            .json()
                            .not_null()
                            .default(Expr::cust("'[]'")),
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
                    .name("idx_supersip_routing_tables_name")
                    .table(Entity)
                    .col(Column::Name)
                    .unique()
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
