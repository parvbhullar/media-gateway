//! Stub created by Plan 05-01. Plan 05-03 fills in handlers (TSUB-05 CRUD).
//!
//! Empty router merged into protected `/api/v1` in `mod.rs` so Wave 2 plans
//! 05-02 and 05-03 can land in parallel without colliding on `mod.rs` edits.

use axum::Router;

use crate::app::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
}
