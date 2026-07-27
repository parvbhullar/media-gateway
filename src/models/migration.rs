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
            Box::new(super::call_record::Migration),
            Box::new(super::frequency_limit::Migration),
            Box::new(super::call_record_indices::Migration),
            Box::new(super::call_record_optimization_indices::Migration),
            Box::new(super::call_record_dashboard_index::Migration),
            Box::new(super::call_record_from_number_index::Migration),
            Box::new(super::add_leg_timeline_column::Migration),
            Box::new(super::add_ring_time_column::Migration),
            Box::new(super::add_rewrite_columns::Migration),
            Box::new(super::system_notification::Migration),
            Box::new(super::add_user_mfa_columns::Migration),
            Box::new(super::add_sip_trunk_register_columns::Migration),
            Box::new(super::rbac::Migration),
            Box::new(super::add_sip_trunk_rewrite_hostport::Migration),
            Box::new(super::add_metadata_column::Migration),
            Box::new(super::system_config::Migration),
            Box::new(super::pending_upload::Migration),
            Box::new(super::did::Migration),
            Box::new(super::api_key::Migration),
            Box::new(super::add_sip_trunk_health_columns::Migration),
            Box::new(super::trunk_group::Migration),
            Box::new(super::trunk_group_member::Migration),
            Box::new(super::add_did_trunk_group_name_column::Migration),
            Box::new(super::trunk_credentials::Migration),
            Box::new(super::trunk_origination_uris::Migration),
            Box::new(super::add_media_config_column::Migration),
            Box::new(super::trunk_capacity::Migration),
            Box::new(super::trunk_acl_entries::Migration),
            Box::new(super::routing_tables::Migration),
            Box::new(super::webhooks::Migration),
            Box::new(super::migrate_sip_trunks_to_trunks_unified::Migration),
            Box::new(super::add_trunks_last_health_check_at::Migration),
            Box::new(super::fix_sip_trunk_kind_config_booleans::Migration),
            Box::new(super::add_cdr_status_answer_columns::Migration),
            // task 2.1 — durable webhook delivery queue. MUST be registered
            // here or the table is never created in production (the migrator
            // runs only this explicit list).
            Box::new(super::webhook_outbox::Migration),
            // task 2.3 — billable (answered) duration column on call records.
            Box::new(super::add_cdr_billable_duration_column::Migration),
            // task 3.1 — org_id tenant-isolation column on rustpbx_dids. MUST be
            // registered here or the column is never created in production.
            Box::new(super::add_org_id_did::Migration),
            // task 3.1 — org_id tenant-isolation columns on extensions, trunks,
            // routes, and routing tables (the `did` template replicated). Each
            // MUST be registered here or the column is never created in
            // production. `add_org_id_routing` is the additive slice; the
            // owner→org_id reconcile (drop `owner`) is deferred.
            Box::new(super::add_org_id_extension::Migration),
            Box::new(super::add_org_id_trunk::Migration),
            Box::new(super::add_org_id_routing::Migration),
            Box::new(super::add_org_id_routing_tables::Migration),
            // task 3.1 reconcile — backfill org_id from the legacy `owner`
            // column on rustpbx_routes, then drop `owner`. MUST run AFTER
            // add_org_id_routing (which creates org_id).
            Box::new(super::backfill_org_id_routing::Migration),
            // task 3.2 — tenant_id scoping column on rustpbx_api_keys (the
            // request-context tenant/org source). MUST be registered here.
            Box::new(super::add_tenant_id_to_api_keys::Migration),
            // 503-attribution — failure_source column on rustpbx_call_records.
            Box::new(super::add_cdr_failure_source_column::Migration),
            // enrich-cdr-api — outcome_kind / q850_cause / q850_text /
            // sipflow_available columns + grouped-summary index + base backfill.
            // MUST run after the table + failure_source exist.
            Box::new(super::add_cdr_outcome_columns::Migration),
        ]
    }
}
