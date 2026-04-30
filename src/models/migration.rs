use sea_orm_migration::{MigrationTrait, MigratorTrait};
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(super::user::Migration),
            Box::new(super::department::Migration),
            Box::new(super::extension::Migration),
            Box::new(super::extension_department::Migration),
            Box::new(super::sip_trunk::Migration),
            Box::new(super::presence::Migration),
            Box::new(super::routing::Migration),
            Box::new(crate::addons::queue::models::Migration),
            Box::new(super::call_record::Migration),
            Box::new(super::frequency_limit::Migration),
            Box::new(super::call_record_indices::Migration),
            Box::new(super::call_record_optimization_indices::Migration),
            Box::new(super::call_record_dashboard_index::Migration),
            Box::new(super::call_record_from_number_index::Migration),
            Box::new(super::add_leg_timeline_column::Migration),
            Box::new(super::add_rewrite_columns::Migration),
            Box::new(super::system_notification::Migration),
            Box::new(super::add_user_mfa_columns::Migration),
            Box::new(super::add_sip_trunk_register_columns::Migration),
            Box::new(super::rbac::Migration),
            Box::new(super::wholesale_agent::Migration),
            Box::new(super::add_sip_trunk_rewrite_hostport::Migration),
            Box::new(super::add_metadata_column::Migration),
            Box::new(super::system_config::Migration),
            Box::new(super::pending_upload::Migration),
            Box::new(super::did::Migration),
            Box::new(super::api_key::Migration),
            Box::new(super::add_sip_trunk_health_columns::Migration),
            // Phase 2 Plan 02-01 — trunk groups.
            // Order is load-bearing: trunk_group must run before
            // trunk_group_member (FK dependency), and the additive DID
            // column comes last so the ordering assertion in the plan's
            // verify block is unambiguous.
            Box::new(super::trunk_group::Migration),
            Box::new(super::trunk_group_member::Migration),
            Box::new(super::add_did_trunk_group_name_column::Migration),
            // Phase 3 Plan 03-01 — TSUB-01..03 sub-resource schema.
            // Order is load-bearing:
            //   1. Create new sub-resource tables (FK to existing rustpbx_trunk_groups.id)
            //   2. Add media_config column (additive, idempotent)
            //   3. Drop credentials column LAST so any in-flight reads succeed during deploy
            Box::new(super::trunk_credentials::Migration),
            Box::new(super::trunk_origination_uris::Migration),
            Box::new(super::add_media_config_column::Migration),
            Box::new(super::drop_credentials_column::Migration),
            // Phase 5 Plan 05-01 — TSUB-04 (capacity) + TSUB-05 (ACL).
            // Order is load-bearing (FK + safe-drop):
            //   1. Create supersip_trunk_capacity (UNIQUE FK to rustpbx_trunk_groups.id)
            //   2. Create supersip_trunk_acl_entries (FK CASCADE)
            //   3. Drop legacy rustpbx_trunk_groups.acl LAST so any in-flight reads
            //      during deploy succeed; mirrors Phase 3 D-02 ordering.
            Box::new(super::trunk_capacity::Migration),
            Box::new(super::trunk_acl_entries::Migration),
            Box::new(super::drop_acl_column::Migration),
            // Phase 6 Plan 06-01 — RTE-01/RTE-02 routing tables (records embedded as JSON column per D-01).
            // Forward-only. Legacy rustpbx_routes is intentionally untouched (D-05).
            Box::new(super::routing_tables::Migration),
            // Phase 7 Plan 07-01 — WH-01 webhooks registry.
            // Forward-only. New table only; no edits to existing tables.
            Box::new(super::webhooks::Migration),
            // Phase 8 Plan 08-01 — TRN-01 translations registry.
            // Forward-only. New table only; no edits to existing tables.
            Box::new(super::translations::Migration),
            // Phase 9 Plan 09-01 — MAN-01 manipulations registry.
            // Forward-only. New table only; no edits to existing tables.
            Box::new(super::manipulations::Migration),
        ]
    }
}
