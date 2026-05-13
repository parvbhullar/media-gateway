//! Migration: `rustpbx_sip_trunks` → `rustpbx_trunks` (unified trunk model).
//!
//! Renames the SIP-only trunk table to the kind-agnostic `rustpbx_trunks`,
//! adds `kind` discriminator + `kind_config: Json`, packs all SIP-specific
//! typed columns into `kind_config`, then drops those columns.
//!
//! See plan: `/home/anuj/.claude/plans/imperative-sauteeing-cake.md` (Phase 1).
//!
//! Dialect support: SQLite (3.35+, for `DROP COLUMN`) and MySQL (5.7+).
//! Idempotent: each step is guarded by a `has_table` / `has_column` check so
//! re-running mid-failure does not double-apply.

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const OLD_TABLE: &str = "rustpbx_sip_trunks";
const NEW_TABLE: &str = "rustpbx_trunks";

/// SIP-specific columns that get packed into `kind_config` then dropped.
/// Order matters for the JSON-object construction below.
const SIP_COLUMNS: &[&str] = &[
    "sip_server",
    "sip_transport",
    "outbound_proxy",
    "auth_username",
    "auth_password",
    "register_enabled",
    "register_expires",
    "register_extra_headers",
    "rewrite_hostport",
    "did_numbers",
    "incoming_from_user_prefix",
    "incoming_to_user_prefix",
    "default_route_label",
    "billing_snapshot",
    "analytics",
    "carrier",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();

        // 1. Rename table (idempotent — skip if already renamed).
        let has_old = manager.has_table(OLD_TABLE).await?;
        let has_new = manager.has_table(NEW_TABLE).await?;
        if has_old && !has_new {
            let sql = match backend {
                DbBackend::Sqlite => format!(
                    "ALTER TABLE \"{OLD_TABLE}\" RENAME TO \"{NEW_TABLE}\""
                ),
                DbBackend::MySql => format!(
                    "RENAME TABLE `{OLD_TABLE}` TO `{NEW_TABLE}`"
                ),
                DbBackend::Postgres => format!(
                    "ALTER TABLE \"{OLD_TABLE}\" RENAME TO \"{NEW_TABLE}\""
                ),
            };
            conn.execute(Statement::from_string(backend, sql)).await?;
        } else if !has_old && !has_new {
            // Neither exists — first-run on a fresh DB before sip_trunk
            // migration ran. Should not happen because sip_trunk::Migration
            // is registered earlier in the list, but bail clean.
            return Ok(());
        }

        // 2. Add `kind` column (default 'sip').
        if !manager.has_column(NEW_TABLE, "kind").await? {
            let sql = match backend {
                DbBackend::Sqlite | DbBackend::Postgres => format!(
                    "ALTER TABLE \"{NEW_TABLE}\" ADD COLUMN \"kind\" TEXT NOT NULL DEFAULT 'sip'"
                ),
                DbBackend::MySql => format!(
                    "ALTER TABLE `{NEW_TABLE}` ADD COLUMN `kind` VARCHAR(32) NOT NULL DEFAULT 'sip'"
                ),
            };
            conn.execute(Statement::from_string(backend, sql)).await?;
        }

        // 3. Add `kind_config` JSON column, nullable initially. We'll
        //    populate it from the SIP columns and then flip it to NOT NULL.
        if !manager.has_column(NEW_TABLE, "kind_config").await? {
            let sql = match backend {
                DbBackend::Sqlite => format!(
                    "ALTER TABLE \"{NEW_TABLE}\" ADD COLUMN \"kind_config\" JSON"
                ),
                DbBackend::MySql => format!(
                    "ALTER TABLE `{NEW_TABLE}` ADD COLUMN `kind_config` JSON NULL"
                ),
                DbBackend::Postgres => format!(
                    "ALTER TABLE \"{NEW_TABLE}\" ADD COLUMN \"kind_config\" JSONB"
                ),
            };
            conn.execute(Statement::from_string(backend, sql)).await?;
        }

        // 4. Pack the SIP-specific columns into `kind_config`.
        //    We only run the UPDATE if the SIP columns are still present
        //    (otherwise the migration is being re-run after columns dropped).
        let still_has_sip_columns = manager.has_column(NEW_TABLE, "sip_server").await?;
        if still_has_sip_columns {
            let pack_sql = build_pack_sql(backend);
            conn.execute(Statement::from_string(backend, pack_sql)).await?;
        }

        // 5. Drop the SIP-specific columns. SQLite 3.35+ and MySQL 5.7+ both
        //    support `ALTER TABLE ... DROP COLUMN` directly.
        for col in SIP_COLUMNS {
            if manager.has_column(NEW_TABLE, col).await? {
                let sql = match backend {
                    DbBackend::Sqlite | DbBackend::Postgres => format!(
                        "ALTER TABLE \"{NEW_TABLE}\" DROP COLUMN \"{col}\""
                    ),
                    DbBackend::MySql => format!(
                        "ALTER TABLE `{NEW_TABLE}` DROP COLUMN `{col}`"
                    ),
                };
                conn.execute(Statement::from_string(backend, sql)).await?;
            }
        }

        // 6. Flip `kind_config` to NOT NULL. SQLite cannot alter a column's
        //    nullability in place; we rely on the application layer to
        //    always write a non-null value and skip the SQL flip for SQLite.
        //    MySQL/Postgres support `MODIFY` / `SET NOT NULL`.
        match backend {
            DbBackend::MySql => {
                let sql = format!(
                    "ALTER TABLE `{NEW_TABLE}` MODIFY COLUMN `kind_config` JSON NOT NULL"
                );
                conn.execute(Statement::from_string(backend, sql)).await?;
            }
            DbBackend::Postgres => {
                let sql = format!(
                    "ALTER TABLE \"{NEW_TABLE}\" ALTER COLUMN \"kind_config\" SET NOT NULL"
                );
                conn.execute(Statement::from_string(backend, sql)).await?;
            }
            DbBackend::Sqlite => {
                // SQLite: can't easily alter nullability. Application layer
                // enforces. (A full table rebuild would work but is overkill.)
            }
        }

        // 7. Rebuild indexes under the new table name.
        // Drop legacy SIP-prefixed indexes if present; ignore failures.
        for legacy in [
            "idx_rustpbx_sip_trunks_name",
            "idx_rustpbx_sip_trunks_status",
            "idx_rustpbx_sip_trunks_direction",
        ] {
            let drop = match backend {
                DbBackend::MySql => format!(
                    "DROP INDEX `{legacy}` ON `{NEW_TABLE}`"
                ),
                DbBackend::Sqlite | DbBackend::Postgres => format!(
                    "DROP INDEX IF EXISTS \"{legacy}\""
                ),
            };
            let _ = conn.execute(Statement::from_string(backend, drop)).await;
        }

        // Create new indexes (idempotent via IF NOT EXISTS on SQLite/PG;
        // MySQL has no IF NOT EXISTS for indexes, so we swallow duplicate errors).
        let new_indexes: &[(&str, &str, bool)] = &[
            ("trunks_name_idx", "name", true),
            ("trunks_kind_idx", "kind", false),
            ("trunks_active_idx", "is_active", false),
        ];
        for (idx_name, col, unique) in new_indexes {
            let unique_kw = if *unique { "UNIQUE " } else { "" };
            let sql = match backend {
                DbBackend::Sqlite | DbBackend::Postgres => format!(
                    "CREATE {unique_kw}INDEX IF NOT EXISTS \"{idx_name}\" ON \"{NEW_TABLE}\" (\"{col}\")"
                ),
                DbBackend::MySql => format!(
                    "CREATE {unique_kw}INDEX `{idx_name}` ON `{NEW_TABLE}` (`{col}`)"
                ),
            };
            let res = conn.execute(Statement::from_string(backend, sql)).await;
            if let Err(e) = res {
                // MySQL: ignore duplicate-key-name (errno 1061).
                let msg = e.to_string();
                if !msg.contains("1061") && !msg.contains("Duplicate key name") {
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Non-reversible data-moving migration. Down is a no-op; restoring
        // requires a backup.
        Ok(())
    }
}

/// Build the dialect-specific UPDATE that packs all SIP columns into
/// `kind_config`. Uses `json_object` (SQLite, MySQL) or `json_build_object`
/// (Postgres).
fn build_pack_sql(backend: DbBackend) -> String {
    match backend {
        DbBackend::Sqlite => {
            let pairs = SIP_COLUMNS
                .iter()
                .map(|c| format!("'{c}', \"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "UPDATE \"{NEW_TABLE}\" SET kind_config = json_object({pairs}) \
                 WHERE kind_config IS NULL"
            )
        }
        DbBackend::MySql => {
            let pairs = SIP_COLUMNS
                .iter()
                .map(|c| format!("'{c}', `{c}`"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "UPDATE `{NEW_TABLE}` SET kind_config = JSON_OBJECT({pairs}) \
                 WHERE kind_config IS NULL"
            )
        }
        DbBackend::Postgres => {
            let pairs = SIP_COLUMNS
                .iter()
                .map(|c| format!("'{c}', \"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "UPDATE \"{NEW_TABLE}\" SET kind_config = json_build_object({pairs}) \
                 WHERE kind_config IS NULL"
            )
        }
    }
}
