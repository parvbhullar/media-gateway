//! `/api/v1/security/*` — Phase 10 Security Suite (SEC-01..SEC-05).
//!
//! Plan 10-01 Wave 1 stub router. Real bodies land in 10-02; this file
//! exists so AppState wiring + `mod.rs` merge are stable for Wave 2 plans
//! that own neither file.

use axum::{Router, extract::State, http::StatusCode};

use crate::app::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/security/firewall",
            axum::routing::get(list_firewall).patch(replace_firewall),
        )
        .route("/security/flood-tracker", axum::routing::get(list_flood_tracker))
        .route("/security/blocks", axum::routing::get(list_blocks))
        .route("/security/blocks/:ip", axum::routing::delete(delete_block))
        .route("/security/auth-failures", axum::routing::get(list_auth_failures))
}

async fn list_firewall(State(_state): State<AppState>) -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "firewall — coming in 10-02")
}
async fn replace_firewall(State(_state): State<AppState>) -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "firewall — coming in 10-02")
}
async fn list_flood_tracker(State(_state): State<AppState>) -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "flood-tracker — coming in 10-02")
}
async fn list_blocks(State(_state): State<AppState>) -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "blocks — coming in 10-02")
}
async fn delete_block(State(_state): State<AppState>) -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "blocks — coming in 10-02")
}
async fn list_auth_failures(State(_state): State<AppState>) -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "auth-failures — coming in 10-02")
}
