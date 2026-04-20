//! `/api/v1/*` carrier-API router foundation (Plan 0).
//!
//! This module hosts the shared error envelope, Bearer-token authentication
//! middleware, and the root router that Plans 1+ will nest feature
//! sub-routers into (gateway health, routing, security, DIDs, etc.).

pub mod auth;
pub mod calls;                    // Phase 4 Plan 04-01 — CALL-01, CALL-02
pub mod cdrs;
pub mod common;
pub mod diagnostics;
pub mod dids;
pub mod error;
pub mod gateways;
pub mod reload_steps;
pub mod routing;                  // Phase 3 Plan 03-01 — RTE-03
pub mod system;
pub mod trunk_credentials;        // Phase 3 Plan 03-01 — TSUB-01
pub mod trunk_media;              // Phase 3 Plan 03-01 — TSUB-03
pub mod trunk_origination_uris;   // Phase 3 Plan 03-01 — TSUB-02
pub mod trunks;

use axum::{Router, middleware};

use crate::app::AppState;

/// Build the `/api/v1/*` router with Bearer-token authentication applied.
///
/// Plans 1+ add their sub-routers into the `protected` merge point below.
/// Plan 0 registers no routes — an unauthenticated request still short-
/// circuits with 401 because the middleware runs before routing.
pub fn api_v1_router(state: AppState) -> Router {
    // Sub-routers from later plans register here.
    let protected: Router<AppState> = Router::new()
        .merge(gateways::router())
        .merge(dids::router())
        .merge(cdrs::router())
        .merge(diagnostics::router())
        .merge(system::router())
        .merge(trunks::router())
        .merge(trunk_credentials::router())        // Phase 3 — TSUB-01
        .merge(trunk_origination_uris::router())   // Phase 3 — TSUB-02
        .merge(trunk_media::router())              // Phase 3 — TSUB-03
        .merge(routing::router())                  // Phase 3 — RTE-03
        .merge(calls::router())                    // Phase 4 Plan 04-01 — CALL-01, CALL-02
        ;

    Router::<AppState>::new()
        .nest("/api/v1", protected)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::api_v1_auth_middleware,
        ))
        .with_state(state)
}
