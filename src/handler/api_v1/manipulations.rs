//! Phase 9 — Manipulations CRUD router stub (MAN-02).
//!
//! Plan 09-01 lands the router shape (`pub fn router() -> Router<AppState>`)
//! so 09-02 can replace handler bodies WITHOUT touching `mod.rs` (Phase 5/6/
//! 7/8 file-ownership pattern). Every handler returns
//! `(StatusCode::NOT_IMPLEMENTED, body)` until 09-02 lands the real CRUD
//! against `supersip_manipulations`.
//!
//! Endpoints (D-33):
//!   - GET    /manipulations          — list (501 stub → 09-02 paginated list)
//!   - POST   /manipulations          — create (501 stub → 09-02 201 + view)
//!   - GET    /manipulations/{name}   — fetch by name (D-04)
//!   - PUT    /manipulations/{name}   — full replacement (engine.invalidate_class)
//!   - DELETE /manipulations/{name}   — remove (engine.invalidate_class)

use axum::{Router, http::StatusCode, routing::get};

use crate::app::AppState;

/// Mount the `/manipulations` sub-router. Auth is applied by the parent
/// `api_v1_router` middleware so anonymous CRUD is impossible (T-09-01-04).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/manipulations", get(list_stub).post(create_stub))
        .route(
            "/manipulations/{name}",
            get(fetch_stub).put(replace_stub).delete(remove_stub),
        )
}

async fn list_stub() -> (StatusCode, &'static str) {
    tracing::debug!("manipulations stub: GET /manipulations");
    (StatusCode::NOT_IMPLEMENTED, "not yet implemented")
}

async fn create_stub() -> (StatusCode, &'static str) {
    tracing::debug!("manipulations stub: POST /manipulations");
    (StatusCode::NOT_IMPLEMENTED, "not yet implemented")
}

async fn fetch_stub() -> (StatusCode, &'static str) {
    tracing::debug!("manipulations stub: GET /manipulations/{{name}}");
    (StatusCode::NOT_IMPLEMENTED, "not yet implemented")
}

async fn replace_stub() -> (StatusCode, &'static str) {
    tracing::debug!("manipulations stub: PUT /manipulations/{{name}}");
    (StatusCode::NOT_IMPLEMENTED, "not yet implemented")
}

async fn remove_stub() -> (StatusCode, &'static str) {
    tracing::debug!("manipulations stub: DELETE /manipulations/{{name}}");
    (StatusCode::NOT_IMPLEMENTED, "not yet implemented")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds_without_panic() {
        let _r = router();
    }
}
