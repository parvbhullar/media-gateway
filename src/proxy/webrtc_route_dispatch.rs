//! Routing-side glue for `kind="webrtc"` trunks.
//!
//! The routing matcher (see `proxy/routing/matcher.rs`, Phase 7) detects
//! WebRTC trunks and returns [`crate::config::RouteResult::WebRtcBridge`]
//! instead of the usual `Forward`. The SIP-side caller — which has the
//! INVITE's SDP offer body — then invokes [`dispatch_webrtc_by_name`] to
//! drive the actual bridge construction via
//! [`crate::proxy::bridge::dispatch_webrtc`].
//!
//! This module exists outside `proxy/bridge/` (which is closed to edits in
//! PR 3) to keep PR 3's contract sealed while still providing the
//! convenience wrapper PR 4's call sites and integration tests need.

use anyhow::{Result, anyhow};
use rustrtc::IceServer;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::models::trunk;
use crate::proxy::bridge::dispatch_webrtc;
use crate::proxy::bridge::webrtc::DispatchOutcome;

/// Look up a `kind="webrtc"` trunk row by name and dispatch the bridge.
///
/// Returns the constructed [`DispatchOutcome`] — the SDP answer to put on
/// the SIP 200 OK, the `Arc<BridgePeer>`, and the adapter+session handle
/// for hangup teardown.
///
/// Errors if no trunk with that name exists, the trunk is inactive, or its
/// kind is not `"webrtc"`.
pub async fn dispatch_webrtc_by_name(
    db: &DatabaseConnection,
    trunk_name: &str,
    invite_offer_sdp: &str,
    global_ice_servers: Option<&[IceServer]>,
) -> Result<DispatchOutcome> {
    let row = fetch_webrtc_trunk(db, trunk_name).await?;
    dispatch_webrtc(&row, invite_offer_sdp, global_ice_servers).await
}

/// Fetch the row for a `kind="webrtc"` trunk, validating it's active and of
/// the right kind. Callers that need capacity limits, dispatch, and close-
/// context info should fetch the row once via this helper and reuse it,
/// rather than running three separate DB round-trips per INVITE.
pub async fn fetch_webrtc_trunk(
    db: &DatabaseConnection,
    trunk_name: &str,
) -> Result<trunk::Model> {
    let row = trunk::Entity::find()
        .filter(trunk::Column::Name.eq(trunk_name))
        .one(db)
        .await
        .map_err(|e| anyhow!("db error looking up trunk '{}': {}", trunk_name, e))?
        .ok_or_else(|| anyhow!("trunk '{}' not found", trunk_name))?;
    if !row.is_active {
        return Err(anyhow!("trunk '{}' is disabled", trunk_name));
    }
    if row.kind != "webrtc" {
        return Err(anyhow!(
            "trunk '{}' has kind '{}', expected 'webrtc'",
            trunk_name,
            row.kind
        ));
    }
    Ok(row)
}

/// Resolve the WebRTC trunk's signaling endpoint URL + auth header from the
/// DB. Used by the BYE-time teardown path to build a `SignalingContext`
/// matching the one used at `negotiate` time without re-running the full
/// dispatcher. Returns `(endpoint_url, auth_header)`.
///
/// Prefer stashing `DispatchOutcome.ctx` directly on the session — that
/// preserves the adapter `protocol` blob too, which this helper drops.
pub async fn lookup_webrtc_close_context(
    db: &DatabaseConnection,
    trunk_name: &str,
) -> Result<(String, Option<String>)> {
    let row = fetch_webrtc_trunk(db, trunk_name).await?;
    let cfg = row
        .webrtc()
        .map_err(|e| anyhow!("trunk '{}' webrtc() config parse failed: {}", trunk_name, e))?;
    Ok((cfg.endpoint_url, cfg.auth_header))
}

/// Extract `(trunk_id, max_concurrent, max_cps)` from a pre-loaded trunk row.
/// When both limits are `None`, the caller is expected to skip the gate
/// entirely.
pub fn capacity_limits_for(row: &trunk::Model) -> (i64, Option<u32>, Option<u32>) {
    let max_calls = row.max_concurrent.and_then(|v| u32::try_from(v).ok());
    let max_cps = row.max_cps.and_then(|v| u32::try_from(v).ok());
    (row.id, max_calls, max_cps)
}

/// Back-compat wrapper for the integration tests that fetched limits by
/// name directly. New code on the hot path should call
/// [`fetch_webrtc_trunk`] once and then [`capacity_limits_for`].
pub async fn lookup_webrtc_capacity_limits(
    db: &DatabaseConnection,
    trunk_name: &str,
) -> Result<(i64, Option<u32>, Option<u32>)> {
    let row = trunk::Entity::find()
        .filter(trunk::Column::Name.eq(trunk_name))
        .one(db)
        .await
        .map_err(|e| anyhow!("db error looking up trunk '{}': {}", trunk_name, e))?
        .ok_or_else(|| anyhow!("trunk '{}' not found", trunk_name))?;
    Ok(capacity_limits_for(&row))
}
