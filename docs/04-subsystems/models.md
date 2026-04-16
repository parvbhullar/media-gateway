# Models

## What it does

The models module contains all SeaORM database entities, migrations, and
database initialization logic. It defines the schema for every persistent
object in SuperSip and supports SQLite, MySQL, and PostgreSQL backends.
SQLite connections are tuned with WAL mode, 64 MB cache, and busy timeout
for high-concurrency SIP workloads.

## Key types & entry points

- **`create_db()`** — connects to the database, applies SQLite pragmas if needed, and runs all pending migrations. `src/models/mod.rs`
- **`prepare_sqlite_database()`** — ensures the SQLite file and parent directories exist. `src/models/mod.rs`
- **`Migrator`** — SeaORM migration runner with all schema migrations. `src/models/migration.rs`

## Sub-modules (entities)

Each entity module defines a SeaORM `Model`, `Entity`, `ActiveModel`, and `Column` enum:

- `api_key` — API key management for Bearer-token auth
- `call_record` — CDR storage with indices (dashboard, from-number, optimization)
- `department` — Organizational departments
- `did` — Direct Inward Dialing numbers
- `extension` — User extensions (SIP endpoints)
- `extension_department` — Extension-to-department mapping
- `frequency_limit` — Rate limiting rules
- `policy` — Call policies
- `presence` — User presence state
- `rbac` — Roles and permissions (role, user_role, role_permission)
- `routing` — Routing rules (database-stored)
- `sip_trunk` — SIP trunk configuration and health
- `system_config` — Key-value system configuration
- `system_notification` — System notification storage
- `pending_upload` — Failed S3 upload queue for retry scheduler
- `user` — User accounts with MFA support
- `wholesale_agent` — Wholesale agent configuration
- `trunk_group` / `trunk_group_member` — Trunk group definitions and membership

## Migration modules

- `add_did_trunk_group_name_column` — Adds trunk group name to DIDs
- `add_leg_timeline_column` — Adds leg timeline to call records
- `add_metadata_column` — Adds metadata column
- `add_rewrite_columns` — Adds rewrite tracking columns to call records
- `add_sip_trunk_health_columns` — Adds health monitoring columns to SIP trunks
- `add_sip_trunk_register_columns` — Adds registration columns to SIP trunks
- `add_sip_trunk_rewrite_hostport` — Adds host:port rewrite to SIP trunks
- `add_user_mfa_columns` — Adds MFA columns to users
- `backfill_dids_from_sip_trunks` — Data migration for DIDs
- `call_record_*_index` — Various CDR query optimization indices

## Configuration

Database URL is configured via `database_url` in the main config. The
module auto-detects the backend (SQLite, MySQL, PostgreSQL) and applies
appropriate connection pool settings.

## Public API surface

The models module does not expose HTTP routes. It is used by all other
modules for database access.

## See also

- [callrecord.md](callrecord.md) — CDR generation that writes to call_record
- [console.md](console.md) — Console UI that reads/writes all entities
- [handler.md](handler.md) — API layer that queries entities

---
**Status:** ✅ Shipped
**Source:** `src/models/`
**Related phases:** Phase 2 adds `trunk_group` and `trunk_group_member`
**Last reviewed:** 2026-04-16
