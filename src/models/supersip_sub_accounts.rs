//! `supersip_sub_accounts` — Phase 13 multi-tenancy master table (TEN-01).
//!
//! Phase 13 Plan 01a storage. One row per sub-account. The literal `'root'`
//! row is the master/system account — created via the big-bang
//! `add_account_id_to_all_tables` migration, immutable, cannot be deleted.
//!
//! Per CONTEXT.md D-02 the `id` is operator-supplied at create time
//! (slug-style: lowercase, alphanumeric + `_-`, max 64 chars). The handler
//! that lands in 13-01c is responsible for slug validation; this entity only
//! captures the column shape.
//!
//! No FK relations are declared here — Phase 13 deliberately avoids cross-
//! prefix FKs from existing tables to `supersip_sub_accounts.id` to keep the
//! big-bang migration purely additive (D-08). Referential integrity for the
//! `account_id` column on every other CRUD table is enforced at the handler
//! layer (13-01b/c/d).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "supersip_sub_accounts")]
pub struct Model {
    #[sea_orm(
        primary_key,
        auto_increment = false,
        column_type = "String(StringLen::N(64))"
    )]
    pub id: String,
    #[sea_orm(unique, column_type = "String(StringLen::N(128))")]
    pub name: String,
    pub enabled: bool,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

// ─── Migration ───────────────────────────────────────────────────────────────
//
// NOTE: Table creation is intentionally NOT performed in this module. The
// `supersip_sub_accounts` table is created by
// `add_account_id_to_all_tables::Migration::up` (Phase 13 Plan 01a) so that
// the schema for the big-bang multi-tenancy foundation is captured in a
// single, idempotent, reversible migration.
