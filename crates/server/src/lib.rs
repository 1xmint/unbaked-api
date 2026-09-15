//! The HTTP server: routes, and the layers every request passes through.

pub mod config;
pub mod files;
pub mod idempotency;
pub mod prices;
pub mod problem;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use unbaked_pay::{Facilitator, Gate, HttpFacilitator, Terms};

use crate::config::{Config, Network};
use crate::idempotency::IdempotencyStore;

/// The payment gate, over whichever facilitator the server was given.
pub type PayGate = Gate<Arc<dyn Facilitator>>;

/// What every route can reach.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// `None` when there is no receiving address: paid routes answer 503.
    pub gate: Option<Arc<PayGate>>,
}

/// The largest request body accepted, in bytes.
pub const BODY_LIMIT: usize = 25 * 1024 * 1024;

/// Layer 1's guide for agents, copied from `docs/agents.md` at the pinned
/// commit. CI checks the copy still matches.
pub const GUIDE: &str = include_str!("../guide/agents.md");

/// The whole server for these settings, paying through the configured
/// facilitator.
pub fn app(config: Config) -> Router {
    let facilitator: Option<Arc<dyn Facilitator>> = match HttpFacilitator::new(&config.facilitator)
    {
        Ok(client) => Some(Arc::new(client)),
        Err(error) => {
            // Paid routes refuse rather than run unpaid.
            tracing::error!(%error, "cannot build the facilitator client");
            None
        }
    };
    app_with(config, facilitator)
}

/// The whole server, paying through `facilitator`.
pub fn app_with(config: Config, facilitator: Option<Arc<dyn Facilitator>>) -> Router {
    layered(
        routes_with(config, facilitator),
        Arc::new(IdempotencyStore::default()),
    )
}

/// The routes with no way to take payment, before the shared layers.
pub fn routes(config: Config) -> Router {
    routes_with(config, None)
}

/// The routes, before the shared layers.
pub fn routes_with(config: Config, facilitator: Option<Arc<dyn Facilitator>>) -> Router {
    let gate = match (facilitator, &config.pay_to) {
        (Some(facilitator), Some(pay_to)) => {
            let terms = match config.network {
                Network::BaseSepolia => Terms::base_sepolia(pay_to, config.daily_cap),
                Network::Base => Terms::base(pay_to, config.daily_cap),
            };
            Some(Arc::new(Gate::new(facilitator, terms)))
        }
        _ => None,
    };
    let state = AppState {
        config: Arc::new(config),
        gate,
    };
    Router::new()
        .route("/health", get(health))
        .route("/v1/guide", get(guide))
        .route("/v1/estimate", post(files::estimate_route))
        .route("/v1/edit", post(files::edit_route))
        .route("/v1/preview", post(files::preview_route))
        .route("/v1/listen", post(files::listen_route))
        .route("/v1/render", post(files::render_route))
        .with_state(state)
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

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "network": state.config.network.caip2(),
        "payments": state.gate.is_some(),
    }))
}

async fn guide() -> impl IntoResponse {
    ([(CONTENT_TYPE, "text/markdown; charset=utf-8")], GUIDE)
}
