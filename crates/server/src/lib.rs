//! The HTTP server: routes, and the layers every request passes through.

pub mod config;
pub mod files;
pub mod idempotency;
pub mod images;
pub mod openai;
pub mod prices;
pub mod problem;
pub mod providers;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use unbaked_pay::{Facilitator, Gate, HttpFacilitator, Quote, Terms};

use crate::config::{Config, Network};
use crate::idempotency::IdempotencyStore;
use crate::openai::OpenAi;
use crate::problem::Problem;
use crate::providers::Pictures;

/// The payment gate, over whichever facilitator the server was given.
pub type PayGate = Gate<Arc<dyn Facilitator>>;

/// What every route can reach.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// `None` when there is no receiving address: paid routes answer 503.
    pub gate: Option<Arc<PayGate>>,
    /// `None` without an OpenAI key: picture routes answer 503.
    pub pictures: Option<Arc<dyn Pictures>>,
}

/// The outside services the server calls. Tests pass fakes.
#[derive(Clone, Default)]
pub struct Services {
    pub facilitator: Option<Arc<dyn Facilitator>>,
    pub pictures: Option<Arc<dyn Pictures>>,
}

/// The largest request body accepted, in bytes.
pub const BODY_LIMIT: usize = 25 * 1024 * 1024;

/// Layer 1's guide for agents, copied from `docs/agents.md` at the pinned
/// commit. CI checks the copy still matches.
pub const GUIDE: &str = include_str!("../guide/agents.md");

/// The whole server for these settings, calling the real services.
pub fn app(config: Config) -> Router {
    let mut services = Services::default();
    match HttpFacilitator::new(&config.facilitator) {
        Ok(client) => services.facilitator = Some(Arc::new(client)),
        // Paid routes refuse rather than run unpaid.
        Err(error) => tracing::error!(%error, "cannot build the facilitator client"),
    }
    if let Some(key) = &config.openai_key {
        match OpenAi::new(key.expose()) {
            Ok(client) => services.pictures = Some(Arc::new(client)),
            Err(error) => tracing::error!(%error, "cannot build the OpenAI client"),
        }
    }
    app_with(config, services)
}

/// The whole server, calling `services`.
pub fn app_with(config: Config, services: Services) -> Router {
    layered(
        routes_with(config, services),
        Arc::new(IdempotencyStore::default()),
    )
}

/// The routes with no outside services, before the shared layers.
pub fn routes(config: Config) -> Router {
    routes_with(config, Services::default())
}

/// The routes, before the shared layers.
pub fn routes_with(config: Config, services: Services) -> Router {
    let gate = match (services.facilitator, &config.pay_to) {
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
        pictures: services.pictures,
    };
    Router::new()
        .route("/health", get(health))
        .route("/v1/guide", get(guide))
        .route("/v1/estimate", post(files::estimate_route))
        .route("/v1/edit", post(files::edit_route))
        .route("/v1/preview", post(files::preview_route))
        .route("/v1/listen", post(files::listen_route))
        .route("/v1/render", post(files::render_route))
        .route("/v1/images/generate", post(images::generate_route))
        .route("/v1/images/edit", post(images::edit_route))
        .with_state(state)
}

/// Takes payment for `quote`, running `work` once the payment verifies.
pub(crate) async fn take_payment<W, Fut>(
    state: &AppState,
    headers: &HeaderMap,
    quote: &Quote,
    work: W,
) -> Response
where
    W: FnOnce() -> Fut,
    Fut: Future<Output = Response>,
{
    let Some(gate) = &state.gate else {
        return Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "payments_not_configured",
            "this server has no UNBAKED_API_PAY_TO, so it cannot take payment",
        )
        .into_response();
    };
    gate.charge(headers, quote, work).await
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
        "pictures": state.pictures.is_some(),
    }))
}

async fn guide() -> impl IntoResponse {
    ([(CONTENT_TYPE, "text/markdown; charset=utf-8")], GUIDE)
}
