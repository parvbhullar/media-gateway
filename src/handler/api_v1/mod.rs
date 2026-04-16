//! `/api/v1/*` carrier-API router foundation (Plan 0).
//!
//! This module hosts the shared error envelope, Bearer-token authentication
//! middleware, and the root router that Plans 1+ will nest feature
//! sub-routers into (gateway health, routing, security, DIDs, etc.).

pub mod auth;
pub mod cdrs;
pub mod common;
pub mod diagnostics;
pub mod dids;
pub mod error;
pub mod gateways;
pub mod reload_steps;
pub mod system;
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
        // Plan 2: .merge(routing::router())
        // Plan 3: .merge(security::router())
        ;

    Router::<AppState>::new()
        .nest("/api/v1", protected)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::api_v1_auth_middleware,
        ))
        .with_state(state)
}
