use sea_orm::entity::prelude::*;
use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, QueryFilter};
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{
    boolean, integer, json_null, string, string_null, text_null, timestamp, timestamp_null,
};
use sea_orm_migration::sea_query::{ColumnDef, ForeignKeyAction as MigrationForeignKeyAction};
use sea_query::Expr;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[derive(Default)]
pub enum RoutingDirection {
    #[sea_orm(string_value = "inbound")]
    Inbound,
    #[sea_orm(string_value = "outbound")]
    #[default]
    Outbound,
}

impl RoutingDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[derive(Default)]
pub enum RoutingSelectionStrategy {
    #[sea_orm(string_value = "rr")]
    #[serde(alias = "rr", alias = "round_robin", alias = "round-robin")]
    #[default]
    RoundRobin,
    #[sea_orm(string_value = "weight")]
    #[serde(alias = "weight")]
    Weighted,
    #[sea_orm(string_value = "hash")]
    Hash,
}

impl RoutingSelectionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoundRobin => "rr",
            Self::Weighted => "weight",
            Self::Hash => "hash",
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "rustpbx_routes")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    #[sea_orm(unique)]
    pub name: String,
    pub description: Option<String>,
    pub direction: RoutingDirection,
    pub priority: i32,
    pub is_active: bool,
    pub selection_strategy: RoutingSelectionStrategy,
    pub hash_key: Option<String>,
    pub source_trunk_id: Option<i64>,
    pub default_trunk_id: Option<i64>,
    pub source_pattern: Option<String>,
    pub destination_pattern: Option<String>,
    pub header_filters: Option<Json>,
    pub rewrite_rules: Option<Json>,
    pub target_trunks: Option<Json>,
    pub notes: Option<Json>,
    pub metadata: Option<Json>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub last_deployed_at: Option<DateTimeUtc>,
    /// Owning org (task 3.1 tenant key). DB default `'default'` until 3.1b
    /// threads the real org_id from request context. Replaced the legacy
    /// free-text `owner` column (backfilled + dropped via
    /// `backfill_org_id_routing`).
    pub org_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::sip_trunk::Entity",
        from = "Column::SourceTrunkId",
        to = "super::sip_trunk::Column::Id",
        on_delete = "SetNull",
        on_update = "Cascade"
    )]
    SourceTrunk,
    #[sea_orm(
        belongs_to = "super::sip_trunk::Entity",
        from = "Column::DefaultTrunkId",
        to = "super::sip_trunk::Column::Id",
        on_delete = "SetNull",
        on_update = "Cascade"
    )]
    DefaultTrunk,
}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// List routes owned by `org_id` (task 3.1 tenant isolation).
    pub async fn list_by_org(
        db: &DatabaseConnection,
        org_id: &str,
    ) -> Result<Vec<Self>, DbErr> {
        Entity::find().filter(Column::OrgId.eq(org_id)).all(db).await
    }

    /// Get a route by name, scoped to `org_id` — `None` when the route belongs
    /// to a different org (cross-tenant lookups never resolve).
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

    async fn insert_route(db: &DatabaseConnection, name: &str, org: Option<&str>) {
        let mut am = ActiveModel {
            name: Set(name.to_string()),
            ..Default::default()
        };
        // Leave org_id NotSet when None so the column default ('default') applies.
        if let Some(org) = org {
            am.org_id = Set(org.to_string());
        }
        am.insert(db).await.expect("insert route");
    }

    #[tokio::test]
    async fn list_by_org_isolates_tenants() {
        let db = fresh().await;
        insert_route(&db, "route_a", Some("org_a")).await;
        insert_route(&db, "route_b", Some("org_b")).await;
        let a = Model::list_by_org(&db, "org_a").await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "route_a");
        assert_eq!(a[0].org_id, "org_a");
    }

    #[tokio::test]
    async fn get_by_org_rejects_foreign_tenant() {
        let db = fresh().await;
        insert_route(&db, "route_a", Some("org_a")).await;
        // The route exists but belongs to org_a → a cross-tenant get is None.
        assert!(
            Model::get_by_org(&db, "org_b", "route_a")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            Model::get_by_org(&db, "org_a", "route_a")
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
        insert_route(&db, "route_c", None).await;
        let row = Model::get_by_org(&db, "default", "route_c")
            .await
            .unwrap()
            .expect("row under default org");
        assert_eq!(row.org_id, "default");
    }
}

#[cfg(test)]
mod tests {
    use super::RoutingSelectionStrategy;

    #[test]
    fn selection_strategy_accepts_aliases() {
        let rr: RoutingSelectionStrategy = serde_json::from_str("\"rr\"").unwrap();
        assert!(matches!(rr, RoutingSelectionStrategy::RoundRobin));

        let rr_alt: RoutingSelectionStrategy = serde_json::from_str("\"round_robin\"").unwrap();
        assert!(matches!(rr_alt, RoutingSelectionStrategy::RoundRobin));

        let weight: RoutingSelectionStrategy = serde_json::from_str("\"weight\"").unwrap();
        assert!(matches!(weight, RoutingSelectionStrategy::Weighted));
    }

    #[test]
    fn selection_strategy_serializes_with_canonical_names() {
        let serialized = serde_json::to_string(&RoutingSelectionStrategy::RoundRobin).unwrap();
        assert_eq!(serialized, "\"roundrobin\"");

        let serialized_weight = serde_json::to_string(&RoutingSelectionStrategy::Weighted).unwrap();
        assert_eq!(serialized_weight, "\"weighted\"");
    }
}

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
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(string(Column::Name).char_len(160))
                    .col(text_null(Column::Description))
                    .col(
                        string(Column::Direction)
                            .char_len(32)
                            .default(RoutingDirection::default().as_str()),
                    )
                    .col(integer(Column::Priority).not_null().default(100))
                    .col(boolean(Column::IsActive).default(true))
                    .col(
                        string(Column::SelectionStrategy)
                            .char_len(32)
                            .default(RoutingSelectionStrategy::default().as_str()),
                    )
                    .col(string_null(Column::HashKey).char_len(120))
                    .col(ColumnDef::new(Column::SourceTrunkId).big_integer().null())
                    .col(ColumnDef::new(Column::DefaultTrunkId).big_integer().null())
                    .col(string_null(Column::SourcePattern).char_len(160))
                    .col(string_null(Column::DestinationPattern).char_len(160))
                    .col(json_null(Column::HeaderFilters))
                    .col(json_null(Column::RewriteRules))
                    .col(json_null(Column::TargetTrunks))
                    // Legacy free-text owner column. The Model no longer maps it
                    // (so `Column::Owner` is gone) — reference it by Alias so the
                    // historical schema is still created here, then superseded:
                    // `backfill_org_id_routing` copies owner→org_id and drops it.
                    .col(string_null(sea_query::Alias::new("owner")).char_len(120))
                    .col(json_null(Column::Notes))
                    .col(json_null(Column::Metadata))
                    .col(timestamp(Column::CreatedAt).default(Expr::current_timestamp()))
                    .col(timestamp(Column::UpdatedAt).default(Expr::current_timestamp()))
                    .col(timestamp_null(Column::LastDeployedAt))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_routes_source_trunk")
                            .from(Entity, Column::SourceTrunkId)
                            .to(super::sip_trunk::Entity, super::sip_trunk::Column::Id)
                            .on_delete(MigrationForeignKeyAction::SetNull)
                            .on_update(MigrationForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_routes_default_trunk")
                            .from(Entity, Column::DefaultTrunkId)
                            .to(super::sip_trunk::Entity, super::sip_trunk::Column::Id)
                            .on_delete(MigrationForeignKeyAction::SetNull)
                            .on_update(MigrationForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_rustpbx_routes_name")
                    .table(Entity)
                    .col(Column::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_rustpbx_routes_direction")
                    .table(Entity)
                    .col(Column::Direction)
                    .col(Column::IsActive)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_rustpbx_routes_priority")
                    .table(Entity)
                    .col(Column::Priority)
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
