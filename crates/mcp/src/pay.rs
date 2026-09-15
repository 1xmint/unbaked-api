//! Paying the `unbaked-api` server with x402, kept inside a session budget.
//!
//! The flow: send the request unpaid; if it is not a 402, that is the answer.
//! Otherwise read `PAYMENT-REQUIRED` ourselves, refuse a network we will not
//! pay on, reserve the price from the session budget before signing anything,
//! sign and resend. A 2xx spends the reservation; an error answer releases it
//! (the server settles only on success); no answer at all spends it.

use std::sync::Arc;

use alloy_primitives::U256;
use alloy_signer_local::PrivateKeySigner;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response};
use serde_json::Value;
use x402_chain_eip155::V2Eip155ExactClient;
use x402_reqwest::X402Client;
use x402_types::scheme::client::{PaymentCandidate, PaymentSelector};

use crate::budget::Budget;
use crate::config::dollars;

/// The only network this tool will pay on. Real money is out of scope for
/// this tool; see PR 7.
pub const NETWORK: &str = "eip155:84532";
pub const SCHEME: &str = "exact";

pub struct Payer {
    http: Client,
    wallet: Option<Arc<PrivateKeySigner>>,
    budget: Budget,
    api_url: String,
}

/// What a paid call ended up doing.
pub enum Paid {
    /// The route did not ask for payment: the free answer.
    Free(Response),
    /// Paid and succeeded: the answer, what it cost, what is left, and the
    /// settlement transaction if the server reported one.
    Settled {
        response: Response,
        paid: u64,
        remaining: u64,
        transaction: Option<String>,
    },
    /// Paid, but the work failed after payment was verified. The reservation
    /// was released; nothing was spent.
    WorkFailed(Response),
}

#[derive(Debug)]
pub enum PayError {
    NoWallet,
    /// The price, what is left in the session, and the session cap, all in
    /// micro-USDC.
    OverBudget {
        price: u64,
        remaining: u64,
        cap: u64,
    },
    UnsupportedNetwork,
    Http(String),
    Sign(String),
}

impl PayError {
    pub fn message(&self) -> String {
        match self {
            Self::NoWallet => {
                "this tool has no UNBAKED_WALLET_KEY, so it cannot pay for this".to_owned()
            }
            Self::OverBudget {
                price,
                remaining,
                cap,
            } => format!(
                "this call costs ${}, but only ${} is left of the ${} session cap \
                 (UNBAKED_SESSION_CAP_USD); nothing was charged",
                dollars(*price),
                dollars(*remaining),
                dollars(*cap),
            ),
            Self::UnsupportedNetwork => {
                "this server asks for a network the tool will not pay on".to_owned()
            }
            Self::Http(detail) => format!("the request failed: {detail}"),
            Self::Sign(detail) => format!("signing the payment failed: {detail}"),
        }
    }
}

/// Only a candidate on our network, with this scheme, at or under the
/// reserved amount.
struct OnlyOurNetwork {
    max: U256,
}

impl PaymentSelector for OnlyOurNetwork {
    fn select<'a>(&self, candidates: &'a [PaymentCandidate]) -> Option<&'a PaymentCandidate> {
        candidates.iter().find(|c| {
            c.chain_id.to_string() == NETWORK && c.scheme == SCHEME && c.amount <= self.max
        })
    }
}

impl Payer {
    pub fn new(api_url: String, wallet: Option<Arc<PrivateKeySigner>>, budget: Budget) -> Self {
        Self {
            http: Client::new(),
            wallet,
            budget,
            api_url,
        }
    }

    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    pub fn wallet_address(&self) -> Option<String> {
        self.wallet.as_ref().map(|w| w.address().to_string())
    }

    /// A plain, unpaid GET.
    pub async fn get(&self, path: &str) -> Result<Response, PayError> {
        self.http
            .get(format!("{}{}", self.api_url, path))
            .send()
            .await
            .map_err(|error| PayError::Http(error.to_string()))
    }

    /// POSTs `body`, paying if the server asks for it.
    pub async fn post(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Paid, PayError> {
        let url = format!("{}{}", self.api_url, path);
        let first = self
            .http
            .post(&url)
            .header(CONTENT_TYPE, content_type)
            .body(body.clone())
            .send()
            .await
            .map_err(|error| PayError::Http(error.to_string()))?;

        if first.status().as_u16() != 402 {
            return Ok(Paid::Free(first));
        }

        let wallet = self.wallet.clone().ok_or(PayError::NoWallet)?;
        let amount = required_amount(&first)?;
        let reservation =
            self.budget
                .reserve(amount)
                .map_err(|remaining| PayError::OverBudget {
                    price: amount,
                    remaining,
                    cap: self.budget.cap(),
                })?;

        let client = X402Client::new()
            .register(V2Eip155ExactClient::new(wallet))
            .with_selector(OnlyOurNetwork {
                max: U256::from(reservation.amount()),
            });
        let headers = client
            .make_payment_headers(first)
            .await
            .map_err(|error| PayError::Sign(error.to_string()))?;

        let mut request = self
            .http
            .post(&url)
            .header(CONTENT_TYPE, content_type)
            .body(body);
        for (name, value) in headers.iter() {
            request = request.header(name, value);
        }
        let second = match request.send().await {
            Ok(second) => second,
            Err(error) => {
                // A signed payment went out. The server may have done the work
                // and settled before the connection dropped, so count it as
                // spent: the session cap must never be exceeded.
                reservation.spend();
                return Err(PayError::Http(format!(
                    "{error}; a signed payment for ${} was sent, so it counts \
                     against the session cap even though no answer came back",
                    dollars(amount)
                )));
            }
        };

        if second.status().is_success() {
            reservation.spend();
            let transaction = settlement_transaction(&second);
            Ok(Paid::Settled {
                remaining: self.budget.remaining(),
                paid: amount,
                transaction,
                response: second,
            })
        } else {
            // `reservation` drops here, releasing the amount back.
            Ok(Paid::WorkFailed(second))
        }
    }
}

/// Reads `PAYMENT-REQUIRED`, and the price for the `exact`/[`NETWORK`] entry.
fn required_amount(response: &Response) -> Result<u64, PayError> {
    let header = response
        .headers()
        .get("payment-required")
        .and_then(|value| value.to_str().ok())
        .ok_or(PayError::UnsupportedNetwork)?;
    let decoded: Value = STANDARD
        .decode(header.trim())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or(PayError::UnsupportedNetwork)?;
    let accepts = decoded["accepts"].as_array().cloned().unwrap_or_default();
    accepts
        .iter()
        .find(|entry| entry["scheme"] == SCHEME && entry["network"] == NETWORK)
        .and_then(|entry| entry["amount"].as_str())
        .and_then(|amount| amount.parse().ok())
        .ok_or(PayError::UnsupportedNetwork)
}

/// The settlement transaction from `PAYMENT-RESPONSE`, if present.
fn settlement_transaction(response: &Response) -> Option<String> {
    let header = response.headers().get("payment-response")?.to_str().ok()?;
    let decoded: Value = serde_json::from_slice(&STANDARD.decode(header.trim()).ok()?).ok()?;
    decoded["transaction"].as_str().map(str::to_owned)
}
