use axum::response::IntoResponse;
use http_body_util::BodyExt;
use rustpbx::handler::api_v1::error::ApiError;

#[tokio::test]
async fn api_error_renders_json_envelope() {
    let resp = ApiError::unauthorized("missing bearer").into_response();
    assert_eq!(resp.status().as_u16(), 401);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "missing bearer");
    assert_eq!(v["code"], "unauthorized");
}
