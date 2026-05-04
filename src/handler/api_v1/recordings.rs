//! `/api/v1/recordings` — first-class recordings surface (Phase 12,
//! REC-01..REC-07). This file is mounted by mod.rs in Plan 12-01;
//! handlers are added in 12-02 (list/get/download/delete) and 12-03
//! (export/bulk).

use axum::Router;

use crate::app::AppState;

/// Empty router — handlers wire here in subsequent plans.
pub fn router() -> Router<AppState> {
    Router::new()
}
