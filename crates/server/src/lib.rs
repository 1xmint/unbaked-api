//! The HTTP server: routes, and the layers every request passes through.

pub mod config;
pub mod idempotency;
pub mod problem;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router, middleware};

use crate::config::Config;
use crate::idempotency::IdempotencyStore;

/// The largest request body accepted, in bytes.
pub const BODY_LIMIT: usize = 25 * 1024 * 1024;

/// Layer 1's guide for agents, copied from `docs/agents.md` at the pinned
/// commit. CI checks the copy still matches.
pub const GUIDE: &str = include_str!("../guide/agents.md");

/// The whole server for these settings.
pub fn app(config: Config) -> Router {
    layered(routes(config), Arc::new(IdempotencyStore::default()))
}

/// The routes, before the shared layers.
pub fn routes(config: Config) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/guide", get(guide))
        .with_state(Arc::new(config))
}

/// Wraps routes in the layers every request passes through, outermost last:
/// the body cap, `Idempotency-Key` (outside the payment gate, so a replay is
/// never charged twice), and problem+json for every error.
pub fn layered(routes: Router, store: Arc<IdempotencyStore>) -> Router {
    routes
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn_with_state(store, idempotency::layer))
        .layer(middleware::from_fn(problem::plain_errors))
}

async fn health(State(config): State<Arc<Config>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "network": config.network.caip2(),
    }))
}

async fn guide() -> impl IntoResponse {
    ([(CONTENT_TYPE, "text/markdown; charset=utf-8")], GUIDE)
}
