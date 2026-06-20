use anyhow::{Context, Result};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use sea_orm_migration::{MigratorTrait, SchemaManager};
use std::collections::HashSet;
use std::time::Duration;

pub mod add_cdr_billable_duration_column;
pub mod add_cdr_status_answer_columns;
pub mod add_did_trunk_group_name_column;
pub mod add_leg_timeline_column;
pub mod add_media_config_column;
pub mod add_metadata_column;
pub mod add_org_id_did;
pub mod add_org_id_extension;
pub mod add_org_id_routing;
pub mod add_org_id_routing_tables;
pub mod add_org_id_trunk;
pub mod add_rewrite_columns;
pub mod add_sip_trunk_health_columns;
pub mod add_sip_trunk_register_columns;
pub mod add_sip_trunk_rewrite_hostport;
pub mod add_tenant_id_to_api_keys;
pub mod add_user_mfa_columns;
pub mod api_key;
pub mod backfill_dids_from_sip_trunks;
pub mod backfill_org_id_routing;
pub mod call_record;

pub mod call_record_dashboard_index;
pub mod call_record_from_number_index;
pub mod call_record_indices;
pub mod call_record_optimization_indices;
pub mod department;
pub mod did;
pub mod extension;
pub mod extension_department;
pub mod frequency_limit;
pub mod kind_schemas;
pub mod migration;
pub mod policy;
pub mod presence;
pub mod rbac;
pub mod routing;
pub mod routing_tables;
pub mod migrate_sip_trunks_to_trunks_unified;
pub mod add_trunks_last_health_check_at;
pub mod fix_sip_trunk_kind_config_booleans;
pub mod sip_trunk;
pub mod trunk;
pub mod system_config;
pub mod system_notification;
pub mod pending_upload;
pub mod trunk_acl_entries;
pub mod trunk_capacity;
pub mod trunk_credentials;
pub mod trunk_group;
pub mod trunk_group_member;
pub mod trunk_origination_uris;
pub mod user;
pub mod webhook_outbox;
pub mod webhooks;

pub fn prepare_sqlite_database(database_url: &str) -> Result<()> {
    let Some(path_part) = database_url.strip_prefix("sqlite://") else {
        return Ok(());
    };

    let (path_str, _) = path_part.split_once('?').unwrap_or((path_part, ""));
    if path_str.is_empty() || path_str.starts_with(':') {
        return Ok(());
    }

    let path = std::path::Path::new(path_str);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create directory for console database at {}",
                    parent.display()
                )
            })?;
        }

    if !path.exists() {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| {
                format!(
                    "failed to create console database file at {}",
                    path.display()
                )
            })?;
    }

    Ok(())
}

pub async fn connect_db(database_url: &str) -> Result<DatabaseConnection> {
    if database_url.starts_with("sqlite://") {
        prepare_sqlite_database(database_url).map_err(|e| {
            tracing::error!("failed to prepare SQLite database {database_url} {:?}", e);
            let msg = format!("failed to prepare SQLite database {database_url}: {e}");
            anyhow::anyhow!(msg)
        })?;
    }

    Database::connect(database_url)
        .await
        .map_err(|e: sea_orm::DbErr| {
            tracing::error!("failed to connect to database {:?}", e);
            let msg = format!("failed to connect to database {database_url}: {e}");
            anyhow::anyhow!(msg)
        })
}

pub async fn create_db(database_url: &str) -> Result<DatabaseConnection> {
    if database_url.starts_with("sqlite://") {
        prepare_sqlite_database(database_url).map_err(|e| {
            tracing::error!("failed to prepare SQLite database {database_url} {:?}", e);
            let msg = format!("failed to prepare SQLite database {database_url}: {e}");
            anyhow::anyhow!(msg)
        })?;
    }

    let mut opt = ConnectOptions::new(database_url.to_owned());

    if database_url.starts_with("sqlite://") {
        // SQLite allows only one concurrent writer. A small pool avoids
        // queueing up connections that would just wait for the WAL lock.
        // WAL mode (applied below) enables concurrent readers, so 20
        // connections comfortably handles 100 simultaneous calls where
        // most operations are reads (routing, trunk lookup) and writes
        // are infrequent bursts (CDR at call-end, presence updates).
        opt.max_connections(20)
            .min_connections(2)
            .acquire_timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(10));
    } else {
        // MySQL / PostgreSQL: genuine concurrent writes benefit from a
        // larger pool — size for ~500 concurrent calls with headroom.
        opt.max_connections(100)
            .min_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(10));
    }

    let db = Database::connect(opt)
        .await
        .map_err(|e: sea_orm::DbErr| {
            tracing::error!("failed to connect to database {:?}", e);
            let msg = format!("failed to connect to database {database_url}: {e}");
            anyhow::anyhow!(msg)
        })?;

    if database_url.starts_with("sqlite://") {
        apply_sqlite_pragmas(&db).await?;
    }

    if let Err(e) = migration::Migrator::up(&db, None).await {
        let msg = e.to_string();
        // sea-orm errors when a migration version exists in the tracking table but no
        // corresponding MigrationTrait is registered (e.g. after moving a migration to
        // an addon-specific migrator). These rows are already applied and their tables
        // exist, so it is safe to ignore the error — but `Migrator::up()` exits before
        // applying any genuinely pending migrations in that case. Fall back to applying
        // pending registered migrations one by one so a fresh refactor (e.g. trunks
        // unify) is not blocked by an unrelated orphan row.
        if msg.contains("is missing, this migration has been applied but its file is missing") {
            tracing::warn!(
                "some previously-applied migrations are no longer registered in the core \
                 migrator (likely moved to an addon); applying pending registered \
                 migrations individually: {msg}"
            );
            apply_pending_registered_migrations(&db, database_url).await?;
        } else {
            tracing::error!("failed to run database migrations on {:?}", e);
            return Err(anyhow::anyhow!(
                "failed to run database migrations on {database_url}: {e}"
            ));
        }
    }


    Ok(db)
}

/// Fallback applied when `Migrator::up()` aborts because the DB has orphan
/// `seaql_migrations` rows whose source files are no longer registered.
///
/// `Migrator::up()` calls `get_pending_migrations()`, which fails on the first
/// orphan and never returns the pending list. Here we bypass that path:
/// query `seaql_migrations` directly for already-applied versions, then walk
/// `Migrator::migrations()` and apply any that are missing. Each registered
/// migration's `up()` is expected to be idempotent (the codebase already
/// follows that convention — see e.g. `migrate_sip_trunks_to_trunks_unified`).
async fn apply_pending_registered_migrations(
    db: &DatabaseConnection,
    database_url: &str,
) -> Result<()> {
    let backend = db.get_database_backend();
    let table = migration::Migrator::migration_table_name().to_string();

    let select_sql = match backend {
        DbBackend::Postgres => format!("SELECT version FROM \"{table}\""),
        DbBackend::MySql => format!("SELECT version FROM `{table}`"),
        DbBackend::Sqlite => format!("SELECT version FROM \"{table}\""),
    };
    let rows = db
        .query_all(Statement::from_string(backend, select_sql))
        .await
        .with_context(|| format!("failed to read {table}"))?;
    let applied: HashSet<String> = rows
        .into_iter()
        .filter_map(|row| row.try_get::<String>("", "version").ok())
        .collect();

    let manager = SchemaManager::new(db);
    for boxed in migration::Migrator::migrations() {
        let name = boxed.name().to_string();
        if applied.contains(&name) {
            continue;
        }
        tracing::info!("Applying pending migration '{}'", name);
        boxed.up(&manager).await.map_err(|e| {
            tracing::error!("failed to apply migration '{}' on {:?}", name, e);
            anyhow::anyhow!(
                "failed to apply pending migration '{name}' on {database_url}: {e}"
            )
        })?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("SystemTime before UNIX EPOCH!")
            .as_secs() as i64;
        let insert_sql = match backend {
            DbBackend::MySql => format!(
                "INSERT INTO `{table}` (version, applied_at) VALUES (?, ?)"
            ),
            _ => format!(
                "INSERT INTO \"{table}\" (version, applied_at) VALUES (?, ?)"
            ),
        };
        db.execute(Statement::from_sql_and_values(
            backend,
            insert_sql,
            [name.clone().into(), now.into()],
        ))
        .await
        .with_context(|| format!("failed to record applied migration '{name}'"))?;
        tracing::info!("Migration '{}' has been applied", name);
    }
    Ok(())
}

/// Applies SQLite PRAGMA settings that improve concurrency and throughput
/// for a high-call-volume SIP gateway.
///
/// `journal_mode=WAL` is the most critical setting: it is database-level
/// and persists across restarts, so it only needs to be set once.  The
/// remaining PRAGMAs are connection-level and applied here to the first
/// connection drawn from the pool; they take effect for the lifetime of
/// that connection. New pool connections inherit the WAL setting from the
/// database file but will use SQLite defaults for the others — acceptable
/// because WAL mode already eliminates the primary source of contention.
async fn apply_sqlite_pragmas(db: &DatabaseConnection) -> Result<()> {
    let pragmas: &[(&str, &str)] = &[
        // WAL (Write-Ahead Log): allows concurrent readers alongside the
        // single writer. Eliminates reader-writer lock contention that
        // occurs in default DELETE journal mode when CDR writes and
        // presence updates fire while routing lookups are in progress.
        // Database-level — persists in the SQLite file across restarts.
        ("journal_mode", "WAL"),

        // NORMAL is safe with WAL (the WAL file itself provides durability)
        // and is roughly 3× faster than the default FULL mode.
        ("synchronous", "NORMAL"),

        // Wait up to 5 s before returning SQLITE_BUSY instead of failing
        // immediately. Smooths over the brief exclusive-lock window during
        // WAL checkpoints and schema migrations.
        ("busy_timeout", "5000"),

        // 64 MB shared page cache. Keeps hot pages (routes, trunks,
        // extensions, recent CDR) in memory and reduces I/O on every
        // call setup. Value is negative → interpreted as kibibytes.
        ("cache_size", "-65536"),

        // Temporary tables and sort buffers live in RAM rather than in a
        // temp file. Speeds up queries that use implicit temp storage
        // (ORDER BY, GROUP BY, sub-queries).
        ("temp_store", "MEMORY"),
    ];

    for (key, value) in pragmas {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            format!("PRAGMA {key}={value}"),
        ))
        .await
        .map_err(|e| anyhow::anyhow!("failed to apply 'PRAGMA {key}={value}': {e}"))?;
    }

    tracing::info!(
        "SQLite tuned for high concurrency: WAL mode, 64 MB cache, \
         5 s busy-timeout, synchronous=NORMAL"
    );

    Ok(())
}
