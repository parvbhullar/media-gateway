//! `/api/v1/admin/keys` — per-tenant API key administration (task 3.2).
//!
//! - `POST   /admin/keys`        mint a scoped key for `tenant_id` (admin only).
//! - `GET    /admin/keys`        list the caller's own tenant's keys (no plaintext).
//! - `DELETE /admin/keys/{name}` revoke a key by name within the caller's tenant.
//!
//! The caller's tenant comes from `Extension<TenantId>`, attached by
//! `api_v1_auth_middleware` from the authenticated key row — never from the
//! request body or URL. `tenant_id` in the mint body is the *target* tenant the
//! new key is scoped to; minting for an arbitrary target requires the caller to
//! be the admin tenant (`ADMIN_TENANT_ID`). The plaintext token is returned
//! exactly once, on mint, and never persisted (only its SHA-256 is stored).

use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::handler::api_v1::auth::{ADMIN_TENANT_ID, TenantId, issue_api_key};
use crate::handler::api_v1::error::{ApiError, ApiResult};
use crate::models::api_key;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/keys", post(mint_key).get(list_keys))
        .route("/admin/keys/{name}", delete(revoke_key))
}

/// Mint request. `tenant_id` is required — there is no silent default (minting
/// without an explicit target tenant is a 400, never a quiet `tenant_id=1`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MintKeyRequest {
    pub tenant_id: Option<i64>,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Mint response — the only place `plaintext` is ever surfaced.
#[derive(Debug, Serialize)]
pub struct MintKeyResponse {
    pub plaintext: String,
    pub hash: String,
    pub name: String,
    pub tenant_id: i64,
    pub created_at: DateTime<Utc>,
}

/// Non-secret view of a key row (no plaintext; the stored value is only a hash).
#[derive(Debug, Serialize)]
pub struct KeyView {
    pub id: i64,
    pub tenant_id: i64,
    pub name: String,
    pub hash_sha256: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl From<api_key::Model> for KeyView {
    fn from(m: api_key::Model) -> Self {
        Self {
            id: m.id,
            tenant_id: m.tenant_id,
            name: m.name,
            hash_sha256: m.hash_sha256,
            description: m.description,
            created_at: m.created_at,
            last_used_at: m.last_used_at,
            revoked_at: m.revoked_at,
        }
    }
}

async fn mint_key(
    State(state): State<AppState>,
    Extension(caller): Extension<TenantId>,
    Json(req): Json<MintKeyRequest>,
) -> ApiResult<(StatusCode, Json<MintKeyResponse>)> {
    // Only the admin tenant may mint keys (for any target tenant).
    if caller.0 != ADMIN_TENANT_ID {
        return Err(ApiError::forbidden(
            "minting API keys requires an admin-tenant key",
        ));
    }

    let tenant_id = req
        .tenant_id
        .ok_or_else(|| ApiError::bad_request("tenant_id is required"))?;
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }

    let db = state.db();

    // Pre-check for a clean 409 (the global unique index on name also guards).
    if api_key::Entity::find()
        .filter(api_key::Column::Name.eq(name))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .is_some()
    {
        return Err(ApiError::conflict(format!(
            "api key '{name}' already exists"
        )));
    }

    let issued = issue_api_key();
    let now = Utc::now();
    let active = api_key::ActiveModel {
        tenant_id: Set(tenant_id),
        name: Set(name.to_string()),
        hash_sha256: Set(issued.hash.clone()),
        description: Set(req.description.clone()),
        created_at: Set(now),
        last_used_at: Set(None),
        revoked_at: Set(None),
        ..Default::default() // id NotSet → auto-increment
    };
    active
        .insert(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(MintKeyResponse {
            plaintext: issued.plaintext,
            hash: issued.hash,
            name: name.to_string(),
            tenant_id,
            created_at: now,
        }),
    ))
}

async fn list_keys(
    State(state): State<AppState>,
    Extension(caller): Extension<TenantId>,
) -> ApiResult<Json<Vec<KeyView>>> {
    let db = state.db();
    let rows = api_key::Entity::find()
        .filter(api_key::Column::TenantId.eq(caller.0))
        .all(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(rows.into_iter().map(KeyView::from).collect()))
}

async fn revoke_key(
    State(state): State<AppState>,
    Extension(caller): Extension<TenantId>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let db = state.db();
    let row = api_key::Entity::find()
        .filter(api_key::Column::TenantId.eq(caller.0))
        .filter(api_key::Column::Name.eq(name.clone()))
        .one(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("api key '{name}' not found")))?;

    let mut am: api_key::ActiveModel = row.into();
    am.revoked_at = Set(Some(Utc::now()));
    am.update(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::migration::Migrator;
    use sea_orm::{Database, DatabaseConnection};
    use sea_orm_migration::MigratorTrait;

    async fn fresh() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.expect("sqlite");
        Migrator::up(&db, None).await.expect("migrate");
        db
    }

    async fn insert_key(db: &DatabaseConnection, tenant: Option<i64>, name: &str, hash: &str) {
        let now = Utc::now();
        let mut am = api_key::ActiveModel {
            name: Set(name.to_string()),
            hash_sha256: Set(hash.to_string()),
            created_at: Set(now),
            ..Default::default()
        };
        // Leave tenant_id NotSet when None so the column default (1) applies.
        if let Some(t) = tenant {
            am.tenant_id = Set(t);
        }
        am.insert(db).await.expect("insert api key");
    }

    #[tokio::test]
    async fn tenant_id_defaults_to_admin_tenant() {
        // A key inserted without tenant_id falls to the column default (1), so
        // pre-3.2 keys become admin-tenant keys.
        let db = fresh().await;
        insert_key(&db, None, "legacy", &"a".repeat(64)).await;
        let row = api_key::Entity::find()
            .filter(api_key::Column::Name.eq("legacy"))
            .one(&db)
            .await
            .unwrap()
            .expect("row");
        assert_eq!(row.tenant_id, ADMIN_TENANT_ID);
    }

    #[tokio::test]
    async fn keys_scope_by_tenant() {
        let db = fresh().await;
        insert_key(&db, Some(1), "k1", &"b".repeat(64)).await;
        insert_key(&db, Some(2), "k2", &"c".repeat(64)).await;
        let t1 = api_key::Entity::find()
            .filter(api_key::Column::TenantId.eq(1))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(t1.len(), 1);
        assert_eq!(t1[0].name, "k1");
        let t2 = api_key::Entity::find()
            .filter(api_key::Column::TenantId.eq(2))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(t2.len(), 1);
        assert_eq!(t2[0].name, "k2");
    }

    #[tokio::test]
    async fn key_name_is_globally_unique() {
        // Per the migration's documented deviation, name stays globally unique
        // (the column-level UNIQUE survives): the same name in a different
        // tenant is rejected.
        let db = fresh().await;
        insert_key(&db, Some(1), "dup", &"d".repeat(64)).await;
        let now = Utc::now();
        let err = api_key::ActiveModel {
            tenant_id: Set(2),
            name: Set("dup".to_string()),
            hash_sha256: Set("e".repeat(64)),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&db)
        .await;
        assert!(err.is_err(), "duplicate name across tenants must be rejected");
    }
}
