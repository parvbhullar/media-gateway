//! Bearer-token authentication helpers and middleware for `/api/v1/*`.
//!
//! API keys are issued as `rpbx_<64-hex>` strings. Only the lowercase hex
//! SHA-256 of the plaintext is persisted in `rustpbx_api_keys.hash_sha256`;
//! the plaintext is surfaced to the operator exactly once at creation time.
//!
//! The middleware extracts `Authorization: Bearer <token>`, hashes it with
//! SHA-256, and looks up a non-revoked row. On success it schedules a
//! fire-and-forget `last_used_at` touch and forwards the request. On any
//! failure it short-circuits with the shared `ApiError` JSON envelope.

use axum::{
    extract::{Request, State},
    http::{HeaderMap, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::app::AppState;
use crate::handler::api_v1::account_scope::AccountScope;
use crate::handler::api_v1::error::ApiError;
use crate::models::api_key;

/// The plaintext prefix prepended to every issued API key. Stable across
/// issuances so operators can recognise an rustpbx-issued token at a glance.
pub const API_KEY_PREFIX: &str = "rpbx_";

/// A freshly minted API key. `plaintext` must be shown to the operator
/// exactly once and never persisted; `hash` is the value stored in the DB.
pub struct IssuedKey {
    pub plaintext: String,
    pub hash: String,
}

/// Generate a new API key. Uses 32 random bytes rendered as lowercase hex
/// under the `rpbx_` prefix, yielding a 69-character plaintext token.
pub fn issue_api_key() -> IssuedKey {
    use rand::RngExt;
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes[..]);
    let hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
    let plaintext = format!("{}{}", API_KEY_PREFIX, hex);
    let hash = hash_token(&plaintext);
    IssuedKey { plaintext, hash }
}

/// Lowercase hex SHA-256 of the supplied plaintext token.
pub fn hash_token(plaintext: &str) -> String {
    let mut h = Sha256::new();
    h.update(plaintext.as_bytes());
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

/// Constant-time compare of a plaintext token against a stored hash.
pub fn verify_api_key_hash(plaintext: &str, stored_hash: &str) -> bool {
    let computed = hash_token(plaintext);
    computed.as_bytes().ct_eq(stored_hash.as_bytes()).into()
}

/// Axum middleware that enforces `Authorization: Bearer <token>` against
/// the `rustpbx_api_keys` table. All failure paths return the shared
/// `ApiError` JSON envelope with the appropriate HTTP status.
pub async fn api_v1_auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = extract_bearer(request.headers()) else {
        return ApiError::unauthorized("missing bearer token").into_response();
    };
    let hash = hash_token(&token);
    let db = state.db();

    let row = match api_key::Entity::find()
        .filter(api_key::Column::HashSha256.eq(hash))
        .filter(api_key::Column::RevokedAt.is_null())
        .one(db)
        .await
    {
        Ok(Some(m)) => m,
        Ok(None) => return ApiError::unauthorized("invalid api key").into_response(),
        Err(_) => return ApiError::internal("auth lookup failed").into_response(),
    };

    let scope = AccountScope::from_account_id(row.account_id.clone());
    request.extensions_mut().insert(scope);

    // Fire-and-forget `last_used_at` touch; never blocks the request.
    let db_clone = db.clone();
    let id = row.id;
    tokio::spawn(async move {
        let am = api_key::ActiveModel {
            id: Set(id),
            last_used_at: Set(Some(Utc::now())),
            ..Default::default()
        };
        let _ = am.update(&db_clone).await;
    });

    next.run(request).await
}

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|s| s.trim().to_string())
}

/// Revoke an API key by name. Used by the CLI `api-key revoke` command and
/// by integration tests that need to assert revocation takes effect
/// immediately (there is no caching layer).
pub async fn revoke_by_name(state: &AppState, name: &str) -> anyhow::Result<bool> {
    let db = state.db();
    if let Some(m) = api_key::Entity::find()
        .filter(api_key::Column::Name.eq(name.to_string()))
        .one(db)
        .await?
    {
        let mut am: api_key::ActiveModel = m.into();
        am.revoked_at = Set(Some(Utc::now()));
        am.update(db).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}
