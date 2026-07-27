//! Console "Organizations" tab. Read/adjust-limits only — org create/update/
//! disable/enable is owned by the external system via `/api/v1/organizations`
//! (see `src/handler/api_v1/organizations.rs`). This page is gated by the
//! `organizations:read` / `organizations:write` permissions, same convention
//! as every other console section (`has_permission`).

use std::sync::Arc;

use axum::{Router, extract::State, http::HeaderMap, response::Response, routing::get};
use serde_json::json;

use crate::console::{ConsoleState, middleware::AuthRequired};

pub fn urls() -> Router<Arc<ConsoleState>> {
    Router::new().route("/organizations", get(page_organizations))
}

async fn page_organizations(
    State(state): State<Arc<ConsoleState>>,
    headers: HeaderMap,
    AuthRequired(user): AuthRequired,
) -> Response {
    let current_user = state.build_current_user_ctx(&user).await;
    let db = state.db();
    let orgs = match crate::models::organization::Model::list_all(db).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "failed to list organizations");
            Vec::new()
        }
    };

    let mut orgs_with_counts = Vec::with_capacity(orgs.len());
    for org in orgs {
        let today = crate::handler::api_v1::organizations::today_counts(db, &org.org_id)
            .await
            .unwrap_or(crate::handler::api_v1::organizations::OrgTodayCounts {
                did_count: 0,
                trunk_count: 0,
                extension_count: 0,
                call_record_count_today: 0,
            });
        orgs_with_counts.push(json!({
            "org_id": org.org_id,
            "name": org.name,
            "enabled": org.enabled,
            "max_cps": org.max_cps,
            "max_calls": org.max_calls,
            "contact_name": org.contact_name,
            "contact_email": org.contact_email,
            "today": today,
        }));
    }

    state.render_with_headers(
        "console/organizations.html",
        json!({
            "nav_active": "organizations",
            "current_user": current_user,
            "organizations": orgs_with_counts,
            "disable_url_prefix": state.url_for("/organizations"),
        }),
        &headers,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::ConsoleConfig, models::migration::Migrator};
    use axum::{body::to_bytes, http::StatusCode};
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

    #[tokio::test]
    async fn page_organizations_renders_with_no_orgs() {
        let state = setup_state().await;
        let user = superuser();
        let resp = page_organizations(State(state), HeaderMap::new(), AuthRequired(user)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Organizations"));
    }

    #[tokio::test]
    async fn page_organizations_renders_with_org_rows() {
        let state = setup_state().await;
        let db = state.db();
        crate::models::organization::Model::upsert(
            db,
            crate::models::organization::NewOrganization {
                org_id: "acme".into(),
                name: "Acme Corp".into(),
                enabled: true,
                max_cps: Some(5),
                max_calls: Some(20),
                contact_name: None,
                contact_email: None,
                notes: None,
            },
        )
        .await
        .expect("seed organization");

        let user = superuser();
        let resp = page_organizations(State(state), HeaderMap::new(), AuthRequired(user)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("acme"));
        assert!(html.contains("Acme Corp"));
    }
}
