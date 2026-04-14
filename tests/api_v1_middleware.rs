use axum::{
    Router,
    body::Body,
    http::{Request, header},
    routing::get,
};
use rustpbx::handler::api_v1::auth::{api_v1_auth_middleware, revoke_by_name};
use tower::ServiceExt;

mod common;
use common::test_state_with_api_key;

fn test_router(state: rustpbx::app::AppState) -> Router {
    Router::new()
        .route("/ping", get(|| async { "pong" }))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api_v1_auth_middleware,
        ))
        .with_state(state)
}

#[tokio::test]
async fn missing_bearer_returns_401() {
    let (state, _token) = test_state_with_api_key("test-missing").await;
    let app = test_router(state);
    let resp = app
        .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn valid_bearer_passes_through() {
    let (state, token) = test_state_with_api_key("test-valid").await;
    let app = test_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/ping")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn revoked_key_returns_401() {
    let (state, token) = test_state_with_api_key("test-revoked").await;
    revoke_by_name(&state, "test-revoked")
        .await
        .expect("revoke must succeed");
    let app = test_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/ping")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}
