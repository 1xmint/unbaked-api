//! The gate in front of paid work.
//!
//! A handler reads its request, works out the price of *that* request, and
//! hands the gate a [`Quote`] and the work. The gate:
//!
//! 1. With no `PAYMENT-SIGNATURE`, answers 402 with `PAYMENT-REQUIRED` for
//!    this price. The work does not run.
//! 2. Refuses a payment whose terms differ from this request's price, so a
//!    payment made for a cheap request cannot buy an expensive one.
//! 3. Refuses an authorisation already running or already settled.
//! 4. Asks the facilitator to verify, then refuses a payer whose earlier
//!    settlement failed, and holds the day's provider spend under the cap.
//! 5. Runs the work. If it did not succeed, nothing is settled.
//! 6. Settles. Only then does the result go out, with `PAYMENT-RESPONSE`.
//!    If settlement fails the result is withheld.
//!
//! The cost of settling last: a payer who empties their wallet between verify
//! and settle gets one job's provider cost from us, once, and is refused after.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::facilitator::{Facilitator, Rejected};
use crate::wire::{self, Payload, PaymentRequired, Requirements, Resource, VERSION};

pub const PAYMENT_REQUIRED: &str = "payment-required";
pub const PAYMENT_SIGNATURE: &str = "payment-signature";
pub const PAYMENT_RESPONSE: &str = "payment-response";

/// How long a settled authorisation is remembered, so it cannot run again.
/// Far longer than any authorisation this gate asks for stays valid.
const SPENT_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// Base Sepolia's USDC contract.
pub const BASE_SEPOLIA_USDC: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";

/// Where and how payments are taken.
#[derive(Clone, Debug)]
pub struct Terms {
    /// CAIP-2 network, e.g. `eip155:84532`.
    pub network: String,
    /// The token contract.
    pub asset: String,
    /// The token's EIP-712 name and version, which the payer signs over.
    pub asset_name: String,
    pub asset_version: String,
    /// The receiving address.
    pub pay_to: String,
    /// How long a signed payment stays valid. It must outlast the slowest
    /// job, because settlement comes after the work.
    pub max_timeout_seconds: u64,
    /// The most provider cost, in millionths of a dollar, started in one UTC
    /// day. Counted when a verified job starts, whether or not it succeeds,
    /// because a provider bills failed calls too.
    pub daily_cap: u64,
}

impl Terms {
    /// USDC on Base Sepolia, the test network.
    pub fn base_sepolia(pay_to: &str, daily_cap: u64) -> Self {
        Self {
            network: "eip155:84532".to_owned(),
            asset: BASE_SEPOLIA_USDC.to_owned(),
            asset_name: "USDC".to_owned(),
            asset_version: "2".to_owned(),
            pay_to: pay_to.to_owned(),
            max_timeout_seconds: 600,
            daily_cap,
        }
    }
}

/// The price of one request.
#[derive(Clone, Debug)]
pub struct Quote {
    /// What the payer pays, in the asset's smallest unit (USDC: millionths of
    /// a dollar).
    pub amount: u64,
    /// What the work costs us at the provider, in millionths of a dollar.
    pub cost: u64,
    pub resource: Resource,
}

/// The payment gate.
pub struct Gate<F> {
    facilitator: F,
    terms: Terms,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Authorisations whose job is running now.
    running: HashSet<[u8; 32]>,
    /// Authorisations already settled, or whose settlement may have happened.
    spent: HashMap<[u8; 32], Instant>,
    /// Payers whose settlement failed, lowercased.
    refused: HashSet<String>,
    day: u64,
    day_cost: u64,
}

impl<F: Facilitator> Gate<F> {
    pub fn new(facilitator: F, terms: Terms) -> Self {
        Self {
            facilitator,
            terms,
            state: Mutex::new(State::default()),
        }
    }

    /// The terms a payment for `amount` must meet.
    pub fn requirements(&self, amount: u64) -> Requirements {
        Requirements {
            scheme: "exact".to_owned(),
            network: self.terms.network.clone(),
            amount: amount.to_string(),
            pay_to: self.terms.pay_to.clone(),
            max_timeout_seconds: self.terms.max_timeout_seconds,
            asset: self.terms.asset.clone(),
            extra: Some(json!({
                "name": self.terms.asset_name,
                "version": self.terms.asset_version,
            })),
        }
    }

    /// Takes payment for `work`, as described in the module notes. The work
    /// counts as succeeded when its response status is 2xx.
    pub async fn charge<W, Fut>(&self, headers: &HeaderMap, quote: &Quote, work: W) -> Response
    where
        W: FnOnce() -> Fut,
        Fut: Future<Output = Response>,
    {
        let requirements = self.requirements(quote.amount);
        let ask = |code: &str, detail: &str| payment_required(quote, &requirements, code, detail);

        let Some(header) = headers.get(PAYMENT_SIGNATURE) else {
            return ask("payment_required", "this request needs payment");
        };
        let Some(payload) = header.to_str().ok().and_then(wire::decode_object) else {
            return ask("payment_malformed", "PAYMENT-SIGNATURE is not base64 JSON");
        };
        let parsed: Payload = match serde_json::from_value(payload.clone()) {
            Ok(parsed) => parsed,
            Err(error) => return ask("payment_malformed", &error.to_string()),
        };
        if parsed.x402_version != VERSION {
            return ask("payment_malformed", "only x402 version 2 is accepted");
        }
        if !same_terms(&parsed.accepted, &requirements) {
            let detail = format!(
                "the payment is for {} of {} to {} on {}; this request costs {}",
                parsed.accepted.amount,
                parsed.accepted.asset,
                parsed.accepted.pay_to,
                parsed.accepted.network,
                requirements.amount,
            );
            return ask("payment_mismatch", &detail);
        }

        let key: [u8; 32] = Sha256::digest(parsed.payload.to_string().as_bytes()).into();
        let _claim = match self.claim(key) {
            Ok(claim) => claim,
            Err((status, code, detail)) => return problem(status, code, detail),
        };

        let verified = match self.facilitator.verify(&payload, &requirements).await {
            Ok(verified) => verified,
            Err(Rejected::Invalid(reason)) => return ask("payment_invalid", &reason),
            Err(rejected) => return facilitator_failed(rejected),
        };
        if let Err((status, code, detail)) = self.admit(&verified.payer, quote.cost) {
            return problem(status, code, detail);
        }

        let mut response = work().await;
        if !response.status().is_success() {
            return response;
        }

        match self.facilitator.settle(&payload, &requirements).await {
            Ok(settled) => {
                self.spend(key);
                tracing::info!(
                    transaction = %settled.transaction,
                    amount = quote.amount,
                    resource = %quote.resource.url,
                    "settled"
                );
                let receipt = wire::encode(settled.receipt.to_string().as_bytes());
                if let Ok(value) = HeaderValue::from_str(&receipt) {
                    response.headers_mut().insert(PAYMENT_RESPONSE, value);
                }
                response
            }
            Err(rejected) => {
                // The result is withheld whatever went wrong. The
                // authorisation may have settled even so, so it never runs
                // again; only a facilitator's explicit failure blames the payer.
                self.spend(key);
                tracing::warn!(payer = %verified.payer, ?rejected, "settlement failed");
                match rejected {
                    Rejected::NotSettled(reason) => {
                        self.refuse(&verified.payer);
                        ask("payment_not_settled", &reason)
                    }
                    other => facilitator_failed(other),
                }
            }
        }
    }

    /// Marks an authorisation as running, unless it is running or spent.
    fn claim(&self, key: [u8; 32]) -> Result<Claim<'_, F>, Refusal> {
        let mut state = self.lock();
        if state.spent.contains_key(&key) {
            return Err((
                StatusCode::PAYMENT_REQUIRED,
                "payment_spent",
                "this payment authorisation has already been used",
            ));
        }
        if !state.running.insert(key) {
            return Err((
                StatusCode::CONFLICT,
                "payment_in_use",
                "a job paid with this authorisation is still running",
            ));
        }
        Ok(Claim { gate: self, key })
    }

    /// Lets a verified payer's job start, counting its cost against the day.
    fn admit(&self, payer: &str, cost: u64) -> Result<(), Refusal> {
        let mut state = self.lock();
        if state.refused.contains(&payer.to_ascii_lowercase()) {
            return Err((
                StatusCode::FORBIDDEN,
                "payer_refused",
                "an earlier payment from this address did not settle",
            ));
        }
        let today = utc_day();
        if state.day != today {
            state.day = today;
            state.day_cost = 0;
        }
        let total = state.day_cost.saturating_add(cost);
        if total > self.terms.daily_cap {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "daily_cap_reached",
                "the server has reached today's spending limit; nothing was charged",
            ));
        }
        state.day_cost = total;
        Ok(())
    }

    fn spend(&self, key: [u8; 32]) {
        let mut state = self.lock();
        let now = Instant::now();
        state
            .spent
            .retain(|_, at| now.duration_since(*at) < SPENT_FOR);
        state.spent.insert(key, now);
    }

    fn refuse(&self, payer: &str) {
        self.lock().refused.insert(payer.to_ascii_lowercase());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Nothing panics while holding the lock, but if something did, the
        // sets are still usable.
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Why a job may not start: status, problem code and detail.
type Refusal = (StatusCode, &'static str, &'static str);

/// Frees a running authorisation when its job ends, however it ends.
struct Claim<'a, F> {
    gate: &'a Gate<F>,
    key: [u8; 32],
}

impl<F> Drop for Claim<'_, F> {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.running.remove(&self.key);
    }
}

/// Whether the payer signed for exactly these terms. Addresses compare
/// without case, since EVM addresses may carry a mixed-case checksum.
fn same_terms(accepted: &Requirements, required: &Requirements) -> bool {
    accepted.scheme == required.scheme
        && accepted.network == required.network
        && accepted.amount == required.amount
        && accepted.pay_to.eq_ignore_ascii_case(&required.pay_to)
        && accepted.asset.eq_ignore_ascii_case(&required.asset)
}

fn utc_day() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / 86_400)
}

/// 402 with this request's terms in `PAYMENT-REQUIRED`.
fn payment_required(
    quote: &Quote,
    requirements: &Requirements,
    code: &str,
    detail: &str,
) -> Response {
    let message = PaymentRequired {
        x402_version: VERSION,
        error: Some(detail),
        resource: &quote.resource,
        accepts: [requirements],
    };
    let mut response = problem(StatusCode::PAYMENT_REQUIRED, code, detail);
    let encoded = wire::encode(&serde_json::to_vec(&message).unwrap_or_default());
    if let Ok(value) = HeaderValue::from_str(&encoded) {
        response.headers_mut().insert(PAYMENT_REQUIRED, value);
    }
    response
}

fn facilitator_failed(rejected: Rejected) -> Response {
    let (code, detail) = match rejected {
        Rejected::Unreachable(detail) => ("facilitator_unreachable", detail),
        Rejected::Unreadable(detail) => ("facilitator_unreadable", detail),
        Rejected::Invalid(detail) => ("payment_invalid", detail),
        Rejected::NotSettled(detail) => ("payment_not_settled", detail),
    };
    problem(StatusCode::SERVICE_UNAVAILABLE, code, &detail)
}

/// An `application/problem+json` response, the same shape the server uses.
fn problem(status: StatusCode, code: &str, detail: &str) -> Response {
    let body: Value = json!({
        "type": "about:blank",
        "title": status.canonical_reason().unwrap_or("Error"),
        "status": status.as_u16(),
        "code": code,
        "detail": detail,
    });
    (
        status,
        [(CONTENT_TYPE, "application/problem+json")],
        body.to_string(),
    )
        .into_response()
}
