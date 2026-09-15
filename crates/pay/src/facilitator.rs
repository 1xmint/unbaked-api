//! Asking a facilitator whether a payment is good, and to settle it.
//!
//! The server never checks signatures or sends transactions itself: that would
//! put a funded key and a chain client inside the process serving requests.
//! Every failure here refuses. Only an explicit `isValid: true` or
//! `success: true` counts, because reading an answer we do not understand as a
//! yes would give the work away.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderMap};
use serde_json::{Value, json};

use crate::wire::{Requirements, SettleAnswer, VERSION, VerifyAnswer};

/// A boxed future, so a facilitator can sit behind `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How long `/verify` may take. It reads the chain but writes nothing.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long `/settle` may take. It waits for the transfer to be mined.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a payment was not accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// The facilitator could not be reached, or did not answer in time.
    Unreachable(String),
    /// The facilitator answered in a shape this does not understand.
    Unreadable(String),
    /// The facilitator said the payment is not good.
    Invalid(String),
    /// The facilitator could not settle the payment.
    NotSettled(String),
}

/// A payment the facilitator says is good.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The paying address.
    pub payer: String,
}

/// A settled payment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    pub transaction: String,
    /// The facilitator's whole answer, returned to the payer as the receipt.
    pub receipt: Value,
}

/// Something that verifies and settles x402 payments.
pub trait Facilitator: Send + Sync {
    fn verify<'a>(
        &'a self,
        payload: &'a Value,
        requirements: &'a Requirements,
    ) -> BoxFuture<'a, Result<Verified, Rejected>>;

    fn settle<'a>(
        &'a self,
        payload: &'a Value,
        requirements: &'a Requirements,
    ) -> BoxFuture<'a, Result<Settled, Rejected>>;
}

impl<T: Facilitator + ?Sized> Facilitator for Arc<T> {
    fn verify<'a>(
        &'a self,
        payload: &'a Value,
        requirements: &'a Requirements,
    ) -> BoxFuture<'a, Result<Verified, Rejected>> {
        (**self).verify(payload, requirements)
    }

    fn settle<'a>(
        &'a self,
        payload: &'a Value,
        requirements: &'a Requirements,
    ) -> BoxFuture<'a, Result<Settled, Rejected>> {
        (**self).settle(payload, requirements)
    }
}

/// A facilitator over HTTP: `POST {base}/verify` and `POST {base}/settle`.
#[derive(Clone, Debug)]
pub struct HttpFacilitator {
    client: reqwest::Client,
    base: String,
    headers: HeaderMap,
}

impl HttpFacilitator {
    /// A client for the facilitator at `base`, e.g. `https://x402.org/facilitator`.
    pub fn new(base: &str) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            base: base.trim_end_matches('/').to_owned(),
            headers: HeaderMap::new(),
        })
    }

    /// Headers sent with every call, e.g. a facilitator's login token.
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    fn endpoint(&self, op: &str) -> String {
        format!("{}/{op}", self.base)
    }

    async fn post(
        &self,
        op: &str,
        timeout: Duration,
        payload: &Value,
        requirements: &Requirements,
    ) -> Result<String, Rejected> {
        let body = json!({
            "x402Version": VERSION,
            "paymentPayload": payload,
            "paymentRequirements": requirements,
        });
        let response = self
            .client
            .post(self.endpoint(op))
            .headers(self.headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| Rejected::Unreachable(error.to_string()))?;
        // A refusal can come with a 4xx status and a readable body, so the
        // body is read whatever the status; an unreadable one still refuses.
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| Rejected::Unreachable(error.to_string()))?;
        if status.is_server_error() && serde_json::from_str::<Value>(&text).is_err() {
            return Err(Rejected::Unreachable(format!("{op} answered {status}")));
        }
        Ok(text)
    }
}

impl Facilitator for HttpFacilitator {
    fn verify<'a>(
        &'a self,
        payload: &'a Value,
        requirements: &'a Requirements,
    ) -> BoxFuture<'a, Result<Verified, Rejected>> {
        Box::pin(async move {
            let text = self
                .post("verify", VERIFY_TIMEOUT, payload, requirements)
                .await?;
            read_verify(&text)
        })
    }

    fn settle<'a>(
        &'a self,
        payload: &'a Value,
        requirements: &'a Requirements,
    ) -> BoxFuture<'a, Result<Settled, Rejected>> {
        Box::pin(async move {
            let text = self
                .post("settle", SETTLE_TIMEOUT, payload, requirements)
                .await?;
            read_settle(&text)
        })
    }
}

/// Reads a `/verify` answer. Anything but an explicit yes with a payer refuses.
pub fn read_verify(text: &str) -> Result<Verified, Rejected> {
    let answer: VerifyAnswer =
        serde_json::from_str(text).map_err(|error| Rejected::Unreadable(error.to_string()))?;
    if !answer.is_valid {
        return Err(Rejected::Invalid(
            answer
                .invalid_reason
                .unwrap_or_else(|| "no reason given".to_owned()),
        ));
    }
    match answer.payer.filter(|payer| !payer.trim().is_empty()) {
        Some(payer) => Ok(Verified { payer }),
        None => Err(Rejected::Unreadable("valid, but no payer".to_owned())),
    }
}

/// Reads a `/settle` answer. Anything but an explicit success with a
/// transaction refuses.
pub fn read_settle(text: &str) -> Result<Settled, Rejected> {
    let receipt: Value =
        serde_json::from_str(text).map_err(|error| Rejected::Unreadable(error.to_string()))?;
    let answer: SettleAnswer = serde_json::from_value(receipt.clone())
        .map_err(|error| Rejected::Unreadable(error.to_string()))?;
    if !answer.success {
        return Err(Rejected::NotSettled(
            answer
                .error_reason
                .unwrap_or_else(|| "no reason given".to_owned()),
        ));
    }
    match answer.transaction.filter(|tx| !tx.trim().is_empty()) {
        Some(transaction) => Ok(Settled {
            transaction,
            receipt,
        }),
        None => Err(Rejected::Unreadable(
            "settled, but no transaction".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requirements() -> Requirements {
        Requirements {
            scheme: "exact".to_owned(),
            network: "eip155:84532".to_owned(),
            amount: "1".to_owned(),
            pay_to: "0x00000000000000000000000000000000000000bb".to_owned(),
            max_timeout_seconds: 600,
            asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".to_owned(),
            extra: None,
        }
    }

    #[test]
    fn only_an_explicit_yes_with_a_payer_verifies() {
        assert_eq!(
            read_verify(r#"{"isValid":true,"payer":"0xaa"}"#),
            Ok(Verified {
                payer: "0xaa".to_owned()
            })
        );
        assert_eq!(
            read_verify(r#"{"isValid":false,"invalidReason":"insufficient_funds"}"#),
            Err(Rejected::Invalid("insufficient_funds".to_owned()))
        );
        for text in [
            "",
            "null",
            "{}",
            r#"{"isValid":"true","payer":"0xaa"}"#,
            r#"{"valid":true,"payer":"0xaa"}"#,
            r#"{"isValid":true}"#,
            "<html>502</html>",
        ] {
            assert!(
                matches!(read_verify(text), Err(Rejected::Unreadable(_))),
                "{text}"
            );
        }
    }

    #[test]
    fn only_an_explicit_success_with_a_transaction_settles() {
        let settled =
            read_settle(r#"{"success":true,"transaction":"0x12","network":"eip155:84532"}"#)
                .unwrap();
        assert_eq!(settled.transaction, "0x12");
        assert_eq!(settled.receipt["network"], "eip155:84532");
        assert_eq!(
            read_settle(r#"{"success":false,"errorReason":"expired"}"#),
            Err(Rejected::NotSettled("expired".to_owned()))
        );
        for text in ["", "{}", r#"{"success":true}"#, r#"{"success":"yes"}"#] {
            assert!(
                matches!(read_settle(text), Err(Rejected::Unreadable(_))),
                "{text}"
            );
        }
    }

    #[test]
    fn a_trailing_slash_on_the_base_is_dropped() {
        let facilitator = HttpFacilitator::new("https://x402.org/facilitator/").unwrap();
        assert_eq!(
            facilitator.endpoint("verify"),
            "https://x402.org/facilitator/verify"
        );
    }

    #[tokio::test]
    async fn an_unreachable_facilitator_refuses() {
        let facilitator = HttpFacilitator::new("http://127.0.0.1:1").unwrap();
        let error = facilitator
            .verify(&json!({}), &requirements())
            .await
            .unwrap_err();
        assert!(matches!(error, Rejected::Unreachable(_)), "{error:?}");
    }
}
