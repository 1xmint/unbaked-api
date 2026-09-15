//! The x402 version 2 messages, as they cross the wire.
//!
//! Written out here rather than taken from `x402-types`, which brings a
//! blockchain maths crate and a generic facilitator framework for five small
//! structs. The field names match x402-rs 2.0.2 (`x402-types/src/proto/v2.rs`),
//! which is what the MCP tool pays with.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol version spoken.
pub const VERSION: u8 = 2;

/// What one payment must be: the terms a facilitator checks a signed payment
/// against. Always built by the server from its own price, never taken from
/// the payer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Requirements {
    pub scheme: String,
    /// CAIP-2 network, e.g. `eip155:84532`.
    pub network: String,
    /// In the asset's smallest unit; for USDC, millionths of a dollar.
    pub amount: String,
    pub pay_to: String,
    pub max_timeout_seconds: u64,
    pub asset: String,
    pub extra: Option<Value>,
}

/// What is being paid for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// The `PAYMENT-REQUIRED` header's contents.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequired<'a> {
    pub x402_version: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'a str>,
    pub resource: &'a Resource,
    pub accepts: [&'a Requirements; 1],
}

/// The parts of a `PAYMENT-SIGNATURE` payload the gate reads itself. The whole
/// payload goes to the facilitator unchanged, because the signature covers it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    pub x402_version: u8,
    /// The terms the payer says they signed for.
    pub accepted: Requirements,
    /// The scheme's signed part: for `exact` on EVM, the transfer authorisation
    /// and its signature.
    pub payload: Value,
}

/// A facilitator's `/verify` answer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyAnswer {
    pub is_valid: bool,
    #[serde(default)]
    pub payer: Option<String>,
    #[serde(default)]
    pub invalid_reason: Option<String>,
}

/// A facilitator's `/settle` answer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleAnswer {
    pub success: bool,
    #[serde(default)]
    pub error_reason: Option<String>,
    #[serde(default)]
    pub transaction: Option<String>,
}

/// Base64 (standard alphabet, padded), as x402 headers use.
pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Reads a base64 JSON object header. `None` for anything else.
pub fn decode_object(header: &str) -> Option<Value> {
    let bytes = STANDARD.decode(header.trim()).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn requirements() -> Requirements {
        Requirements {
            scheme: "exact".to_owned(),
            network: "eip155:84532".to_owned(),
            amount: "5000".to_owned(),
            pay_to: "0x00000000000000000000000000000000000000bb".to_owned(),
            max_timeout_seconds: 600,
            asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".to_owned(),
            extra: Some(json!({"name": "USDC", "version": "2"})),
        }
    }

    #[test]
    fn payment_required_uses_the_x402_rs_field_names() {
        let resource = Resource {
            url: "/v1/speech".to_owned(),
            description: None,
            mime_type: Some("audio/mpeg".to_owned()),
        };
        let requirements = requirements();
        let message = PaymentRequired {
            x402_version: VERSION,
            error: None,
            resource: &resource,
            accepts: [&requirements],
        };
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({
                "x402Version": 2,
                "resource": {"url": "/v1/speech", "mimeType": "audio/mpeg"},
                "accepts": [{
                    "scheme": "exact",
                    "network": "eip155:84532",
                    "amount": "5000",
                    "payTo": "0x00000000000000000000000000000000000000bb",
                    "maxTimeoutSeconds": 600,
                    "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
                    "extra": {"name": "USDC", "version": "2"},
                }],
            })
        );
    }

    #[test]
    fn a_payload_in_the_x402_rs_shape_reads() {
        let sent = json!({
            "x402Version": 2,
            "accepted": serde_json::to_value(requirements()).unwrap(),
            "payload": {"signature": "0xabc", "authorization": {"value": "5000"}},
            "resource": {"url": "/v1/speech"},
        });
        let header = encode(sent.to_string().as_bytes());
        let value = decode_object(&header).unwrap();
        let payload: Payload = serde_json::from_value(value).unwrap();
        assert_eq!(payload.x402_version, 2);
        assert_eq!(payload.accepted, requirements());
        assert_eq!(payload.payload["signature"], "0xabc");
    }

    #[test]
    fn only_a_base64_json_object_decodes() {
        assert!(decode_object("").is_none());
        assert!(decode_object("!!!").is_none());
        assert!(decode_object(&encode(b"hello")).is_none());
        assert!(decode_object(&encode(b"[1,2]")).is_none());
        assert!(decode_object(&encode(b"{}")).is_some());
    }
}
