//! Phase 13 Plan 01a — big-bang multi-tenancy migration (TEN-01 / TEN-06).
//!
//! Single reversible migration that:
//!
//! 1. Creates `supersip_sub_accounts` (master/system tenant table) with the
//!    seeded `'root'` row that every existing CRUD row backfills onto.
//! 2. Adds `account_id VARCHAR(64) NOT NULL DEFAULT 'root'` plus
//!    `idx_<table>_account_id` to every existing CRUD table the api_v1
//!    surface touches (17 tables, see `TABLES`).
//!
//! Idempotency guarantees (CONTEXT.md D-01 / D-08, threat T-13.01a-01/02/03):
//!
//! - `create_table(...).if_not_exists()` for `supersip_sub_accounts`.
//! - `INSERT ... WHERE NOT EXISTS` for the seed row so re-running `up()`
//!   does not duplicate the master tenant.
//! - `has_column` guard before each `add_column`.
//! - `has_index` guard before each `create_index`.
//!
//! Reversibility (CONTEXT.md D-08):
//!
//! - `down()` drops every `idx_<table>_account_id` and `account_id` column
//!   in REVERSE order, then drops `supersip_sub_accounts`.
//!
//! `rustpbx_routes` (legacy routing — Phase 6 D-05) is intentionally skipped.
//! Only the modern `supersip_routing_tables` is touched on the routing
//! surface.

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_query::{Alias, ColumnDef};

/// The 17 existing CRUD tables that receive `account_id` + index.
///
/// Order is load-bearing — `down()` iterates in reverse so the drop happens
/// in mirror image. Stored as plain string identifiers because the migration
/// references entities across many modules; using `Alias::new` keeps the
/// migration body decoupled from the entity types and matches the
/// `add_sip_trunk_register_columns.rs` raw-table-name pattern.
const TABLES: &[&str] = &[
    "rustpbx_sip_trunks",
    "rustpbx_dids",
    "rustpbx_trunk_groups",
    "rustpbx_trunk_group_members",
    "supersip_trunk_credentials",
    "supersip_trunk_origination_uris",
    "supersip_trunk_capacity",
    "supersip_trunk_acl_entries",
    "supersip_routing_tables",
    "supersip_manipulations",
    "supersip_translations",
    "supersip_security_rules",
    "supersip_security_blocks",
    "rustpbx_frequency_limits",
    "supersip_webhooks",
    "rustpbx_api_keys",
    "rustpbx_call_records",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Create supersip_sub_accounts (idempotent via if_not_exists).
        manager
            .create_table(
                Table::create()
                    .table(super::supersip_sub_accounts::Entity)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(super::supersip_sub_accounts::Column::Id)
                            .string_len(64)
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(super::supersip_sub_accounts::Column::Name)
                            .string_len(128)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(super::supersip_sub_accounts::Column::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(super::supersip_sub_accounts::Column::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(super::supersip_sub_accounts::Column::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        if !manager
            .has_index("supersip_sub_accounts", "idx_supersip_sub_accounts_name")
            .await?
        {
            manager
                .create_index(
                    Index::create()
                        .if_not_exists()
                        .name("idx_supersip_sub_accounts_name")
                        .table(super::supersip_sub_accounts::Entity)
                        .col(super::supersip_sub_accounts::Column::Name)
                        .unique()
                        .to_owned(),
                )
                .await?;
        }

        // 2. Seed the 'root' master row idempotently. Use raw SQL with
        //    NOT EXISTS so re-running up() is safe (T-13.01a-02).
        seed_root_row(manager).await?;

        // 3. Add account_id + idx_<table>_account_id to each of the 17
        //    existing CRUD tables. has_column / has_index guards make every
        //    step idempotent (T-13.01a-01).
        for table in TABLES {
            if !manager.has_column(*table, "account_id").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(*table))
                            .add_column(
                                ColumnDef::new(Alias::new("account_id"))
                                    .string_len(64)
                                    .not_null()
                                    .default("root"),
                            )
                            .to_owned(),
                    )
                    .await?;
            }

            let idx_name = format!("idx_{}_account_id", table);
            if !manager.has_index(*table, &idx_name).await? {
                manager
                    .create_index(
                        Index::create()
                            .if_not_exists()
                            .name(&idx_name)
                            .table(Alias::new(*table))
                            .col(Alias::new("account_id"))
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Reverse order: drop each table's index + account_id column, then
        // drop supersip_sub_accounts last.
        for table in TABLES.iter().rev() {
            let idx_name = format!("idx_{}_account_id", table);
            if manager.has_index(*table, &idx_name).await? {
                manager
                    .drop_index(
                        Index::drop()
                            .name(&idx_name)
                            .table(Alias::new(*table))
                            .to_owned(),
                    )
                    .await?;
            }

            if manager.has_column(*table, "account_id").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(*table))
                            .drop_column(Alias::new("account_id"))
                            .to_owned(),
                    )
                    .await?;
            }
        }

        manager
            .drop_table(
                Table::drop()
                    .table(super::supersip_sub_accounts::Entity)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

/// Insert the immutable `'root'` master tenant if (and only if) no row with
/// `id = 'root'` already exists. Uses backend-aware SQL because SeaORM
/// migration-time inserts via the query builder do not expose a portable
/// `INSERT ... WHERE NOT EXISTS` shortcut.
async fn seed_root_row(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    let backend = conn.get_database_backend();

    // current_timestamp() rendered per-backend so the seed survives a
    // sqlite::memory: + postgres mix.
    let now = match backend {
        DatabaseBackend::Postgres => "NOW()",
        DatabaseBackend::MySql => "NOW()",
        DatabaseBackend::Sqlite => "CURRENT_TIMESTAMP",
    };

    let sql = format!(
        "INSERT INTO supersip_sub_accounts (id, name, enabled, created_at, updated_at) \
         SELECT 'root', 'Master Account', {true_lit}, {now}, {now} \
         WHERE NOT EXISTS (SELECT 1 FROM supersip_sub_accounts WHERE id = 'root')",
        true_lit = match backend {
            DatabaseBackend::Postgres => "TRUE",
            DatabaseBackend::MySql => "1",
            DatabaseBackend::Sqlite => "1",
        },
        now = now,
    );

    conn.execute(Statement::from_string(backend, sql)).await?;
    Ok(())
}
