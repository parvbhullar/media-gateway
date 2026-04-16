//! Database-driven trunk_group to DestConfig resolver + distribution mode
//! translation + dispatch helper.
//!
//! Phase 2 Plan 02-03 (TRK-05). Translates a persisted trunk_group into a
//! `DestConfig::Multiple` + `select_method` + `hash_key` triple that the
//! existing `matcher::select_trunk` already understands.

use anyhow::{Result, anyhow};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use std::sync::Arc;

use crate::models::trunk_group::{
    Column as TgColumn, Entity as TgEntity, TrunkGroupDistributionMode,
};
use crate::models::trunk_group_member::{
    Column as TgmColumn, Entity as TgmEntity,
};
use crate::proxy::routing::DestConfig;

/// Errors from trunk_group resolution.
#[derive(Debug, thiserror::Error)]
pub enum TrunkGroupResolveError {
    #[error("trunk group '{0}' not found")]
    NotFound(String),
    #[error("trunk group '{0}' has no members")]
    NoMembers(String),
    #[error("parallel distribution requires the parallel-trunk-dial feature")]
    ParallelFeatureDisabled,
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

/// The output of resolving a trunk_group name into dispatch parameters.
#[derive(Debug)]
pub struct ResolvedTrunkGroup {
    pub dest_config: DestConfig,
    pub select_method: &'static str,
    pub hash_key: Option<String>,
}

/// Look up a trunk_group by name, fetch its members in position order,
/// and translate `distribution_mode` into the `(select_method, hash_key)`
/// pair that `matcher::select_trunk` expects.
pub async fn resolve_trunk_group_to_dest_config(
    db: &DatabaseConnection,
    group_name: &str,
) -> std::result::Result<ResolvedTrunkGroup, TrunkGroupResolveError> {
    let group = TgEntity::find()
        .filter(TgColumn::Name.eq(group_name))
        .one(db)
        .await?
        .ok_or_else(|| TrunkGroupResolveError::NotFound(group_name.to_string()))?;

    let members = TgmEntity::find()
        .filter(TgmColumn::TrunkGroupId.eq(group.id))
        .order_by_asc(TgmColumn::Position)
        .all(db)
        .await?;

    if members.is_empty() {
        return Err(TrunkGroupResolveError::NoMembers(
            group_name.to_string(),
        ));
    }

    let gateway_names: Vec<String> =
        members.into_iter().map(|m| m.gateway_name).collect();

    // NOTE: DefaultHasher is stable within a single process run;
    // Phase 2 tests rely on that. Cross-version stability is NOT
    // guaranteed and is explicitly out of scope.
    // TODO(phase-5+): hash_src_ip currently maps to "from.user" which
    //   is a SEMANTIC MISMATCH -- true src-IP affinity needs a new
    //   variant reading the peer socket address, not the From URI
    //   user-part. Tracked for Phase 5 or later.
    let (select_method, hash_key) = match group.distribution_mode {
        TrunkGroupDistributionMode::RoundRobin => ("rr", None),
        TrunkGroupDistributionMode::WeightBased => ("weighted", None),
        TrunkGroupDistributionMode::HashCallid => {
            ("hash", Some("call-id".to_string()))
        }
        TrunkGroupDistributionMode::HashSrcIp => {
            ("hash", Some("from.user".to_string()))
        }
        TrunkGroupDistributionMode::HashDestination => {
            ("hash", Some("to.user".to_string()))
        }
        TrunkGroupDistributionMode::Parallel => {
            #[cfg(not(feature = "parallel-trunk-dial"))]
            return Err(TrunkGroupResolveError::ParallelFeatureDisabled);
            #[cfg(feature = "parallel-trunk-dial")]
            return Err(TrunkGroupResolveError::Db(
                sea_orm::DbErr::Custom(
                    "parallel distribution not yet implemented".to_string(),
                ),
            ));
        }
    };

    Ok(ResolvedTrunkGroup {
        dest_config: DestConfig::Multiple(gateway_names),
        select_method,
        hash_key,
    })
}

/// High-level dispatch helper: resolve a trunk_group name then delegate
/// to the existing `matcher::select_trunk` to pick a single gateway.
pub async fn select_gateway_for_trunk_group(
    db: &DatabaseConnection,
    group_name: &str,
    option: &rsipstack::dialog::invitation::InviteOption,
    routing_state: Arc<crate::call::RoutingState>,
    trunks_config: Option<
        &std::collections::HashMap<String, crate::proxy::routing::TrunkConfig>,
    >,
) -> Result<String> {
    let resolved = resolve_trunk_group_to_dest_config(db, group_name)
        .await
        .map_err(|e| anyhow!("{}", e))?;
    crate::proxy::routing::matcher::select_trunk(
        &resolved.dest_config,
        resolved.select_method,
        &resolved.hash_key,
        option,
        routing_state,
        trunks_config,
    )
}
