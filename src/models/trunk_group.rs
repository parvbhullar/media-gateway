//! `rustpbx_trunk_groups` — logical bundle of one or more gateways with a
//! distribution policy, credentials, ACL, and failover metadata.
//!
//! Phase 2 Plan 02-01 ships this entity alongside
//! [`crate::models::trunk_group_member`]. Both are additive — zero
//! modification to the legacy sip-trunks table (TRK-01 / MIG-01).
//!
//! `direction` reuses [`super::sip_trunk::SipTrunkDirection`]: the column
//! stores the same string values (`inbound` / `outbound` / `bidirectional`)
//! so one enum type is sufficient.

use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{boolean, json_null, string, string_null, timestamp};
use sea_orm_migration::sea_query::ColumnDef;
use sea_query::Expr;
use serde::{Deserialize, Serialize};

// Re-export so handlers and resolvers can `use crate::models::trunk_group::SipTrunkDirection`
// without also having to reach into `sip_trunk`.
pub use super::sip_trunk::SipTrunkDirection;

/// Distribution mode for selecting a member gateway inside a trunk group.
///
/// Phase 2 supports the five ready modes (round_robin, weight_based,
/// hash_callid, hash_src_ip, hash_destination) plus a feature-gated
/// `parallel` mode that is reserved for v2.1+.
#[derive(Copy, Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum TrunkGroupDistributionMode {
    #[sea_orm(string_value = "round_robin")]
    RoundRobin,
    #[sea_orm(string_value = "weight_based")]
    WeightBased,
    #[sea_orm(string_value = "hash_callid")]
    HashCallid,
    #[sea_orm(string_value = "hash_src_ip")]
    HashSrcIp,
    #[sea_orm(string_value = "hash_destination")]
    HashDestination,
    #[sea_orm(string_value = "parallel")]
    Parallel,
}

impl TrunkGroupDistributionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoundRobin => "round_robin",
            Self::WeightBased => "weight_based",
            Self::HashCallid => "hash_callid",
            Self::HashSrcIp => "hash_src_ip",
            Self::HashDestination => "hash_destination",
            Self::Parallel => "parallel",
        }
    }
}

impl Default for TrunkGroupDistributionMode {
    fn default() -> Self {
        Self::RoundRobin
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "rustpbx_trunk_groups")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    #[sea_orm(unique)]
    pub name: String,
    pub display_name: Option<String>,
    pub direction: SipTrunkDirection,
    pub distribution_mode: TrunkGroupDistributionMode,
    pub nofailover_sip_codes: Option<Json>,
    // Phase 3 Plan 03-01 (TSUB-03): media configuration JSON blob.
    // Additive per D-09. Shape:
    //   {codecs: ["pcmu","pcma"], dtmf_mode, srtp, media_mode}
    pub media_config: Option<Json>,
    pub is_active: bool,
    pub metadata: Option<Json>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    /// Phase 13 Plan 01a (TEN-01) — owning sub-account; defaults to 'root'.
    #[sea_orm(default_value = "root")]
    pub account_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

// ─── Migration ───────────────────────────────────────────────────────────────

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
                            .primary_key()
                            .auto_increment(),
                    )
                    .col(string(Column::Name).char_len(120))
                    .col(string_null(Column::DisplayName).char_len(160))
                    .col(
                        string(Column::Direction)
                            .char_len(32)
                            .default(SipTrunkDirection::default().as_str()),
                    )
                    .col(
                        string(Column::DistributionMode)
                            .char_len(32)
                            .default(TrunkGroupDistributionMode::default().as_str()),
                    )
                    .col(json_null(Column::NofailoverSipCodes))
                    // Phase 3 Plan 03-01 (TSUB-03, D-09): media config JSON.
                    // Added to fresh-DB CREATE so new installs don't need
                    // the add_media_config_column alter (which is the
                    // upgrade path for Phase-2-era DBs and is idempotent).
                    .col(json_null(Column::MediaConfig))
                    .col(boolean(Column::IsActive).default(true))
                    .col(json_null(Column::Metadata))
                    .col(timestamp(Column::CreatedAt).default(Expr::current_timestamp()))
                    .col(timestamp(Column::UpdatedAt).default(Expr::current_timestamp()))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_trunk_groups_name")
                    .table(Entity)
                    .col(Column::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_trunk_groups_direction_active")
                    .table(Entity)
                    .col(Column::Direction)
                    .col(Column::IsActive)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Entity).to_owned())
            .await
    }
}
