//! DEPRECATED: re-export shim for the unified `trunk` model.
//!
//! Wave 1 of the rustpbx-native WebRTC bridge unified `sip_trunk` and a new
//! `webrtc` kind into a single `trunk` model with a `kind_config` JSON
//! column. The columns formerly exposed as typed fields on `Model`
//! (`sip_server`, `auth_username`, ...) now live inside `kind_config` and
//! are accessed via `Model::sip()? -> SipTrunkConfig`.
//!
//! This shim re-exports the renamed `trunk::Entity` / `ActiveModel` /
//! `Column` / `Relation` / `Model` and aliases the renamed enums.
//! Existing call sites that do field access on the dropped columns
//! (`row.sip_server`, etc.) WILL fail to compile under this shim; that is
//! intentional — Wave 2 migrates each call site to
//! `Model::sip()?.sip_server` and similar.
//!
//! The legacy CREATE TABLE migration remains in this module so its
//! `DeriveMigrationName`-derived tracking name (`m_..._sip_trunk`) is
//! preserved for already-deployed DBs. Its column references are scoped
//! to the private `legacy` submodule so they don't collide with the
//! re-exported new-model `Column`.
//!
//! Will be removed in a follow-up wave once all importers migrate.

use sea_orm_migration::prelude::*;

// ---- Public shim: forward new-model types under the old module path. ----

pub use super::trunk::{
    ActiveModel, Column, Entity, Model, PrimaryKey, Relation, SipTransport, SipTrunkConfig,
    WebRtcTrunkConfig,
};

/// Alias retained for back-compat. New code should use `trunk::TrunkStatus`.
pub type SipTrunkStatus = super::trunk::TrunkStatus;

/// Alias retained for back-compat. New code should use `trunk::TrunkDirection`.
pub type SipTrunkDirection = super::trunk::TrunkDirection;

// ---- Legacy CREATE TABLE migration. ----

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm_migration::schema::{
            boolean, double_null, integer_null, json_null, string, string_null, text_null,
            timestamp,
        };
        use sea_query::Expr;

        manager
            .create_table(
                Table::create()
                    .table(legacy::Entity)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(legacy::Column::Id)
                            .big_integer()
                            .primary_key()
                            .auto_increment(),
                    )
                    .col(string(legacy::Column::Name).char_len(120))
                    .col(string_null(legacy::Column::DisplayName).char_len(160))
                    .col(string_null(legacy::Column::Carrier).char_len(160))
                    .col(text_null(legacy::Column::Description))
                    .col(
                        string(legacy::Column::Status)
                            .char_len(32)
                            .default("healthy"),
                    )
                    .col(
                        string(legacy::Column::Direction)
                            .char_len(32)
                            .default("bidirectional"),
                    )
                    .col(string_null(legacy::Column::SipServer).char_len(160))
                    .col(
                        string(legacy::Column::SipTransport)
                            .char_len(16)
                            .default("udp"),
                    )
                    .col(string_null(legacy::Column::OutboundProxy).char_len(160))
                    .col(string_null(legacy::Column::AuthUsername).char_len(160))
                    .col(string_null(legacy::Column::AuthPassword).char_len(160))
                    .col(string_null(legacy::Column::DefaultRouteLabel).char_len(160))
                    .col(integer_null(legacy::Column::MaxCps))
                    .col(integer_null(legacy::Column::MaxConcurrent))
                    .col(integer_null(legacy::Column::MaxCallDuration))
                    .col(double_null(legacy::Column::UtilisationPercent))
                    .col(double_null(legacy::Column::WarningThresholdPercent))
                    .col(json_null(legacy::Column::AllowedIps))
                    .col(json_null(legacy::Column::DidNumbers))
                    .col(json_null(legacy::Column::BillingSnapshot))
                    .col(json_null(legacy::Column::Analytics))
                    .col(json_null(legacy::Column::Tags))
                    .col(string_null(legacy::Column::IncomingFromUserPrefix).char_len(160))
                    .col(string_null(legacy::Column::IncomingToUserPrefix).char_len(160))
                    .col(boolean(legacy::Column::IsActive).default(true))
                    .col(json_null(legacy::Column::Metadata))
                    .col(timestamp(legacy::Column::CreatedAt).default(Expr::current_timestamp()))
                    .col(timestamp(legacy::Column::UpdatedAt).default(Expr::current_timestamp()))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_sip_trunks_name")
                    .table(legacy::Entity)
                    .col(legacy::Column::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_sip_trunks_status")
                    .table(legacy::Entity)
                    .col(legacy::Column::Status)
                    .col(legacy::Column::IsActive)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_sip_trunks_direction")
                    .table(legacy::Entity)
                    .col(legacy::Column::Direction)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(legacy::Entity).to_owned())
            .await
    }
}

/// Pre-rename legacy Entity. Scoped to a submodule so its `Column` enum
/// does not collide with the publicly re-exported new `Column`. Used by:
///   - This file's `Migration` (CREATE TABLE).
///   - Sibling `add_sip_trunk_*` migrations that ALTER the legacy table.
pub mod legacy {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Copy, Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    #[sea_orm(rs_type = "String", db_type = "Text")]
    #[derive(Default)]
    pub enum LegacyStatus {
        #[sea_orm(string_value = "healthy")]
        #[default]
        Healthy,
    }

    #[derive(Copy, Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    #[sea_orm(rs_type = "String", db_type = "Text")]
    #[derive(Default)]
    pub enum LegacyDirection {
        #[sea_orm(string_value = "bidirectional")]
        #[default]
        Bidirectional,
    }

    #[derive(Copy, Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    #[sea_orm(rs_type = "String", db_type = "Text")]
    #[derive(Default)]
    pub enum LegacyTransport {
        #[sea_orm(string_value = "udp")]
        #[default]
        Udp,
    }

    /// Pre-Wave-1 schema. Columns added by later `add_sip_trunk_*` migrations
    /// are appended here so those migrations can reference them via
    /// `legacy::Column::*`.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize, Default)]
    #[sea_orm(table_name = "rustpbx_sip_trunks")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = true)]
        pub id: i64,
        #[sea_orm(unique)]
        pub name: String,
        pub display_name: Option<String>,
        pub carrier: Option<String>,
        pub description: Option<String>,
        pub status: LegacyStatus,
        pub direction: LegacyDirection,
        pub sip_server: Option<String>,
        pub sip_transport: LegacyTransport,
        pub outbound_proxy: Option<String>,
        pub auth_username: Option<String>,
        pub auth_password: Option<String>,
        pub default_route_label: Option<String>,
        pub max_cps: Option<i32>,
        pub max_concurrent: Option<i32>,
        pub max_call_duration: Option<i32>,
        pub utilisation_percent: Option<f64>,
        pub warning_threshold_percent: Option<f64>,
        pub allowed_ips: Option<Json>,
        pub did_numbers: Option<Json>,
        pub billing_snapshot: Option<Json>,
        pub analytics: Option<Json>,
        pub tags: Option<Json>,
        pub incoming_from_user_prefix: Option<String>,
        pub incoming_to_user_prefix: Option<String>,
        pub is_active: bool,
        pub metadata: Option<Json>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,

        // Columns added by `add_sip_trunk_register_columns`:
        pub register_enabled: bool,
        pub register_expires: Option<i32>,
        pub register_extra_headers: Option<Json>,

        // Column added by `add_sip_trunk_rewrite_hostport`:
        pub rewrite_hostport: bool,

        // Columns added by `add_sip_trunk_health_columns`:
        pub last_health_check_at: Option<DateTimeUtc>,
        pub health_check_interval_secs: Option<i32>,
        pub failure_threshold: Option<i32>,
        pub recovery_threshold: Option<i32>,
        pub consecutive_failures: i32,
        pub consecutive_successes: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
