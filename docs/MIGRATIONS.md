# Migration Audit — docs/MIGRATIONS.md

**Created:** Phase 12, Plan 12-04 (MIG-02)
**Source:** `src/models/migration.rs::Migrator::migrations()`
**Policy:** Every new migration must include a documented rollback path in its phase plan.
**Forward-only policy (D-26):** When `down()` rollback is infeasible (data destruction,
irreversible backfills, config-state tables), mark reversible=no with the reason.

## Migration Register

| # | migration_name | up_summary | reversible | down_summary | notes |
|---|----------------|------------|------------|--------------|-------|
| 1 | user | Create rustpbx_users table | yes | drop_table rustpbx_users | Phase 0 foundation |
| 2 | department | Create rustpbx_departments table | yes | drop_table rustpbx_departments | Phase 0 foundation |
| 3 | extension | Create rustpbx_extensions table | yes | drop_table rustpbx_extensions | Phase 0 foundation |
| 4 | extension_department | Create rustpbx_extension_departments join table | yes | drop_table rustpbx_extension_departments | Phase 0 foundation |
| 5 | sip_trunk | Create rustpbx_sip_trunks table | yes | drop_table rustpbx_sip_trunks | Phase 0 foundation |
| 6 | presence | Create presence_states table | yes | drop_table presence_states | Phase 0 foundation |
| 7 | routing | Create rustpbx_routes table (legacy routing) | yes | drop_table rustpbx_routes | Phase 0 legacy routing — superseded by routing_tables (Phase 6) but not removed |
| 8 | queue (addon) | Create rustpbx_queues table via addons::queue::models::Migration | yes | drop_table rustpbx_queues | Addon migration registered in core migrator for unified apply |
| 9 | call_record | Create rustpbx_call_records table | yes | drop_table rustpbx_call_records | Primary CDR store |
| 10 | frequency_limit | Create frequency_limit table | yes | drop_table frequency_limit | Rate-limiting store |
| 11 | call_record_indices | Add indices on rustpbx_call_records (direction, status, started_at) | yes | drop_index on each index | Phase 0 query performance |
| 12 | call_record_optimization_indices | Add composite/optimization indices on rustpbx_call_records | yes | drop_index on each index | Phase 0 query performance |
| 13 | call_record_dashboard_index | Add dashboard query index on rustpbx_call_records | yes | drop_index | Phase 0 dashboard performance |
| 14 | call_record_from_number_index | Add from_number index on rustpbx_call_records | yes | drop_index | Phase 0 query performance |
| 15 | add_leg_timeline_column | Add leg_timeline JSON column to rustpbx_call_records | no | no-op (forward-only) | Additive nullable column; no DROP COLUMN in down(). Safe to leave on rollback |
| 16 | add_rewrite_columns | Add rewrite_original_from / rewrite_original_to columns to rustpbx_call_records | no | no-op (forward-only) | Additive nullable columns; safe to leave on rollback |
| 17 | system_notification | Create system_notification table | yes | drop_table system_notification | Phase 0 notifications |
| 18 | add_user_mfa_columns | Add MFA columns to rustpbx_users | no | no-op (forward-only) | Additive nullable columns; removing would break MFA runtime |
| 19 | add_sip_trunk_register_columns | Add SIP registration columns to rustpbx_sip_trunks | no | no-op (forward-only) | Additive nullable columns; safe to leave on rollback |
| 20 | rbac | Create rustpbx_roles, rustpbx_role_permissions, rustpbx_user_roles tables (with seeded system roles) | yes | drop_table rustpbx_user_roles, rustpbx_role_permissions, rustpbx_roles (in FK-safe order) | Phase 0 RBAC foundation |
| 21 | wholesale_agent | Create wholesale_agent table | yes | drop_table wholesale_agent | Phase 0 billing agent |
| 22 | add_sip_trunk_rewrite_hostport | Add rewrite_hostport column to rustpbx_sip_trunks | no | no-op (forward-only) | Additive nullable column; safe to leave on rollback |
| 23 | add_metadata_column | Add metadata JSON column to rustpbx_call_records | no | no-op (forward-only) | Additive nullable column; removing would require a new migration |
| 24 | system_config | Create rustpbx_system_config table | yes | drop_table rustpbx_system_config | Phase 0 system config store |
| 25 | pending_upload | Create rustpbx_pending_uploads table | yes | drop_table rustpbx_pending_uploads | S3 retry scheduler store |
| 26 | did | Create rustpbx_dids table | yes | drop_table rustpbx_dids | Phase 1 DID store |
| 27 | api_key | Create rustpbx_api_keys table | yes | drop_table rustpbx_api_keys | Phase 1 API key store |
| 28 | add_sip_trunk_health_columns | Add health threshold columns to rustpbx_sip_trunks | no | no-op (forward-only) | Additive nullable columns; safe to leave on rollback |
| 29 | trunk_group | Create rustpbx_trunk_groups table | yes | drop_table rustpbx_trunk_groups | Phase 2 Plan 02-01 — TRK-01 |
| 30 | trunk_group_member | Create rustpbx_trunk_group_members join table | yes | drop_table rustpbx_trunk_group_members | Phase 2 Plan 02-01 — TRK-01; FK dependency on trunk_groups |
| 31 | add_did_trunk_group_name_column | Add trunk_group_name column to rustpbx_dids | no | no-op (forward-only) | Phase 2 Plan 02-01 additive column; safe to leave on rollback |
| 32 | trunk_credentials | Create supersip_trunk_credentials table | yes | drop_table supersip_trunk_credentials | Phase 3 Plan 03-01 — TSUB-01 |
| 33 | trunk_origination_uris | Create supersip_trunk_origination_uris table | yes | drop_table supersip_trunk_origination_uris | Phase 3 Plan 03-01 — TSUB-02 |
| 34 | add_media_config_column | Add media_config JSON column to rustpbx_trunk_groups | no | no-op (forward-only) | Phase 3 Plan 03-01 — TSUB-03 additive column |
| 35 | drop_credentials_column | Drop credentials column from rustpbx_trunk_groups | no | no-op (forward-only) | Phase 3 Plan 03-01 D-02: data migrated to trunk_credentials before drop; restoring requires a new migration |
| 36 | trunk_capacity | Create supersip_trunk_capacity table | yes | drop_table supersip_trunk_capacity | Phase 5 Plan 05-01 — TSUB-04 |
| 37 | trunk_acl_entries | Create supersip_trunk_acl_entries table | yes | drop_table supersip_trunk_acl_entries | Phase 5 Plan 05-01 — TSUB-05 |
| 38 | drop_acl_column | Drop acl column from rustpbx_trunk_groups | no | no-op (forward-only) | Phase 5 Plan 05-01: data migrated to trunk_acl_entries before drop; restoring requires a new migration |
| 39 | routing_tables | Create supersip_routing_tables table + unique index on name | yes | drop_table supersip_routing_tables | Phase 6 Plan 06-01 — RTE-01/RTE-02; down() is drop_table (verified). Legacy rustpbx_routes untouched per D-05 |
| 40 | webhooks | Create supersip_webhooks table + unique index on name | no | no-op (forward-only) | Phase 7 Plan 07-01 — WH-01. Forward-only per Phase 6 D-05 convention: operator rollbacks must not drop webhook config rows |
| 41 | translations | Create supersip_translations table + unique index on name | no | no-op (forward-only) | Phase 8 Plan 08-01 — TRN-01. Forward-only per Phase 6 D-05 convention: operator rollbacks must not drop translation config rows |
| 42 | manipulations | Create supersip_manipulations table + unique index on name | no | no-op (forward-only) | Phase 9 Plan 09-01 — MAN-01. Forward-only per Phase 6 D-05 / Phase 8 convention: operator rollbacks must not drop manipulation config rows |
| 43 | security_rules | Create supersip_security_rules table + index on position | no | no-op (forward-only) | Phase 10 Plan 10-01 — SEC-01. Forward-only per Phase 6 D-05 / Phase 8 convention: rollbacks must not drop firewall rules |
| 44 | security_blocks | Create supersip_security_blocks table + unique index on (ip, realm) | no | no-op (forward-only) | Phase 10 Plan 10-01 — SEC-03. Forward-only per Phase 6 D-05 / Phase 8 convention: rollbacks must not drop security block records |
| 45 | add_account_id_to_all_tables | Create supersip_sub_accounts (seeded `'root'` master row) + add `account_id VARCHAR(64) NOT NULL DEFAULT 'root'` and `idx_<table>_account_id` to 17 existing CRUD tables | yes | drop idx_<table>_account_id + drop column account_id on each of the 17 tables in REVERSE order, then drop_table supersip_sub_accounts | Phase 13 Plan 01a — TEN-01 / TEN-06 big-bang multi-tenancy foundation. Idempotent via has_column / has_index guards; seed insert uses NOT EXISTS so re-runs are safe. Existing rows backfill atomically through DEFAULT 'root'. Skips legacy `rustpbx_routes` per Phase 6 D-05 — only modern `supersip_routing_tables` is touched in the routing surface. Tables touched: rustpbx_sip_trunks, rustpbx_dids, rustpbx_trunk_groups, rustpbx_trunk_group_members, supersip_trunk_credentials, supersip_trunk_origination_uris, supersip_trunk_capacity, supersip_trunk_acl_entries, supersip_routing_tables, supersip_manipulations, supersip_translations, supersip_security_rules, supersip_security_blocks, rustpbx_frequency_limits, supersip_webhooks, rustpbx_api_keys, rustpbx_call_records |

## Summary

| Classification | Count |
|----------------|-------|
| Reversible (yes) | 27 |
| Forward-only (no) | 17 |
| Total | 44 |

## Forward-Only Policy

A migration is marked **forward-only** when:
1. The `down()` method is a no-op (`Ok(())`), AND
2. One of the following applies:
   - The column/table holds runtime operator configuration that must survive rollbacks
     (webhooks, translations, manipulations, security rules, security blocks)
   - The column is additive and nullable — removing it would require a separate migration
     and could break existing code referencing the column
   - The `up()` drops a column — the inverse would require recreating the column and
     potentially restoring lost data (drop_credentials_column, drop_acl_column)

## Maintenance Instructions

When adding a new migration:
1. Implement a real `down()` if the operation is feasibly reversible (table create -> drop_table).
2. If forward-only (config tables, additive-backfill, destructive-drop), write a no-op `down()`
   with an explicit comment: `// Forward-only per Phase N D-XX convention. <reason>.`
3. Add a row to this table in the same plan that introduces the migration.
4. Do NOT leave `down()` unimplemented or panicking — use `Ok(())` for forward-only.
