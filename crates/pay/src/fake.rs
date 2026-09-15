//! A facilitator that answers from memory, for tests.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use crate::facilitator::{BoxFuture, Facilitator, Rejected, Settled, Verified};
use crate::wire::{self, Requirements, VERSION};

/// Approves any payment signed for the required amount, unless told to refuse.
#[derive(Debug, Default)]
pub struct FakeFacilitator {
    verify_refusal: Mutex<Option<Rejected>>,
    settle_refusal: Mutex<Option<Rejected>>,
    verifies: AtomicUsize,
    settles: AtomicUsize,
}

impl FakeFacilitator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every later `/verify` answers with this refusal.
    pub fn refuse_verify(&self, refusal: Rejected) {
        *self.verify_refusal.lock().unwrap() = Some(refusal);
    }

    /// Every later `/settle` answers with this refusal.
    pub fn refuse_settle(&self, refusal: Rejected) {
        *self.settle_refusal.lock().unwrap() = Some(refusal);
    }

    /// How many times `/verify` was asked.
    pub fn verifies(&self) -> usize {
        self.verifies.load(Ordering::SeqCst)
    }

    /// How many times `/settle` was asked.
    pub fn settles(&self) -> usize {
        self.settles.load(Ordering::SeqCst)
    }

    /// A `PAYMENT-SIGNATURE` value from `payer` for these terms. A different
    /// `nonce` makes a different authorisation.
    pub fn payment(payer: &str, requirements: &Requirements, nonce: u64) -> String {
        let payload = json!({
            "x402Version": VERSION,
            "accepted": requirements,
            "payload": {
                "signature": format!("0xfake{nonce}"),
                "authorization": {
                    "from": payer,
                    "to": requirements.pay_to,
                    "value": requirements.amount,
                    "nonce": nonce.to_string(),
                },
            },
        });
        wire::encode(payload.to_string().as_bytes())
    }
}

fn payer(payload: &Value) -> String {
    payload["payload"]["authorization"]["from"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

impl Facilitator for FakeFacilitator {
    fn verify<'a>(
        &'a self,
        payload: &'a Value,
        requirements: &'a Requirements,
    ) -> BoxFuture<'a, Result<Verified, Rejected>> {
        self.verifies.fetch_add(1, Ordering::SeqCst);
        let answer = match self.verify_refusal.lock().unwrap().clone() {
            Some(refusal) => Err(refusal),
            // Like a real facilitator, check the signed value, not the claim.
            None if payload["payload"]["authorization"]["value"]
                != requirements.amount.as_str() =>
            {
                Err(Rejected::Invalid(
                    "invalid_exact_evm_payload_value".to_owned(),
                ))
            }
            None => Ok(Verified {
                payer: payer(payload),
            }),
        };
        Box::pin(async move { answer })
    }

    fn settle<'a>(
        &'a self,
        payload: &'a Value,
        requirements: &'a Requirements,
    ) -> BoxFuture<'a, Result<Settled, Rejected>> {
        let count = self.settles.fetch_add(1, Ordering::SeqCst) + 1;
        let answer = match self.settle_refusal.lock().unwrap().clone() {
            Some(refusal) => Err(refusal),
            None => {
                let transaction = format!("0x{count:064x}");
                Ok(Settled {
                    receipt: json!({
                        "success": true,
                        "transaction": transaction,
                        "network": requirements.network,
                        "payer": payer(payload),
                    }),
                    transaction,
                })
            }
        };
        Box::pin(async move { answer })
    }
}
