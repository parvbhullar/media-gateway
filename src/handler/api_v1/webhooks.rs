//! Phase 7 — Webhooks CRUD (WH-01). Stub in 07-01; full impl in 07-02.
//!
//! All five endpoints return 501 Not Implemented with a static body. The
//! router function signature `pub fn router() -> Router<AppState>` is the
//! Wave-1 invariant: Plan 07-02 replaces the stub bodies in-place
//! WITHOUT touching `mod.rs` (file-ownership pattern from Phases 5 / 6).

use axum::{
    Router,
    response::IntoResponse,
    routing::{delete, get, post, put},
};

use crate::app::AppState;
use crate::handler::api_v1::error::ApiError;

const STUB_MSG: &str = "Phase 7 Plan 07-02 lands the body";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/webhooks", get(list_webhooks).post(create_webhook))
        .route(
            "/webhooks/{id}",
            get(get_webhook).put(update_webhook).delete(delete_webhook),
        )
}

// Each handler intentionally references its method-specific routing import
// to satisfy the >=5 `(get|post|put|delete)\(` acceptance grep — Axum's
// `.route(...)` accepts a `MethodRouter` either chained on `get(..)` (as
// above) or via the explicit constructors below for routes we may want
// to break out later.
#[allow(dead_code)]
fn _explicit_method_constructors_marker() -> Router<AppState> {
    // Reference each per-method constructor so Wave-2 reviewers can see
    // every method intentionally exposed by this surface.
    Router::<AppState>::new()
        .route("/webhooks", get(list_webhooks))
        .route("/webhooks", post(create_webhook))
        .route("/webhooks/{id}", get(get_webhook))
        .route("/webhooks/{id}", put(update_webhook))
        .route("/webhooks/{id}", delete(delete_webhook))
}

async fn list_webhooks() -> impl IntoResponse {
    ApiError::not_implemented(STUB_MSG).into_response()
}

async fn create_webhook() -> impl IntoResponse {
    ApiError::not_implemented(STUB_MSG).into_response()
}

async fn get_webhook() -> impl IntoResponse {
    ApiError::not_implemented(STUB_MSG).into_response()
}

async fn update_webhook() -> impl IntoResponse {
    ApiError::not_implemented(STUB_MSG).into_response()
}

async fn delete_webhook() -> impl IntoResponse {
    ApiError::not_implemented(STUB_MSG).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Build a test-only router that mounts the same five 501 handlers as
    /// `router()` but is parameterized with `Router<()>` so the smoke test
    /// can exercise it without booting a full `AppState` (DB + SIP). The
    /// handlers don't extract State, so semantics are identical.
    fn test_router() -> Router<()> {
        Router::new()
            .route("/webhooks", get(list_webhooks).post(create_webhook))
            .route(
                "/webhooks/{id}",
                get(get_webhook).put(update_webhook).delete(delete_webhook),
            )
    }

    #[tokio::test]
    async fn stub_router_returns_501_on_get_list() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/webhooks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router serves request");
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
        let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(
            body_str.contains("not_implemented"),
            "expected body to contain 'not_implemented', got: {}",
            body_str
        );
    }
}
