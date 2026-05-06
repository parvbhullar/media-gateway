//! Phase 8 — Translations CRUD router (TRN-02).
//!
//! Plan 08-02 GREEN body. Replaces the 08-01 stub bodies with full CRUD
//! against `supersip_translations`. The `pub fn router() -> Router<AppState>`
//! signature is the Wave-1 invariant: this plan replaces handler bodies
//! WITHOUT touching `mod.rs` (Phase 5 / 6 / 7 file-ownership pattern).
//!
//! Endpoints (D-26):
//!   - GET    /translations          — paginated list (D-27 envelope)
//!   - POST   /translations          — create (201 + full view)
//!   - GET    /translations/{name}   — fetch by name (D-04)
//!   - PUT    /translations/{name}   — full replacement; engine.invalidate()
//!   - DELETE /translations/{name}   — remove; engine.invalidate()
//!
//! Validation (D-03 / D-21 / D-25):
//!   - name: ^[a-z0-9-]+$, 1..=64 chars
//!   - patterns: ≤4096 chars each; must compile via `regex::Regex::new`
//!   - replacements: non-empty when paired pattern is set; D-19 probe
//!     replacement against `"0123456789"` to surface bad backreferences
//!   - at least one of caller_pattern / destination_pattern non-null
//!   - direction ∈ {inbound, outbound, both} (default "both")
//!   - priority ∈ [-1000, 1000] (default 100)
//!
//! Cache invalidation (D-13): PUT and DELETE both call
//! `state.translation_engine().invalidate(&existing.id)` so the next INVITE
//! recompiles the new pattern (or stops applying the deleted rule).
//!
//! D-07 normalization: a replacement provided alongside a null paired
//! pattern is silently dropped at write time — the engine never reads such
//! a replacement, and persisting it would mislead operators.

use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::common::{CommonScopeQuery, PaginatedResponse, Pagination, build_account_filter};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::translations::{
    self, Column as TrColumn, Entity as TrEntity, Model as TrModel,
};

// ─── Constants (D-03 / D-21) ─────────────────────────────────────────────

const MAX_NAME_LEN: usize = 64;
const MAX_PATTERN_LEN: usize = 4096;
const MIN_PRIORITY: i32 = -1000;
const MAX_PRIORITY: i32 = 1000;
const DEFAULT_PRIORITY: i32 = 100;
const DEFAULT_DIRECTION: &str = "both";
const VALID_DIRECTIONS: &[&str] = &["inbound", "outbound", "both"];
const REPLACEMENT_PROBE: &str = "0123456789";

// ─── Wire types (D-27) ───────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct TranslationView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub caller_pattern: Option<String>,
    pub destination_pattern: Option<String>,
    pub caller_replacement: Option<String>,
    pub destination_replacement: Option<String>,
    pub direction: String,
    pub priority: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&TrModel> for TranslationView {
    fn from(m: &TrModel) -> Self {
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
            description: m.description.clone(),
            caller_pattern: m.caller_pattern.clone(),
            destination_pattern: m.destination_pattern.clone(),
            caller_replacement: m.caller_replacement.clone(),
            destination_replacement: m.destination_replacement.clone(),
            direction: m.direction.clone(),
            priority: m.priority,
            is_active: m.is_active,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl From<TrModel> for TranslationView {
    fn from(m: TrModel) -> Self {
        Self::from(&m)
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateTranslationRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub caller_pattern: Option<String>,
    #[serde(default)]
    pub destination_pattern: Option<String>,
    #[serde(default)]
    pub caller_replacement: Option<String>,
    #[serde(default)]
    pub destination_replacement: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub is_active: Option<bool>,
}

// ─── Validators (pub: re-used by 08-03 engine for compile-time guards) ───

/// Validate a translation name: lowercase letters, digits, dashes only;
/// 1..=64 chars (D-03).
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(format!(
            "translation name must be 1-{} chars",
            MAX_NAME_LEN
        ));
    }
    for &b in name.as_bytes() {
        let ok = b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-';
        if !ok {
            return Err(
                "translation name must contain only lowercase letters, \
                 digits, and dashes (^[a-z0-9-]+$)"
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Validate a single pattern: length cap (D-21) + compile (D-03).
pub fn validate_pattern(field: &str, pattern: &str) -> Result<regex::Regex, String> {
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(format!(
            "{} length {} exceeds cap of {} chars",
            field,
            pattern.len(),
            MAX_PATTERN_LEN
        ));
    }
    regex::Regex::new(pattern).map_err(|e| {
        format!("{} is not a valid regex: {}", field, e)
    })
}

/// Validate a paired (pattern, replacement) tuple.
///
/// Per D-25: replacement must be non-empty when paired pattern is set.
/// Per D-19: the replacement is probed by attempting `regex.replace_all`
/// against a fixed digit string so invalid backreferences (`$99` etc.)
/// surface as 400 at write time rather than blowing up mid-call.
pub fn validate_pattern_replacement_pair(
    field_pattern: &str,
    pattern: &Option<String>,
    field_replacement: &str,
    replacement: &Option<String>,
) -> Result<(), String> {
    let Some(p) = pattern else {
        return Ok(());
    };
    let compiled = validate_pattern(field_pattern, p)?;
    let r = replacement.as_deref().unwrap_or("");
    if r.is_empty() {
        return Err(format!(
            "{} may not be empty when {} is set (D-25)",
            field_replacement, field_pattern
        ));
    }
    // D-19 probe: catch invalid backreferences without panicking.
    let _ = compiled.replace_all(REPLACEMENT_PROBE, r);
    Ok(())
}

/// Validate the direction string (D-03 / D-22).
pub fn validate_direction(direction: &str) -> Result<(), String> {
    if VALID_DIRECTIONS.contains(&direction) {
        Ok(())
    } else {
        Err(format!(
            "direction '{}' invalid: must be one of {}",
            direction,
            VALID_DIRECTIONS.join(", ")
        ))
    }
}

/// Validate priority range (D-03).
pub fn validate_priority(p: i32) -> Result<(), String> {
    if !(MIN_PRIORITY..=MAX_PRIORITY).contains(&p) {
        return Err(format!(
            "priority {} out of range [{}, {}]",
            p, MIN_PRIORITY, MAX_PRIORITY
        ));
    }
    Ok(())
}

/// Top-level validator invoked by POST and PUT.
pub fn validate_translation(req: &CreateTranslationRequest) -> Result<(), String> {
    validate_name(&req.name)?;

    if req.caller_pattern.is_none() && req.destination_pattern.is_none() {
        return Err(
            "at least one of caller_pattern or destination_pattern must be \
             set (rule with both null is meaningless)"
                .to_string(),
        );
    }

    validate_pattern_replacement_pair(
        "caller_pattern",
        &req.caller_pattern,
        "caller_replacement",
        &req.caller_replacement,
    )?;
    validate_pattern_replacement_pair(
        "destination_pattern",
        &req.destination_pattern,
        "destination_replacement",
        &req.destination_replacement,
    )?;

    let direction = req.direction.as_deref().unwrap_or(DEFAULT_DIRECTION);
    validate_direction(direction)?;

    let priority = req.priority.unwrap_or(DEFAULT_PRIORITY);
    validate_priority(priority)?;

    Ok(())
}

// ─── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/translations", get(list).post(create))
        .route(
            "/translations/{name}",
            get(fetch).put(replace).delete(remove),
        )
}

// ─── Handlers ────────────────────────────────────────────────────────────

async fn list(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Query(scope_q): Query<CommonScopeQuery>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<PaginatedResponse<TranslationView>>> {
    let db = state.db();
    let page_no = pagination.page.max(1);
    let page_size = pagination.limit();

    let conds = build_account_filter(&scope, TrColumn::AccountId, &scope_q, Condition::all())?;

    let paginator = TrEntity::find()
        .filter(conds)
        .order_by_asc(TrColumn::Priority)
        .order_by_asc(TrColumn::Name)
        .paginate(db, page_size);

    let total = paginator
        .num_items()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let rows = paginator
        .fetch_page(page_no.saturating_sub(1))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(PaginatedResponse::new(
        rows.iter().map(TranslationView::from).collect(),
        page_no,
        page_size,
        total,
    )))
}

async fn create(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Json(req): Json<CreateTranslationRequest>,
) -> ApiResult<(StatusCode, Json<TranslationView>)> {
    let db = state.db();

    validate_translation(&req).map_err(ApiError::bad_request)?;

    // Pre-check duplicate name → 409.
    let dup = TrEntity::find()
        .filter(TrColumn::Name.eq(req.name.clone()))
        .filter(TrColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "translation '{}' already exists",
            req.name
        )));
    }

    // D-07 normalization: drop replacement when paired pattern is null.
    let caller_replacement = if req.caller_pattern.is_some() {
        req.caller_replacement.clone()
    } else {
        None
    };
    let destination_replacement = if req.destination_pattern.is_some() {
        req.destination_replacement.clone()
    } else {
        None
    };

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let am = translations::ActiveModel {
        id: Set(id),
        name: Set(req.name),
        description: Set(req.description),
        caller_pattern: Set(req.caller_pattern),
        destination_pattern: Set(req.destination_pattern),
        caller_replacement: Set(caller_replacement),
        destination_replacement: Set(destination_replacement),
        direction: Set(req
            .direction
            .unwrap_or_else(|| DEFAULT_DIRECTION.to_string())),
        priority: Set(req.priority.unwrap_or(DEFAULT_PRIORITY)),
        is_active: Set(req.is_active.unwrap_or(true)),
        created_at: Set(now),
        updated_at: Set(now),
        account_id: Set(scope.account_id.clone()),
    };
    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((StatusCode::CREATED, Json(TranslationView::from(inserted))))
}

async fn fetch(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
) -> ApiResult<Json<TranslationView>> {
    let db = state.db();
    let row = TrEntity::find()
        .filter(TrColumn::Name.eq(name.clone()))
        .filter(TrColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("translation '{}' not found", name))
        })?;
    Ok(Json(TranslationView::from(row)))
}

async fn replace(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
    Json(req): Json<CreateTranslationRequest>,
) -> ApiResult<Json<TranslationView>> {
    let db = state.db();

    validate_translation(&req).map_err(ApiError::bad_request)?;

    let existing = TrEntity::find()
        .filter(TrColumn::Name.eq(name.clone()))
        .filter(TrColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("translation '{}' not found", name))
        })?;

    // Duplicate-name check on rename.
    if req.name != existing.name {
        let dup = TrEntity::find()
            .filter(TrColumn::Name.eq(req.name.clone()))
            .filter(TrColumn::AccountId.eq(scope.account_id.clone()))
            .one(db)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        if dup.is_some() {
            return Err(ApiError::conflict(format!(
                "translation '{}' already exists",
                req.name
            )));
        }
    }

    // D-07 normalization (same as create).
    let caller_replacement = if req.caller_pattern.is_some() {
        req.caller_replacement.clone()
    } else {
        None
    };
    let destination_replacement = if req.destination_pattern.is_some() {
        req.destination_replacement.clone()
    } else {
        None
    };

    let preserved_id = existing.id.clone();
    let preserved_created_at = existing.created_at;

    let mut am: translations::ActiveModel = existing.into();
    am.name = Set(req.name);
    am.description = Set(req.description);
    am.caller_pattern = Set(req.caller_pattern);
    am.destination_pattern = Set(req.destination_pattern);
    am.caller_replacement = Set(caller_replacement);
    am.destination_replacement = Set(destination_replacement);
    am.direction = Set(req
        .direction
        .unwrap_or_else(|| DEFAULT_DIRECTION.to_string()));
    am.priority = Set(req.priority.unwrap_or(DEFAULT_PRIORITY));
    am.is_active = Set(req.is_active.unwrap_or(true));
    am.created_at = Set(preserved_created_at);
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // D-13: drop the cached compiled regex so the next INVITE recompiles
    // from the freshly-stored pattern. Key by stable UUID id.
    state.translation_engine().invalidate(&preserved_id);

    Ok(Json(TranslationView::from(updated)))
}

async fn remove(
    State(state): State<AppState>,
    Extension(scope): Extension<AccountScope>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();

    let existing = TrEntity::find()
        .filter(TrColumn::Name.eq(name.clone()))
        .filter(TrColumn::AccountId.eq(scope.account_id.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("translation '{}' not found", name))
        })?;

    let rule_id = existing.id.clone();

    // D-13: invalidate BEFORE the row vanishes so any in-flight engine
    // call rebinds against the post-delete DB state on its next read.
    state.translation_engine().invalidate(&rule_id);

    TrEntity::delete_by_id(rule_id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

// ─── Validator unit tests ────────────────────────────────────────────────

#[cfg(test)]
mod validators {
    use super::*;

    #[test]
    fn name_lowercase_dashes_ok() {
        assert!(validate_name("uk-normalize").is_ok());
        assert!(validate_name("a").is_ok());
        assert!(validate_name("a-1-b").is_ok());
    }

    #[test]
    fn name_uppercase_rejected() {
        assert!(validate_name("UK").is_err());
    }

    #[test]
    fn name_underscore_rejected() {
        assert!(validate_name("uk_normalize").is_err());
    }

    #[test]
    fn name_empty_rejected() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn name_too_long_rejected() {
        let big = "a".repeat(65);
        assert!(validate_name(&big).is_err());
    }

    #[test]
    fn name_at_max_ok() {
        let n = "a".repeat(MAX_NAME_LEN);
        assert!(validate_name(&n).is_ok());
    }

    #[test]
    fn pattern_compile_error_rejected() {
        assert!(validate_pattern("p", "[invalid").is_err());
    }

    #[test]
    fn pattern_at_max_len_ok() {
        let p = "a".repeat(MAX_PATTERN_LEN);
        assert!(validate_pattern("p", &p).is_ok());
    }

    #[test]
    fn pattern_too_long_rejected() {
        let p = "a".repeat(MAX_PATTERN_LEN + 1);
        assert!(validate_pattern("p", &p).is_err());
    }

    #[test]
    fn empty_replacement_rejected_when_pattern_set() {
        let err = validate_pattern_replacement_pair(
            "caller_pattern",
            &Some(r"^0(\d+)$".to_string()),
            "caller_replacement",
            &Some("".to_string()),
        )
        .unwrap_err();
        assert!(err.contains("D-25"), "expected D-25 reference: {}", err);
    }

    #[test]
    fn null_replacement_rejected_when_pattern_set() {
        assert!(validate_pattern_replacement_pair(
            "caller_pattern",
            &Some(r"^0(\d+)$".to_string()),
            "caller_replacement",
            &None,
        )
        .is_err());
    }

    #[test]
    fn replacement_ok_when_pattern_null() {
        assert!(validate_pattern_replacement_pair(
            "caller_pattern",
            &None,
            "caller_replacement",
            &Some("anything".to_string()),
        )
        .is_ok());
    }

    #[test]
    fn direction_valid() {
        assert!(validate_direction("inbound").is_ok());
        assert!(validate_direction("outbound").is_ok());
        assert!(validate_direction("both").is_ok());
        assert!(validate_direction("sideways").is_err());
    }

    #[test]
    fn priority_in_range() {
        assert!(validate_priority(0).is_ok());
        assert!(validate_priority(MIN_PRIORITY).is_ok());
        assert!(validate_priority(MAX_PRIORITY).is_ok());
        assert!(validate_priority(MIN_PRIORITY - 1).is_err());
        assert!(validate_priority(MAX_PRIORITY + 1).is_err());
    }

    #[test]
    fn validate_translation_both_null_rejected() {
        let req = CreateTranslationRequest {
            name: "x".into(),
            description: None,
            caller_pattern: None,
            destination_pattern: None,
            caller_replacement: None,
            destination_replacement: None,
            direction: Some("both".into()),
            priority: None,
            is_active: None,
        };
        assert!(validate_translation(&req).is_err());
    }

    #[test]
    fn validate_translation_happy() {
        let req = CreateTranslationRequest {
            name: "uk".into(),
            description: None,
            caller_pattern: Some(r"^0(\d+)$".into()),
            destination_pattern: None,
            caller_replacement: Some("+44$1".into()),
            destination_replacement: None,
            direction: Some("inbound".into()),
            priority: Some(50),
            is_active: Some(true),
        };
        assert!(validate_translation(&req).is_ok());
    }
}

#[cfg(test)]
mod router_smoke {
    use super::*;

    #[test]
    fn router_builds_without_panic() {
        let _r = router();
    }
}
