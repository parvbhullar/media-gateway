//! `/api/v1/trunks/{name}/media` — TSUB-03 full implementation.
//!
//! Phase 3 Plan 03-04. Backed by the `rustpbx_trunk_groups.media_config`
//! Json column (Plan 03-01 schema). GET returns defaults `{codecs:[],
//! dtmf_mode:null, srtp:null, media_mode:null}` when column is NULL per
//! D-11 — never 404 for the media sub-resource of an existing trunk_group.
//! PUT replaces atomically and stores `Some(json)` even when every enum
//! field is null (D-11 — keeps the schema observable). Validation per
//! D-12; codec wire format lowercase per D-10. Phase 5 will enforce these
//! in the proxy hot path (488 on codec mismatch).

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::trunk_group::{
    self, Column as TrunkGroupColumn, Entity as TrunkGroupEntity,
};

// ── Wire type (D-09) ─────────────────────────────────────────────────────

/// TrunkMediaConfig — canonical wire shape per D-09.
///
/// Same struct serves GET responses, PUT request bodies, and PUT response
/// echo. `codecs` defaults to `[]`; the three enum fields default to
/// `None`. `deny_unknown_fields` catches operator typos early (e.g.
/// `dtmf-mode` vs `dtmf_mode`).
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrunkMediaConfig {
    #[serde(default)]
    pub codecs: Vec<String>,
    #[serde(default)]
    pub dtmf_mode: Option<String>,
    #[serde(default)]
    pub srtp: Option<String>,
    #[serde(default)]
    pub media_mode: Option<String>,
}

impl TrunkMediaConfig {
    /// D-11 default shape: what GET returns when the `media_config` column
    /// is NULL. The empty codec list and three nulls are the canonical
    /// "no config" state — distinguishable from a PUT-stored all-null by
    /// reading the raw DB column (stored = `Some(json)`), but identical at
    /// the wire layer.
    fn defaults() -> Self {
        Self {
            codecs: vec![],
            dtmf_mode: None,
            srtp: None,
            media_mode: None,
        }
    }
}

// ── Validation (D-10, D-12) ──────────────────────────────────────────────

/// D-10: wire format is lowercase (`"pcmu"`, `"pcma"`, `"opus"`). Reject
/// uppercase letters to prevent operator confusion and keep the Phase-5
/// enforcement layer's case-folding simple (it translates to rsipstack's
/// uppercase RFC 3551 form on the hot path).
fn validate_codec(codec: &str) -> ApiResult<()> {
    let trimmed = codec.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("codec name must be non-empty"));
    }
    if trimmed != codec {
        return Err(ApiError::bad_request(
            "codec name must not have leading/trailing whitespace",
        ));
    }
    if codec.chars().any(|c| c.is_uppercase()) {
        return Err(ApiError::bad_request(format!(
            "codec '{}' must be lowercase (D-10 wire format)",
            codec
        )));
    }
    Ok(())
}

/// D-12: dtmf_mode ∈ {rfc2833, info, inband} or null.
fn validate_dtmf_mode(value: &Option<String>) -> ApiResult<()> {
    if let Some(v) = value {
        match v.as_str() {
            "rfc2833" | "info" | "inband" => Ok(()),
            other => Err(ApiError::bad_request(format!(
                "invalid dtmf_mode '{}': expected rfc2833 | info | inband",
                other
            ))),
        }
    } else {
        Ok(())
    }
}

/// D-12: srtp ∈ {srtp, srtp_optional} or null.
fn validate_srtp(value: &Option<String>) -> ApiResult<()> {
    if let Some(v) = value {
        match v.as_str() {
            "srtp" | "srtp_optional" => Ok(()),
            other => Err(ApiError::bad_request(format!(
                "invalid srtp '{}': expected srtp | srtp_optional",
                other
            ))),
        }
    } else {
        Ok(())
    }
}

/// D-12: media_mode ∈ {relay, transcode} or null.
fn validate_media_mode(value: &Option<String>) -> ApiResult<()> {
    if let Some(v) = value {
        match v.as_str() {
            "relay" | "transcode" => Ok(()),
            other => Err(ApiError::bad_request(format!(
                "invalid media_mode '{}': expected relay | transcode",
                other
            ))),
        }
    } else {
        Ok(())
    }
}

fn validate_media_config(cfg: &TrunkMediaConfig) -> ApiResult<()> {
    for c in &cfg.codecs {
        validate_codec(c)?;
    }
    validate_dtmf_mode(&cfg.dtmf_mode)?;
    validate_srtp(&cfg.srtp)?;
    validate_media_mode(&cfg.media_mode)?;
    Ok(())
}

// ── Router ───────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/trunks/{name}/media", get(get_media).put(put_media))
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Resolve `{name}` to a `trunk_group::Model`. Returns 404 if missing,
/// consistent with Plans 03-02 / 03-03. Returns the full Model (not just
/// id) because PUT needs it as the source for `ActiveModel::from(model)`
/// to preserve non-updated columns.
async fn load_trunk_group(
    db: &sea_orm::DatabaseConnection,
    name: &str,
) -> ApiResult<trunk_group::Model> {
    TrunkGroupEntity::find()
        .filter(TrunkGroupColumn::Name.eq(name))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("trunk group '{}' not found", name))
        })
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// GET /trunks/{name}/media — read the `media_config` column. If NULL,
/// return defaults per D-11 (never 404 for the media sub-resource of an
/// existing trunk_group). If populated, deserialize and return.
async fn get_media(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<TrunkMediaConfig>> {
    let db = state.db();
    let group = load_trunk_group(db, &name).await?;

    let cfg = match group.media_config {
        None => TrunkMediaConfig::defaults(),
        Some(json) => serde_json::from_value::<TrunkMediaConfig>(json)
            .map_err(|e| {
                ApiError::internal(format!(
                    "stored media_config is malformed: {}",
                    e
                ))
            })?,
    };
    Ok(Json(cfg))
}

/// PUT /trunks/{name}/media — replace the entire `media_config` column
/// atomically. Stores `Some(json)` even when every enum field is null
/// (D-11) so subsequent GETs reflect operator intent instead of the NULL
/// fallback. 404 if the parent trunk group is missing; 400 on codec or
/// enum validation failure.
async fn put_media(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(cfg): Json<TrunkMediaConfig>,
) -> ApiResult<Json<TrunkMediaConfig>> {
    let db = state.db();
    validate_media_config(&cfg)?;
    let group = load_trunk_group(db, &name).await?;

    let stored_json = serde_json::to_value(&cfg)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let mut am: trunk_group::ActiveModel = group.into();
    am.media_config = Set(Some(stored_json));
    am.updated_at = Set(Utc::now());
    am.update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(cfg))
}
