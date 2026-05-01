//! Phase 9 — Manipulations CRUD router (MAN-02).
//!
//! Plan 09-02 GREEN body. Replaces the 09-01 stub bodies with full CRUD
//! against `supersip_manipulations`. The `pub fn router() -> Router<AppState>`
//! signature is the Wave-1 invariant: this plan replaces handler bodies
//! WITHOUT touching `mod.rs` (Phase 5/6/7/8 file-ownership pattern).
//!
//! Endpoints (D-33):
//!   - GET    /manipulations          — paginated list
//!   - POST   /manipulations          — create (201 + full view)
//!   - GET    /manipulations/{name}   — fetch by name
//!   - PUT    /manipulations/{name}   — full replacement; engine.invalidate_class
//!   - DELETE /manipulations/{name}   — remove; engine.invalidate_class
//!
//! Validation — D-34 14-step pipeline at POST and PUT.
//!
//! Cache invalidation (D-33): PUT and DELETE both call
//! `state.manipulation_engine().invalidate_class(&class_id)` AFTER the DB
//! write succeeds.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app::AppState;
use crate::handler::api_v1::common::{PaginatedResponse, Pagination};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::manipulations::{
    self, Column as ManColumn, Entity as ManEntity, Model as ManModel,
};
use crate::proxy::manipulation::{Action, ConditionOp, Rule};

// ─── Constants (D-31, D-03) ──────────────────────────────────────────────────

/// SIP headers that operators may NOT mutate via set_header or remove_header.
/// Case-insensitive equality check at validation time (D-31).
pub const FORBIDDEN_HEADERS: &[&str] = &[
    "via",
    "from",
    "to",
    "call-id",
    "cseq",
    "contact",
    "max-forwards",
    "content-length",
    "content-type",
];

const MAX_NAME_LEN: usize = 64;
const MAX_REGEX_LEN: usize = 4096;
const MIN_PRIORITY: i32 = -1000;
const MAX_PRIORITY: i32 = 1000;
const DEFAULT_PRIORITY: i32 = 100;
const DEFAULT_DIRECTION: &str = "both";
const VALID_DIRECTIONS: &[&str] = &["inbound", "outbound", "both"];

// ─── Valid sources (D-07) ─────────────────────────────────────────────────────

fn is_valid_source(source: &str) -> bool {
    matches!(
        source,
        "caller_number" | "destination_number" | "trunk"
    ) || source.starts_with("header:")
        || source.starts_with("var:")
}

// ─── Wire types ───────────────────────────────────────────────────────────────

/// Response shape for manipulation class endpoints.
#[derive(Debug, Serialize, Deserialize)]
pub struct ManipulationView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub direction: String,
    pub priority: i32,
    pub is_active: bool,
    pub rules: Vec<Rule>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<&ManModel> for ManipulationView {
    type Error = ApiError;

    fn try_from(m: &ManModel) -> Result<Self, ApiError> {
        let rules: Vec<Rule> = serde_json::from_value(m.rules.clone())
            .map_err(|e| ApiError::internal(format!("failed to parse stored rules: {}", e)))?;
        Ok(Self {
            id: m.id.clone(),
            name: m.name.clone(),
            description: m.description.clone(),
            direction: m.direction.clone(),
            priority: m.priority,
            is_active: m.is_active,
            rules,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
    }
}

impl TryFrom<ManModel> for ManipulationView {
    type Error = ApiError;

    fn try_from(m: ManModel) -> Result<Self, ApiError> {
        ManipulationView::try_from(&m)
    }
}

/// Request body for POST /manipulations (create).
#[derive(Debug, Deserialize)]
pub struct ManipulationCreateBody {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

/// Request body for PUT /manipulations/{name} (full replacement).
#[derive(Debug, Deserialize)]
pub struct ManipulationUpdateBody {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

// ─── D-34 14-step validation pipeline ────────────────────────────────────────

/// Validate actions/anti_actions for forbidden headers, sip_code range, and
/// sleep range (D-34 steps 10-12).
fn validate_actions(actions: &[Action]) -> Result<(), ApiError> {
    for action in actions {
        match action {
            Action::SetHeader { name, value: _ } => {
                let lower = name.to_ascii_lowercase();
                if FORBIDDEN_HEADERS.contains(&lower.as_str()) {
                    return Err(ApiError::bad_request(format!(
                        "header '{}' is system-critical and cannot be mutated; \
                         allowed examples: User-Agent, P-Asserted-Identity, X-*",
                        name
                    )));
                }
            }
            Action::RemoveHeader { name } => {
                let lower = name.to_ascii_lowercase();
                if FORBIDDEN_HEADERS.contains(&lower.as_str()) {
                    return Err(ApiError::bad_request(format!(
                        "header '{}' is system-critical and cannot be mutated; \
                         allowed examples: User-Agent, P-Asserted-Identity, X-*",
                        name
                    )));
                }
            }
            Action::Hangup { sip_code, .. } => {
                if !(*sip_code >= 400 && *sip_code <= 699) {
                    return Err(ApiError::bad_request(format!(
                        "hangup.sip_code {} out of valid range [400, 699]",
                        sip_code
                    )));
                }
            }
            Action::Sleep { duration_ms } => {
                if *duration_ms < 10 || *duration_ms > 5000 {
                    return Err(ApiError::bad_request(format!(
                        "sleep.duration_ms {} out of valid range [10, 5000]",
                        duration_ms
                    )));
                }
            }
            // Log level enforced by serde; SetVar/Log have no additional constraints.
            Action::Log { .. } | Action::SetVar { .. } => {}
        }
    }
    Ok(())
}

/// Check interpolation syntax in a string — reject unclosed `${` (D-34 step 14).
/// Unknown variable names are NOT rejected per D-20 (warn-and-continue at runtime).
fn validate_interpolation(s: &str) -> Result<(), ApiError> {
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            if let Some('{') = chars.peek() {
                chars.next(); // consume '{'
                let mut found_close = false;
                for inner in chars.by_ref() {
                    if inner == '}' {
                        found_close = true;
                        break;
                    }
                }
                if !found_close {
                    return Err(ApiError::bad_request(
                        "invalid interpolation syntax: unclosed '${'".to_string(),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Full D-34 14-step validation. Steps run cheapest first (name, enums)
/// then expensive (regex compile) last.
pub fn validate_class(
    name: &str,
    direction: &str,
    priority: i32,
    rules: &[Rule],
) -> Result<(), ApiError> {
    // Step 1 — name format + length.
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(ApiError::bad_request(format!(
            "name must be 1-{} chars",
            MAX_NAME_LEN
        )));
    }
    for &b in name.as_bytes() {
        if !(b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-') {
            return Err(ApiError::bad_request(
                "name must contain only lowercase letters, digits, and dashes \
                 (^[a-z0-9-]+$)"
                    .to_string(),
            ));
        }
    }

    // Step 2 — direction enum.
    if !VALID_DIRECTIONS.contains(&direction) {
        return Err(ApiError::bad_request(format!(
            "direction '{}' invalid: must be one of {}",
            direction,
            VALID_DIRECTIONS.join(", ")
        )));
    }

    // Step 3 — priority range.
    if !(MIN_PRIORITY..=MAX_PRIORITY).contains(&priority) {
        return Err(ApiError::bad_request(format!(
            "priority {} out of range [{}, {}]",
            priority, MIN_PRIORITY, MAX_PRIORITY
        )));
    }

    for (rule_idx, rule) in rules.iter().enumerate() {
        // Step 4 — each rule has ≥1 condition.
        if rule.conditions.is_empty() {
            return Err(ApiError::bad_request(format!(
                "rule[{}] must have at least 1 condition",
                rule_idx
            )));
        }

        // Step 5 — each rule has ≥1 action OR ≥1 anti_action.
        if rule.actions.is_empty() && rule.anti_actions.is_empty() {
            return Err(ApiError::bad_request(format!(
                "rule[{}] must have at least 1 action or 1 anti_action",
                rule_idx
            )));
        }

        for (cond_idx, cond) in rule.conditions.iter().enumerate() {
            // Step 6 — condition.source in locked enum.
            if !is_valid_source(&cond.source) {
                return Err(ApiError::bad_request(format!(
                    "rule[{}].conditions[{}].source '{}' invalid: must be one of \
                     caller_number, destination_number, trunk, header:<name>, var:<name>",
                    rule_idx, cond_idx, cond.source
                )));
            }

            // Step 7 — condition.op enforced by serde (unknown op -> 400 from extractor).

            // Step 8 — regex patterns compile + ≤4096 chars.
            if matches!(cond.op, ConditionOp::Regex | ConditionOp::NotRegex) {
                if cond.value.len() > MAX_REGEX_LEN {
                    return Err(ApiError::bad_request(format!(
                        "rule[{}].conditions[{}].value length {} exceeds cap of {} chars",
                        rule_idx,
                        cond_idx,
                        cond.value.len(),
                        MAX_REGEX_LEN
                    )));
                }
                regex::Regex::new(&cond.value).map_err(|e| {
                    ApiError::bad_request(format!(
                        "rule[{}].conditions[{}].value is not a valid regex: {}",
                        rule_idx, cond_idx, e
                    ))
                })?;
            }
        }

        // Steps 9-13 — action type (by serde), forbidden headers, sip_code, sleep, log level.
        validate_actions(&rule.actions)?;
        validate_actions(&rule.anti_actions)?;

        // Step 14 — variable interpolation syntax in action value/message fields.
        for action in rule.actions.iter().chain(rule.anti_actions.iter()) {
            match action {
                Action::SetHeader { value, .. } => validate_interpolation(value)?,
                Action::SetVar { value, .. } => validate_interpolation(value)?,
                Action::Log { message, .. } => validate_interpolation(message)?,
                _ => {}
            }
        }
    }

    Ok(())
}

// ─── Router ──────────────────────────────────────────────────────────────────

/// Mount the `/manipulations` sub-router. Auth is applied by the parent
/// `api_v1_router` middleware so anonymous CRUD is impossible (T-09-01-04).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/manipulations", get(list).post(create))
        .route(
            "/manipulations/{name}",
            get(fetch).put(replace).delete(remove),
        )
}

// ─── Handlers ────────────────────────────────────────────────────────────────

async fn list(
    State(state): State<AppState>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<PaginatedResponse<ManipulationView>>> {
    let db = state.db();
    let page_no = pagination.page.max(1);
    let page_size = pagination.limit();

    let paginator = ManEntity::find()
        .order_by_asc(ManColumn::Priority)
        .order_by_asc(ManColumn::Name)
        .paginate(db, page_size);

    let total = paginator
        .num_items()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let rows = paginator
        .fetch_page(page_no.saturating_sub(1))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let views: Result<Vec<ManipulationView>, ApiError> =
        rows.iter().map(ManipulationView::try_from).collect();

    Ok(Json(PaginatedResponse::new(
        views?,
        page_no,
        page_size,
        total,
    )))
}

async fn create(
    State(state): State<AppState>,
    Json(raw): Json<serde_json::Value>,
) -> ApiResult<(StatusCode, Json<ManipulationView>)> {
    // Parse manually so serde deserialization errors (unknown enum variants, etc.)
    // map to 400 Bad Request rather than axum's default 422 Unprocessable Entity.
    let req: ManipulationCreateBody = serde_json::from_value(raw)
        .map_err(|e| ApiError::bad_request(format!("invalid request body: {}", e)))?;
    let db = state.db();

    let direction = req
        .direction
        .as_deref()
        .unwrap_or(DEFAULT_DIRECTION)
        .to_string();
    let priority = req.priority.unwrap_or(DEFAULT_PRIORITY);

    validate_class(&req.name, &direction, priority, &req.rules)?;

    // Pre-check duplicate name → 409.
    let dup = ManEntity::find()
        .filter(ManColumn::Name.eq(req.name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if dup.is_some() {
        return Err(ApiError::conflict(format!(
            "manipulation class '{}' already exists",
            req.name
        )));
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let rules_json = serde_json::to_value(&req.rules)
        .map_err(|e| ApiError::internal(format!("failed to serialize rules: {}", e)))?;

    let am = manipulations::ActiveModel {
        id: Set(id),
        name: Set(req.name),
        description: Set(req.description),
        direction: Set(direction),
        priority: Set(priority),
        is_active: Set(req.is_active.unwrap_or(true)),
        rules: Set(rules_json),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let inserted = am
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let view = ManipulationView::try_from(inserted)?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn fetch(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<ManipulationView>> {
    let db = state.db();
    let row = ManEntity::find()
        .filter(ManColumn::Name.eq(name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("manipulation class '{}' not found", name))
        })?;
    Ok(Json(ManipulationView::try_from(row)?))
}

async fn replace(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(raw): Json<serde_json::Value>,
) -> ApiResult<Json<ManipulationView>> {
    // Parse manually so serde deserialization errors map to 400 Bad Request.
    let req: ManipulationUpdateBody = serde_json::from_value(raw)
        .map_err(|e| ApiError::bad_request(format!("invalid request body: {}", e)))?;
    let db = state.db();

    let direction = req
        .direction
        .as_deref()
        .unwrap_or(DEFAULT_DIRECTION)
        .to_string();
    let priority = req.priority.unwrap_or(DEFAULT_PRIORITY);

    validate_class(&req.name, &direction, priority, &req.rules)?;

    let existing = ManEntity::find()
        .filter(ManColumn::Name.eq(name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("manipulation class '{}' not found", name))
        })?;

    // Duplicate-name check on rename.
    if req.name != existing.name {
        let dup = ManEntity::find()
            .filter(ManColumn::Name.eq(req.name.clone()))
            .one(db)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        if dup.is_some() {
            return Err(ApiError::conflict(format!(
                "manipulation class '{}' already exists",
                req.name
            )));
        }
    }

    let class_id = existing.id.clone();
    let preserved_created_at = existing.created_at;

    let rules_json = serde_json::to_value(&req.rules)
        .map_err(|e| ApiError::internal(format!("failed to serialize rules: {}", e)))?;

    let mut am: manipulations::ActiveModel = existing.into();
    am.name = Set(req.name);
    am.description = Set(req.description);
    am.direction = Set(direction);
    am.priority = Set(priority);
    am.is_active = Set(req.is_active.unwrap_or(true));
    am.rules = Set(rules_json);
    am.created_at = Set(preserved_created_at);
    am.updated_at = Set(Utc::now());

    let updated = am
        .update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // D-33: drop cached compiled regexes so the next INVITE recompiles from
    // the freshly-stored rules.
    state.manipulation_engine().invalidate_class(&class_id);

    Ok(Json(ManipulationView::try_from(updated)?))
}

async fn remove(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();

    let existing = ManEntity::find()
        .filter(ManColumn::Name.eq(name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(format!("manipulation class '{}' not found", name))
        })?;

    let class_id = existing.id.clone();

    // D-33: invalidate cache BEFORE the row vanishes.
    state.manipulation_engine().invalidate_class(&class_id);

    ManEntity::delete_by_id(class_id)
        .exec(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

// ─── Validator unit tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod validators {
    use super::*;
    use crate::proxy::manipulation::{Condition, ConditionMode, Rule};

    fn simple_rule(source: &str, op: ConditionOp, value: &str) -> Rule {
        Rule {
            name: None,
            conditions: vec![Condition {
                source: source.into(),
                op,
                value: value.into(),
            }],
            condition_mode: ConditionMode::And,
            actions: vec![Action::SetHeader {
                name: "X-Test".into(),
                value: "1".into(),
            }],
            anti_actions: vec![],
        }
    }

    #[test]
    fn name_lowercase_ok() {
        let r = simple_rule("caller_number", ConditionOp::Equals, "x");
        assert!(validate_class("uk-normalize", "both", 100, &[r]).is_ok());
    }

    #[test]
    fn name_uppercase_rejected() {
        let r = simple_rule("caller_number", ConditionOp::Equals, "x");
        assert!(validate_class("UK", "both", 100, &[r]).is_err());
    }

    #[test]
    fn name_too_long_rejected() {
        let big = "a".repeat(65);
        let r = simple_rule("caller_number", ConditionOp::Equals, "x");
        assert!(validate_class(&big, "both", 100, &[r]).is_err());
    }

    #[test]
    fn invalid_direction_rejected() {
        let r = simple_rule("caller_number", ConditionOp::Equals, "x");
        assert!(validate_class("name", "sideways", 100, &[r]).is_err());
    }

    #[test]
    fn priority_out_of_range_rejected() {
        let r = simple_rule("caller_number", ConditionOp::Equals, "x");
        assert!(validate_class("name", "both", 2000, &[r.clone()]).is_err());
        assert!(validate_class("name", "both", -2000, &[r]).is_err());
    }

    #[test]
    fn empty_conditions_rejected() {
        let rule = Rule {
            name: None,
            conditions: vec![],
            condition_mode: ConditionMode::And,
            actions: vec![Action::SetHeader {
                name: "X-A".into(),
                value: "1".into(),
            }],
            anti_actions: vec![],
        };
        assert!(validate_class("name", "both", 100, &[rule]).is_err());
    }

    #[test]
    fn no_actions_and_no_anti_actions_rejected() {
        let rule = Rule {
            name: None,
            conditions: vec![Condition {
                source: "caller_number".into(),
                op: ConditionOp::Equals,
                value: "x".into(),
            }],
            condition_mode: ConditionMode::And,
            actions: vec![],
            anti_actions: vec![],
        };
        assert!(validate_class("name", "both", 100, &[rule]).is_err());
    }

    #[test]
    fn invalid_source_rejected() {
        let r = simple_rule("bogus", ConditionOp::Equals, "x");
        assert!(validate_class("name", "both", 100, &[r]).is_err());
    }

    #[test]
    fn header_prefix_source_valid() {
        let r = simple_rule("header:X-Foo", ConditionOp::Contains, "bar");
        assert!(validate_class("name", "both", 100, &[r]).is_ok());
    }

    #[test]
    fn var_prefix_source_valid() {
        let r = simple_rule("var:greeting", ConditionOp::Equals, "played");
        assert!(validate_class("name", "both", 100, &[r]).is_ok());
    }

    #[test]
    fn bare_header_rejected() {
        let r = simple_rule("header", ConditionOp::Equals, "x");
        assert!(validate_class("name", "both", 100, &[r]).is_err());
    }

    #[test]
    fn bare_var_rejected() {
        let r = simple_rule("var", ConditionOp::Equals, "x");
        assert!(validate_class("name", "both", 100, &[r]).is_err());
    }

    #[test]
    fn invalid_regex_rejected() {
        let r = simple_rule("caller_number", ConditionOp::Regex, "(unclosed");
        assert!(validate_class("name", "both", 100, &[r]).is_err());
    }

    #[test]
    fn oversized_regex_rejected() {
        let big = "a".repeat(4097);
        let r = simple_rule("caller_number", ConditionOp::Regex, &big);
        assert!(validate_class("name", "both", 100, &[r]).is_err());
    }

    #[test]
    fn forbidden_header_set_rejected() {
        let rule = Rule {
            name: None,
            conditions: vec![Condition {
                source: "caller_number".into(),
                op: ConditionOp::Equals,
                value: "x".into(),
            }],
            condition_mode: ConditionMode::And,
            actions: vec![Action::SetHeader {
                name: "Via".into(),
                value: "evil".into(),
            }],
            anti_actions: vec![],
        };
        assert!(validate_class("name", "both", 100, &[rule]).is_err());
    }

    #[test]
    fn forbidden_header_case_insensitive_rejected() {
        let rule = Rule {
            name: None,
            conditions: vec![Condition {
                source: "caller_number".into(),
                op: ConditionOp::Equals,
                value: "x".into(),
            }],
            condition_mode: ConditionMode::And,
            actions: vec![Action::RemoveHeader {
                name: "cAlL-iD".into(),
            }],
            anti_actions: vec![],
        };
        assert!(validate_class("name", "both", 100, &[rule]).is_err());
    }

    #[test]
    fn sip_code_out_of_range_rejected() {
        let rule = Rule {
            name: None,
            conditions: vec![Condition {
                source: "caller_number".into(),
                op: ConditionOp::Equals,
                value: "x".into(),
            }],
            condition_mode: ConditionMode::And,
            actions: vec![Action::Hangup {
                sip_code: 200,
                reason: "OK".into(),
            }],
            anti_actions: vec![],
        };
        assert!(validate_class("name", "both", 100, &[rule]).is_err());
    }

    #[test]
    fn sip_code_valid_boundary() {
        let rule = Rule {
            name: None,
            conditions: vec![Condition {
                source: "caller_number".into(),
                op: ConditionOp::Equals,
                value: "x".into(),
            }],
            condition_mode: ConditionMode::And,
            actions: vec![Action::Hangup {
                sip_code: 400,
                reason: "Bad".into(),
            }],
            anti_actions: vec![],
        };
        assert!(validate_class("name", "both", 100, &[rule]).is_ok());
    }

    #[test]
    fn sleep_too_long_rejected() {
        let rule = Rule {
            name: None,
            conditions: vec![Condition {
                source: "caller_number".into(),
                op: ConditionOp::Equals,
                value: "x".into(),
            }],
            condition_mode: ConditionMode::And,
            actions: vec![Action::Sleep { duration_ms: 6000 }],
            anti_actions: vec![],
        };
        assert!(validate_class("name", "both", 100, &[rule]).is_err());
    }

    #[test]
    fn sleep_too_short_rejected() {
        let rule = Rule {
            name: None,
            conditions: vec![Condition {
                source: "caller_number".into(),
                op: ConditionOp::Equals,
                value: "x".into(),
            }],
            condition_mode: ConditionMode::And,
            actions: vec![Action::Sleep { duration_ms: 5 }],
            anti_actions: vec![],
        };
        assert!(validate_class("name", "both", 100, &[rule]).is_err());
    }

    #[test]
    fn interpolation_valid_placeholder() {
        assert!(validate_interpolation("hello ${caller_number}").is_ok());
        assert!(validate_interpolation("${var:x}_suffix").is_ok());
        assert!(validate_interpolation("no placeholders").is_ok());
    }

    #[test]
    fn interpolation_unclosed_brace_rejected() {
        assert!(validate_interpolation("${unclosed").is_err());
        assert!(validate_interpolation("before ${unclosed").is_err());
    }

    #[test]
    fn anti_actions_only_valid() {
        let rule = Rule {
            name: None,
            conditions: vec![Condition {
                source: "caller_number".into(),
                op: ConditionOp::Equals,
                value: "x".into(),
            }],
            condition_mode: ConditionMode::And,
            actions: vec![],
            anti_actions: vec![Action::SetHeader {
                name: "X-Alt".into(),
                value: "1".into(),
            }],
        };
        assert!(validate_class("name", "both", 100, &[rule]).is_ok());
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
