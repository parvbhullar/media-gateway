use axum::{body::Body, http::Request};
use tower::ServiceExt;

mod common;

#[tokio::test]
async fn api_v1_requires_bearer() {
    let state = common::test_state_empty().await;
    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/ping")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}
