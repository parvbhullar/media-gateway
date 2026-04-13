use crate::{
    config_merge::read_default_country,
    console::{ConsoleState, middleware::AuthRequired},
    models::did::{self, Column as DidColumn, DidError, Entity as DidEntity, Model, NewDid, normalize_did},
};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::warn;

pub fn urls() -> Router<Arc<ConsoleState>> {
    Router::new()
        .route("/dids", get(list_dids).put(create_did))
        .route("/dids/page", get(page_dids))
        .route("/dids/bulk", post(bulk_create_dids))
        .route(
            "/dids/{number}",
            get(get_did).patch(update_did).delete(delete_did),
        )
}

async fn page_dids(
    State(state): State<Arc<ConsoleState>>,
    headers: HeaderMap,
    AuthRequired(user): AuthRequired,
) -> Response {
    let current_user = state.build_current_user_ctx(&user).await;
    state.render_with_headers(
        "console/dids.html",
        json!({
            "nav_active": "dids",
            "current_user": current_user,
            "list_url": state.url_for("/dids"),
            "bulk_url": state.url_for("/dids/bulk"),
            "trunks_url": state.url_for("/sip-trunk"),
        }),
        &headers,
    )
}

// ---------------------------------------------------------------------------
// Request / response shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    trunk: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    unassigned: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateDidPayload {
    number: String,
    #[serde(default)]
    trunk_name: Option<String>,
    #[serde(default)]
    extension_number: Option<String>,
    #[serde(default)]
    failover_trunk: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
struct BulkCreatePayload {
    #[serde(default)]
    trunk_name: Option<String>,
    numbers: Vec<String>,
    #[serde(default)]
    extension_number: Option<String>,
    #[serde(default)]
    failover_trunk: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

/// Use `Option<Option<T>>` semantics: absent => leave as-is, `Some(None)` => set NULL,
/// `Some(Some(v))` => set to v. `#[serde(default, deserialize_with = ...)]` would work
/// but serde_json's default handling of `null` already maps to `Some(None)` when the
/// field is declared as `Option<Option<T>>` with `#[serde(default)]`. However, to get
/// the "absent vs null" distinction we use `serde_with::rust::double_option` style
/// via a manual helper.
#[derive(Debug, Clone, Default, Deserialize)]
struct UpdateDidPayload {
    #[serde(default, deserialize_with = "deserialize_double_option")]
    trunk_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    extension_number: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    failover_trunk: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    label: Option<Option<String>>,
    #[serde(default)]
    enabled: Option<bool>,
}

fn deserialize_double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

// ---------------------------------------------------------------------------
// Error helpers
// ---------------------------------------------------------------------------

fn unprocessable(msg: impl Into<String>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "message": msg.into() })),
    )
        .into_response()
}

fn conflict(msg: impl Into<String>) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({ "message": msg.into() })),
    )
        .into_response()
}

fn not_found(msg: impl Into<String>) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "message": msg.into() })),
    )
        .into_response()
}

fn server_err(e: impl std::fmt::Display) -> Response {
    warn!("did handler server error: {}", e);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "message": "Internal server error" })),
    )
        .into_response()
}

fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "message": "Permission denied" })),
    )
        .into_response()
}

fn did_error_message(err: &DidError) -> String {
    match err {
        DidError::Empty => "DID number is required".to_string(),
        DidError::MissingRegion => {
            "No default country configured; DID must start with + (E.164)".to_string()
        }
        DidError::InvalidNumber(msg) => format!("Invalid phone number: {msg}"),
        DidError::UnknownCountry(code) => format!("Unknown country code: {code}"),
    }
}

// ---------------------------------------------------------------------------
// DID index reload plumbing
// ---------------------------------------------------------------------------

async fn reload_did_index(state: &Arc<ConsoleState>) {
    if let Some(app_state) = state.app_state() {
        app_state
            .sip_server()
            .inner
            .data_context
            .reload_did_index()
            .await;
    }
    // In test / headless contexts there is no sip_server wired up; that's OK.
    state.mark_pending_reload();
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_dids(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Query(query): Query<ListQuery>,
) -> Response {
    if !state.has_permission(&user, "trunks", "read").await {
        return forbidden();
    }
    let db = state.db();

    let mut selector = DidEntity::find();
    if query.unassigned.unwrap_or(false) {
        selector = selector.filter(DidColumn::TrunkName.is_null());
    } else if let Some(trunk) = query
        .trunk
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        selector = selector.filter(DidColumn::TrunkName.eq(trunk));
    }
    if let Some(q) = query.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let like = format!("%{}%", q);
        let cond = Condition::any()
            .add(DidColumn::Number.like(like.clone()))
            .add(DidColumn::Label.like(like));
        selector = selector.filter(cond);
    }
    selector = selector.order_by_asc(DidColumn::Number);

    match selector.all(db).await {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|m| serde_json::to_value(m).unwrap_or(json!({})))
                .collect();
            Json(json!({ "items": items })).into_response()
        }
        Err(err) => server_err(err),
    }
}

async fn create_did(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<CreateDidPayload>,
) -> Response {
    if !state.has_permission(&user, "trunks", "write").await {
        return forbidden();
    }
    let db = state.db();

    let region = read_default_country(db).await;
    let normalized = match normalize_did(&payload.number, region.as_deref()) {
        Ok(n) => n,
        Err(err) => return unprocessable(did_error_message(&err)),
    };

    let trunk_name = super::normalize_optional_string(&payload.trunk_name);

    match Model::get(db, &normalized).await {
        Ok(Some(_)) => {
            return conflict(format!("DID {normalized} already exists"));
        }
        Ok(None) => {}
        Err(err) => return server_err(err),
    }

    let new = NewDid {
        number: normalized.clone(),
        trunk_name,
        extension_number: super::normalize_optional_string(&payload.extension_number),
        failover_trunk: super::normalize_optional_string(&payload.failover_trunk),
        label: super::normalize_optional_string(&payload.label),
        enabled: payload.enabled,
    };

    if let Err(err) = Model::upsert(db, new).await {
        return server_err(err);
    }

    reload_did_index(&state).await;

    match Model::get(db, &normalized).await {
        Ok(Some(model)) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(model).unwrap_or(json!({}))),
        )
            .into_response(),
        Ok(None) => server_err("row vanished after insert"),
        Err(err) => server_err(err),
    }
}

async fn bulk_create_dids(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<BulkCreatePayload>,
) -> Response {
    if !state.has_permission(&user, "trunks", "write").await {
        return forbidden();
    }
    let db = state.db();

    let trunk_name = super::normalize_optional_string(&payload.trunk_name);

    let region = read_default_country(db).await;
    let ext = super::normalize_optional_string(&payload.extension_number);
    let failover = super::normalize_optional_string(&payload.failover_trunk);
    let label = super::normalize_optional_string(&payload.label);

    let mut accepted: Vec<Value> = Vec::new();
    let mut rejected: Vec<Value> = Vec::new();

    for raw in payload.numbers.iter() {
        let normalized = match normalize_did(raw, region.as_deref()) {
            Ok(n) => n,
            Err(err) => {
                rejected.push(json!({
                    "input": raw,
                    "reason": did_error_message(&err),
                }));
                continue;
            }
        };

        match Model::get(db, &normalized).await {
            Ok(Some(_)) => {
                rejected.push(json!({
                    "input": raw,
                    "normalized": normalized,
                    "reason": "duplicate",
                }));
                continue;
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(normalized = %normalized, error = %err, "bulk DID lookup failed");
                rejected.push(json!({
                    "input": raw,
                    "normalized": normalized,
                    "reason": "db error",
                }));
                continue;
            }
        }

        let new = NewDid {
            number: normalized.clone(),
            trunk_name: trunk_name.clone(),
            extension_number: ext.clone(),
            failover_trunk: failover.clone(),
            label: label.clone(),
            enabled: payload.enabled,
        };
        match Model::upsert(db, new).await {
            Ok(()) => {
                accepted.push(json!({
                    "input": raw,
                    "number": normalized,
                }));
            }
            Err(err) => {
                tracing::warn!(normalized = %normalized, error = %err, "bulk DID upsert failed");
                rejected.push(json!({
                    "input": raw,
                    "normalized": normalized,
                    "reason": "db error",
                }));
            }
        }
    }

    if !accepted.is_empty() {
        reload_did_index(&state).await;
    }

    Json(json!({
        "accepted": accepted,
        "rejected": rejected,
    }))
    .into_response()
}

async fn get_did(
    AxumPath(number): AxumPath<String>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "trunks", "read").await {
        return forbidden();
    }
    let db = state.db();
    let region = read_default_country(db).await;
    let normalized = match normalize_did(&number, region.as_deref()) {
        Ok(n) => n,
        Err(_) => return not_found("DID not found"),
    };
    match Model::get(db, &normalized).await {
        Ok(Some(m)) => Json(serde_json::to_value(m).unwrap_or(json!({}))).into_response(),
        Ok(None) => not_found(format!("DID {normalized} not found")),
        Err(err) => server_err(err),
    }
}

async fn update_did(
    AxumPath(number): AxumPath<String>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<UpdateDidPayload>,
) -> Response {
    if !state.has_permission(&user, "trunks", "write").await {
        return forbidden();
    }
    let db = state.db();
    let region = read_default_country(db).await;
    let normalized = match normalize_did(&number, region.as_deref()) {
        Ok(n) => n,
        Err(_) => return not_found("DID not found"),
    };

    let existing = match Model::get(db, &normalized).await {
        Ok(Some(m)) => m,
        Ok(None) => return not_found(format!("DID {normalized} not found")),
        Err(err) => return server_err(err),
    };

    // Merge: double-option semantics — absent => keep, Some(None) => clear,
    // Some(Some(v)) => set.
    let trunk_name = match payload.trunk_name {
        Some(Some(v)) => super::normalize_optional_string(&Some(v)),
        Some(None) => None,
        None => existing.trunk_name.clone(),
    };

    let extension_number = match payload.extension_number {
        Some(Some(v)) => super::normalize_optional_string(&Some(v)),
        Some(None) => None,
        None => existing.extension_number.clone(),
    };

    let failover_trunk = match payload.failover_trunk {
        Some(Some(v)) => super::normalize_optional_string(&Some(v)),
        Some(None) => None,
        None => existing.failover_trunk.clone(),
    };

    let label = match payload.label {
        Some(Some(v)) => super::normalize_optional_string(&Some(v)),
        Some(None) => None,
        None => existing.label.clone(),
    };

    let enabled = payload.enabled.unwrap_or(existing.enabled);

    let new = NewDid {
        number: normalized.clone(),
        trunk_name,
        extension_number,
        failover_trunk,
        label,
        enabled,
    };

    if let Err(err) = Model::upsert(db, new).await {
        return server_err(err);
    }

    reload_did_index(&state).await;

    match Model::get(db, &normalized).await {
        Ok(Some(m)) => Json(serde_json::to_value(m).unwrap_or(json!({}))).into_response(),
        Ok(None) => server_err("row vanished after update"),
        Err(err) => server_err(err),
    }
}

async fn delete_did(
    AxumPath(number): AxumPath<String>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "trunks", "write").await {
        return forbidden();
    }
    let db = state.db();
    let region = read_default_country(db).await;
    let normalized = match normalize_did(&number, region.as_deref()) {
        Ok(n) => n,
        // Nothing stored could match an unparseable input; treat as already-gone.
        Err(_) => return StatusCode::NO_CONTENT.into_response(),
    };

    if let Err(err) = did::Model::delete(db, &normalized).await {
        return server_err(err);
    }

    reload_did_index(&state).await;

    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ConsoleConfig, console::middleware::AuthRequired, models::migration::Migrator,
    };
    use axum::{
        body::to_bytes,
        extract::{Json as AxumJson, Path as AxumPath, Query as AxumQuery, State},
        http::StatusCode,
    };
    use chrono::Utc;
    use sea_orm::Database;
    use sea_orm_migration::MigratorTrait;

    fn superuser() -> crate::models::user::Model {
        let now = Utc::now();
        crate::models::user::Model {
            id: 1,
            email: "admin@rustpbx.com".into(),
            username: "admin".into(),
            password_hash: "hashed".into(),
            reset_token: None,
            reset_token_expires: None,
            last_login_at: None,
            last_login_ip: None,
            created_at: now,
            updated_at: now,
            is_active: true,
            is_staff: true,
            is_superuser: true,
            mfa_enabled: false,
            mfa_secret: None,
            auth_source: "local".into(),
        }
    }

    fn unprivileged_user() -> crate::models::user::Model {
        let now = Utc::now();
        crate::models::user::Model {
            id: 99,
            email: "limited@rustpbx.com".into(),
            username: "limited".into(),
            password_hash: "hashed".into(),
            reset_token: None,
            reset_token_expires: None,
            last_login_at: None,
            last_login_ip: None,
            created_at: now,
            updated_at: now,
            is_active: true,
            is_staff: false,
            is_superuser: false,
            mfa_enabled: false,
            mfa_secret: None,
            auth_source: "local".into(),
        }
    }

    async fn setup_state() -> Arc<ConsoleState> {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect sqlite memory");
        Migrator::up(&db, None).await.expect("run migrations");
        ConsoleState::initialize(
            Arc::new(crate::callrecord::DefaultCallRecordFormatter::default()),
            db,
            ConsoleConfig::default(),
        )
        .await
        .expect("initialize console state")
    }

    async fn resp_json(resp: Response) -> Value {
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        if body.is_empty() {
            return json!(null);
        }
        serde_json::from_slice(&body).unwrap_or(json!(null))
    }

    #[tokio::test]
    async fn create_did_denied_without_permission() {
        let state = setup_state().await;
        let user = unprivileged_user();
        let payload = CreateDidPayload {
            number: "+14155551212".into(),
            trunk_name: Some("t1".into()),
            extension_number: None,
            failover_trunk: None,
            label: None,
            enabled: true,
        };
        let resp = create_did(State(state), AuthRequired(user), AxumJson(payload)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_did_happy_path_then_conflict() {
        let state = setup_state().await;
        let user = superuser();
        let payload = CreateDidPayload {
            number: "+14155551212".into(),
            trunk_name: Some("t1".into()),
            extension_number: None,
            failover_trunk: None,
            label: Some("main".into()),
            enabled: true,
        };
        let resp = create_did(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumJson(payload.clone()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let v = resp_json(resp).await;
        assert_eq!(v["number"], "+14155551212");
        assert_eq!(v["trunk_name"], "t1");

        // duplicate
        let resp2 = create_did(State(state), AuthRequired(user), AxumJson(payload)).await;
        assert_eq!(resp2.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_did_invalid_returns_422() {
        let state = setup_state().await;
        let user = superuser();
        let payload = CreateDidPayload {
            number: "not-a-number".into(),
            trunk_name: Some("t1".into()),
            extension_number: None,
            failover_trunk: None,
            label: None,
            enabled: true,
        };
        let resp = create_did(State(state), AuthRequired(user), AxumJson(payload)).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn list_and_get_did() {
        let state = setup_state().await;
        let user = superuser();
        let payload = CreateDidPayload {
            number: "+14155551212".into(),
            trunk_name: Some("t1".into()),
            extension_number: None,
            failover_trunk: None,
            label: Some("main".into()),
            enabled: true,
        };
        let _ = create_did(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumJson(payload),
        )
        .await;

        let resp = list_dids(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumQuery(ListQuery::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = resp_json(resp).await;
        assert_eq!(v["items"].as_array().unwrap().len(), 1);

        // Filter by trunk
        let resp = list_dids(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumQuery(ListQuery {
                trunk: Some("other".into()),
                q: None,
                unassigned: None,
            }),
        )
        .await;
        let v = resp_json(resp).await;
        assert_eq!(v["items"].as_array().unwrap().len(), 0);

        // Filter by q matching label
        let resp = list_dids(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumQuery(ListQuery {
                trunk: None,
                q: Some("main".into()),
                unassigned: None,
            }),
        )
        .await;
        let v = resp_json(resp).await;
        assert_eq!(v["items"].as_array().unwrap().len(), 1);

        // Get
        let resp = get_did(
            AxumPath("+14155551212".to_string()),
            State(state),
            AuthRequired(user),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_did_missing_returns_404() {
        let state = setup_state().await;
        let user = superuser();
        let resp = get_did(
            AxumPath("+14155559999".to_string()),
            State(state),
            AuthRequired(user),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_did_with_unparseable_path_returns_404() {
        let state = setup_state().await;
        let user = superuser();
        let resp = get_did(
            AxumPath("not-a-number".to_string()),
            State(state),
            AuthRequired(user),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_did_with_unparseable_path_returns_404() {
        let state = setup_state().await;
        let user = superuser();
        let resp = update_did(
            AxumPath("not-a-number".to_string()),
            State(state),
            AuthRequired(user),
            AxumJson(UpdateDidPayload::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_did_with_unparseable_path_returns_204() {
        let state = setup_state().await;
        let user = superuser();
        let resp = delete_did(
            AxumPath("not-a-number".to_string()),
            State(state),
            AuthRequired(user),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn update_did_merges_fields() {
        let state = setup_state().await;
        let user = superuser();
        let _ = create_did(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumJson(CreateDidPayload {
                number: "+14155551212".into(),
                trunk_name: Some("t1".into()),
                extension_number: Some("1001".into()),
                failover_trunk: None,
                label: Some("main".into()),
                enabled: true,
            }),
        )
        .await;

        // PATCH: clear label (null), leave trunk_name as-is, disable
        let update = UpdateDidPayload {
            trunk_name: None,
            extension_number: None,
            failover_trunk: None,
            label: Some(None),
            enabled: Some(false),
        };
        let resp = update_did(
            AxumPath("+14155551212".to_string()),
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumJson(update),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = resp_json(resp).await;
        assert_eq!(v["trunk_name"], "t1");
        assert_eq!(v["extension_number"], "1001");
        assert!(v["label"].is_null());
        assert_eq!(v["enabled"], false);
    }

    #[tokio::test]
    async fn update_did_missing_returns_404() {
        let state = setup_state().await;
        let user = superuser();
        let resp = update_did(
            AxumPath("+14155559999".to_string()),
            State(state),
            AuthRequired(user),
            AxumJson(UpdateDidPayload::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_did_is_idempotent() {
        let state = setup_state().await;
        let user = superuser();
        let _ = create_did(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumJson(CreateDidPayload {
                number: "+14155551212".into(),
                trunk_name: Some("t1".into()),
                extension_number: None,
                failover_trunk: None,
                label: None,
                enabled: true,
            }),
        )
        .await;

        let resp = delete_did(
            AxumPath("+14155551212".to_string()),
            State(state.clone()),
            AuthRequired(user.clone()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Second delete still 204
        let resp = delete_did(
            AxumPath("+14155551212".to_string()),
            State(state),
            AuthRequired(user),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn bulk_add_partial_success() {
        let state = setup_state().await;
        let user = superuser();

        // Pre-seed duplicate
        let _ = create_did(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumJson(CreateDidPayload {
                number: "+14155551212".into(),
                trunk_name: Some("t1".into()),
                extension_number: None,
                failover_trunk: None,
                label: None,
                enabled: true,
            }),
        )
        .await;

        let payload = BulkCreatePayload {
            trunk_name: Some("t1".into()),
            numbers: vec![
                "+14155551212".into(), // dup
                "+14155551213".into(), // ok
                "not-a-number".into(), // invalid
                "+14155551214".into(), // ok
            ],
            extension_number: None,
            failover_trunk: None,
            label: None,
            enabled: true,
        };
        let resp = bulk_create_dids(
            State(state),
            AuthRequired(user),
            AxumJson(payload),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = resp_json(resp).await;
        let accepted = v["accepted"].as_array().unwrap();
        let rejected = v["rejected"].as_array().unwrap();
        assert_eq!(accepted.len(), 2);
        assert_eq!(rejected.len(), 2);
    }

    #[tokio::test]
    async fn create_did_without_trunk_is_parked() {
        let state = setup_state().await;
        let user = superuser();
        let payload = CreateDidPayload {
            number: "+14158675309".into(),
            trunk_name: None,
            extension_number: None,
            failover_trunk: None,
            label: Some("parked".into()),
            enabled: true,
        };
        let resp = create_did(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumJson(payload),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let v = resp_json(resp).await;
        assert_eq!(v["number"], "+14158675309");
        assert!(v["trunk_name"].is_null());

        // Filter list by unassigned=true
        let resp = list_dids(
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumQuery(ListQuery {
                trunk: None,
                q: None,
                unassigned: Some(true),
            }),
        )
        .await;
        let v = resp_json(resp).await;
        assert_eq!(v["items"].as_array().unwrap().len(), 1);

        // Detach via PATCH round-trip: re-attach then detach
        let resp = update_did(
            AxumPath("+14158675309".to_string()),
            State(state.clone()),
            AuthRequired(user.clone()),
            AxumJson(UpdateDidPayload {
                trunk_name: Some(Some("t1".into())),
                ..UpdateDidPayload::default()
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = resp_json(resp).await;
        assert_eq!(v["trunk_name"], "t1");

        let resp = update_did(
            AxumPath("+14158675309".to_string()),
            State(state),
            AuthRequired(user),
            AxumJson(UpdateDidPayload {
                trunk_name: Some(None),
                ..UpdateDidPayload::default()
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = resp_json(resp).await;
        assert!(v["trunk_name"].is_null());
    }

    #[tokio::test]
    async fn bulk_create_without_trunk_parks_all() {
        let state = setup_state().await;
        let user = superuser();
        let payload = BulkCreatePayload {
            trunk_name: None,
            numbers: vec![
                "+14155551213".into(),
                "+14155551214".into(),
            ],
            extension_number: None,
            failover_trunk: None,
            label: None,
            enabled: true,
        };
        let resp = bulk_create_dids(State(state.clone()), AuthRequired(user), AxumJson(payload)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = resp_json(resp).await;
        assert_eq!(v["accepted"].as_array().unwrap().len(), 2);
        let db = state.db();
        assert_eq!(did::Model::count_unassigned(db).await.unwrap(), 2);
    }
}
