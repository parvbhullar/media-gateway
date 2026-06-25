use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DatabaseConnection, DbErr, QueryFilter};
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{
    boolean, integer_null, string, string_null, text_null, timestamp, timestamp_null,
};
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::Serialize;

pub const DEFAULT_FORWARDING_TIMEOUT: i32 = 30;
pub const MIN_FORWARDING_TIMEOUT: i32 = 5;
pub const MAX_FORWARDING_TIMEOUT: i32 = 120;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Default)]
#[sea_orm(table_name = "rustpbx_extensions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    #[sea_orm(unique)]
    pub extension: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub status: Option<String>,
    pub login_disabled: bool,
    pub voicemail_disabled: bool,
    pub allow_guest_calls: bool,
    pub sip_password: Option<String>,
    pub call_forwarding_mode: Option<String>,
    pub call_forwarding_destination: Option<String>,
    pub call_forwarding_timeout: Option<i32>,
    #[sea_orm(column_type = "DateTime")]
    pub registered_at: Option<DateTimeUtc>,
    pub notes: Option<String>,
    #[sea_orm(column_type = "DateTime", default_value = "CURRENT_TIMESTAMP")]
    pub created_at: DateTimeUtc,
    #[sea_orm(column_type = "DateTime", default_value = "CURRENT_TIMESTAMP")]
    pub updated_at: DateTimeUtc,
    /// Owning org (task 3.1 tenant isolation). DB default `'default'` until 3.1b
    /// threads the real org_id from request context.
    pub org_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        panic!("no direct relations defined for extension");
    }
}

impl Related<super::department::Entity> for Entity {
    fn to() -> RelationDef {
        super::extension_department::Relation::Department.def()
    }

    fn via() -> Option<RelationDef> {
        Some(super::extension_department::Relation::Extension.def().rev())
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// List extensions owned by `org_id` (task 3.1 tenant isolation).
    pub async fn list_by_org(
        db: &DatabaseConnection,
        org_id: &str,
    ) -> Result<Vec<Self>, DbErr> {
        Entity::find().filter(Column::OrgId.eq(org_id)).all(db).await
    }

    /// Get an extension by its number, scoped to `org_id` — `None` when the
    /// extension belongs to a different org (cross-tenant lookups never resolve).
    pub async fn get_by_org(
        db: &DatabaseConnection,
        org_id: &str,
        extension: &str,
    ) -> Result<Option<Self>, DbErr> {
        Entity::find()
            .filter(Column::Extension.eq(extension))
            .filter(Column::OrgId.eq(org_id))
            .one(db)
            .await
    }
}

impl Entity {
    pub async fn find_by_id_with_departments<C>(
        conn: &C,
        id: i64,
    ) -> Result<Option<(Model, Vec<super::department::Model>)>, DbErr>
    where
        C: ConnectionTrait,
    {
        let mut results = Self::find()
            .filter(Column::Id.eq(id))
            .find_with_related(super::department::Entity)
            .all(conn)
            .await?;

        Ok(results.pop())
    }

    pub async fn replace_departments<C>(
        conn: &C,
        extension_id: i64,
        department_ids: &[i64],
    ) -> Result<(), DbErr>
    where
        C: ConnectionTrait,
    {
        super::extension_department::Entity::delete_many()
            .filter(super::extension_department::Column::ExtensionId.eq(extension_id))
            .exec(conn)
            .await?;

        if department_ids.is_empty() {
            return Ok(());
        }

        let models =
            department_ids
                .iter()
                .map(|department_id| super::extension_department::ActiveModel {
                    extension_id: Set(extension_id),
                    department_id: Set(*department_id),
                    ..Default::default()
                });

        super::extension_department::Entity::insert_many(models)
            .exec(conn)
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{department, migration::Migrator};
    use sea_orm::Database;

    #[tokio::test]
    async fn extension_can_map_to_multiple_departments() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");

        Migrator::up(&db, None)
            .await
            .expect("migrations should succeed");

        let extension = ActiveModel {
            extension: Set("1001".to_string()),
            login_disabled: Set(false),
            voicemail_disabled: Set(false),
            allow_guest_calls: Set(false),
            call_forwarding_mode: Set(Some("none".to_string())),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("insert extension");

        let sales = department::ActiveModel {
            name: Set("Sales".to_string()),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("insert sales");

        let support = department::ActiveModel {
            name: Set("Support".to_string()),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("insert support");

        Entity::replace_departments(&db, extension.id, &[sales.id, support.id])
            .await
            .expect("assign departments");

        let result = Entity::find_by_id_with_departments(&db, extension.id)
            .await
            .expect("query extension")
            .expect("extension exists");

        assert_eq!(result.1.len(), 2, "extension should have two departments");

        Entity::replace_departments(&db, extension.id, &[sales.id])
            .await
            .expect("reassign departments");

        let result = Entity::find_by_id_with_departments(&db, extension.id)
            .await
            .expect("query extension")
            .expect("extension exists");

        assert_eq!(result.1.len(), 1, "extension should have one department");
        assert_eq!(result.1[0].id, sales.id);
    }
}

#[cfg(test)]
mod org_id_tests {
    use super::*;
    use crate::models::migration::Migrator;
    use sea_orm::{ActiveModelTrait, Database};
    use sea_orm_migration::MigratorTrait;

    async fn fresh() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.expect("sqlite");
        Migrator::up(&db, None).await.expect("migrate");
        db
    }

    async fn insert_extension(db: &DatabaseConnection, ext: &str, org: &str) {
        ActiveModel {
            extension: Set(ext.to_string()),
            org_id: Set(org.to_string()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert extension");
    }

    #[tokio::test]
    async fn list_by_org_isolates_tenants() {
        let db = fresh().await;
        insert_extension(&db, "1001", "org_a").await;
        insert_extension(&db, "2001", "org_b").await;
        let a = Model::list_by_org(&db, "org_a").await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].extension, "1001");
        assert_eq!(a[0].org_id, "org_a");
    }

    #[tokio::test]
    async fn get_by_org_rejects_foreign_tenant() {
        let db = fresh().await;
        insert_extension(&db, "1001", "org_a").await;
        // The extension exists but belongs to org_a → a cross-tenant get is None.
        assert!(
            Model::get_by_org(&db, "org_b", "1001")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            Model::get_by_org(&db, "org_a", "1001")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn insert_defaults_org_id() {
        // A plain insert leaves org_id NotSet → the column default ('default')
        // applies, so existing write paths keep working until 3.1b threads a
        // real org_id.
        let db = fresh().await;
        ActiveModel {
            extension: Set("3001".to_string()),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("insert extension");
        let row = Model::get_by_org(&db, "default", "3001")
            .await
            .unwrap()
            .expect("row under default org");
        assert_eq!(row.org_id, "default");
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
                    .col(string(Column::Extension).char_len(32))
                    .col(string_null(Column::DisplayName).char_len(160))
                    .col(string_null(Column::Email).char_len(160))
                    .col(string_null(Column::Status).char_len(32))
                    .col(boolean(Column::LoginDisabled).default(false))
                    .col(boolean(Column::VoicemailDisabled).default(false))
                    .col(boolean(Column::AllowGuestCalls).default(false))
                    .col(string_null(Column::SipPassword).char_len(160))
                    .col(string_null(Column::CallForwardingMode).char_len(32))
                    .col(string_null(Column::CallForwardingDestination).char_len(160))
                    .col(integer_null(Column::CallForwardingTimeout))
                    .col(timestamp_null(Column::RegisteredAt))
                    .col(text_null(Column::Notes))
                    .col(timestamp(Column::CreatedAt).default(Expr::current_timestamp()))
                    .col(timestamp(Column::UpdatedAt).default(Expr::current_timestamp()))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_rustpbx_extensions_extension")
                    .table(Entity)
                    .col(Column::Extension)
                    .unique()
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
