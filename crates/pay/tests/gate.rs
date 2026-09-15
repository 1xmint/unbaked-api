//! The payment gate against the fake facilitator.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::to_bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use tokio::sync::Notify;
use unbaked_pay::fake::FakeFacilitator;
use unbaked_pay::gate::{PAYMENT_REQUIRED, PAYMENT_RESPONSE, PAYMENT_SIGNATURE};
use unbaked_pay::{Gate, Quote, Rejected, Resource, Terms, wire};

const PAY_TO: &str = "0x00000000000000000000000000000000000000bb";
const PAYER: &str = "0x00000000000000000000000000000000000000aa";

type TestGate = Gate<Arc<FakeFacilitator>>;

fn gate(daily_cap: u64) -> (Arc<TestGate>, Arc<FakeFacilitator>) {
    let fake = Arc::new(FakeFacilitator::new());
    let gate = Gate::new(fake.clone(), Terms::base_sepolia(PAY_TO, daily_cap));
    (Arc::new(gate), fake)
}

fn quote(amount: u64) -> Quote {
    Quote {
        amount,
        cost: 10,
        resource: Resource {
            url: "/v1/speech".to_owned(),
            description: Some("speech".to_owned()),
            mime_type: Some("audio/mpeg".to_owned()),
        },
    }
}

/// A payment from `payer` signed for `amount`.
fn paid(gate: &TestGate, payer: &str, amount: u64, nonce: u64) -> HeaderMap {
    let value = FakeFacilitator::payment(payer, &gate.requirements(amount), nonce);
    let mut headers = HeaderMap::new();
    headers.insert(PAYMENT_SIGNATURE, HeaderValue::from_str(&value).unwrap());
    headers
}

/// Work that counts its runs and answers `status`.
async fn work(runs: &AtomicUsize, status: StatusCode) -> Response {
    runs.fetch_add(1, Ordering::SeqCst);
    (status, "the result").into_response()
}

async fn problem_code(response: Response) -> String {
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    json["code"].as_str().unwrap().to_owned()
}

fn header_json(response: &Response, name: &str) -> Value {
    let header = response.headers().get(name).unwrap().to_str().unwrap();
    wire::decode_object(header).unwrap()
}

#[tokio::test]
async fn no_payment_gets_402_priced_for_this_request() {
    let (gate, fake) = gate(1_000);
    let runs = AtomicUsize::new(0);

    let response = gate
        .charge(&HeaderMap::new(), &quote(12_345), || {
            work(&runs, StatusCode::OK)
        })
        .await;

    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    let required = header_json(&response, PAYMENT_REQUIRED);
    assert_eq!(required["x402Version"], 2);
    assert_eq!(required["resource"]["url"], "/v1/speech");
    let terms = &required["accepts"][0];
    assert_eq!(terms["scheme"], "exact");
    assert_eq!(terms["network"], "eip155:84532");
    assert_eq!(terms["amount"], "12345");
    assert_eq!(terms["payTo"], PAY_TO);
    assert_eq!(terms["asset"], unbaked_pay::gate::BASE_SEPOLIA_USDC);
    assert_eq!(terms["extra"]["name"], "USDC");
    assert_eq!(problem_code(response).await, "payment_required");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(fake.verifies(), 0);
}

#[tokio::test]
async fn a_paid_request_runs_once_settles_and_returns_the_receipt() {
    let (gate, fake) = gate(1_000);
    let runs = AtomicUsize::new(0);

    let response = gate
        .charge(&paid(&gate, PAYER, 500, 1), &quote(500), || {
            work(&runs, StatusCode::OK)
        })
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let receipt = header_json(&response, PAYMENT_RESPONSE);
    assert_eq!(receipt["success"], true);
    assert_eq!(receipt["network"], "eip155:84532");
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    assert_eq!(&body[..], b"the result");
    assert_eq!((runs.load(Ordering::SeqCst), fake.settles()), (1, 1));
}

#[tokio::test]
async fn a_payment_for_a_cheaper_request_does_not_buy_this_one() {
    let (gate, fake) = gate(1_000);
    let runs = AtomicUsize::new(0);

    let response = gate
        .charge(&paid(&gate, PAYER, 100, 1), &quote(5_000), || {
            work(&runs, StatusCode::OK)
        })
        .await;

    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        header_json(&response, PAYMENT_REQUIRED)["accepts"][0]["amount"],
        "5000"
    );
    assert_eq!(problem_code(response).await, "payment_mismatch");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(fake.verifies(), 0);
}

#[tokio::test]
async fn failed_work_is_not_settled() {
    let (gate, fake) = gate(1_000);
    let runs = AtomicUsize::new(0);

    let response = gate
        .charge(&paid(&gate, PAYER, 500, 1), &quote(500), || {
            work(&runs, StatusCode::INTERNAL_SERVER_ERROR)
        })
        .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(response.headers().get(PAYMENT_RESPONSE).is_none());
    assert_eq!((runs.load(Ordering::SeqCst), fake.settles()), (1, 0));

    // The authorisation was never spent, so the payer can retry with it.
    let retry = gate
        .charge(&paid(&gate, PAYER, 500, 1), &quote(500), || {
            work(&runs, StatusCode::OK)
        })
        .await;
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(fake.settles(), 1);
}

#[tokio::test]
async fn a_failed_settlement_withholds_the_result_and_refuses_the_payer() {
    let (gate, fake) = gate(1_000);
    let runs = AtomicUsize::new(0);
    fake.refuse_settle(Rejected::NotSettled("insufficient_funds".to_owned()));

    let response = gate
        .charge(&paid(&gate, PAYER, 500, 1), &quote(500), || {
            work(&runs, StatusCode::OK)
        })
        .await;
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    assert!(response.headers().get(PAYMENT_RESPONSE).is_none());
    assert_eq!(problem_code(response).await, "payment_not_settled");
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // A fresh authorisation from the same address, any letter case, is refused
    // before the work runs.
    let again = gate
        .charge(
            &paid(
                &gate,
                &PAYER.to_ascii_uppercase().replace("0X", "0x"),
                500,
                2,
            ),
            &quote(500),
            || work(&runs, StatusCode::OK),
        )
        .await;
    assert_eq!(again.status(), StatusCode::FORBIDDEN);
    assert_eq!(problem_code(again).await, "payer_refused");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn one_authorisation_used_twice_at_once_runs_once() {
    let (gate, fake) = gate(1_000);
    let runs = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());

    let first = tokio::spawn({
        let (gate, runs, release, started) =
            (gate.clone(), runs.clone(), release.clone(), started.clone());
        async move {
            let headers = paid(&gate, PAYER, 500, 1);
            gate.charge(&headers, &quote(500), || async move {
                runs.fetch_add(1, Ordering::SeqCst);
                started.notify_one();
                release.notified().await;
                (StatusCode::OK, "the result").into_response()
            })
            .await
        }
    });
    started.notified().await;

    let second = gate
        .charge(&paid(&gate, PAYER, 500, 1), &quote(500), || {
            work(&runs, StatusCode::OK)
        })
        .await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(second).await, "payment_in_use");

    release.notify_one();
    let first = first.await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert!(first.headers().get(PAYMENT_RESPONSE).is_some());

    // Once settled, it can never run again.
    let third = gate
        .charge(&paid(&gate, PAYER, 500, 1), &quote(500), || {
            work(&runs, StatusCode::OK)
        })
        .await;
    assert_eq!(problem_code(third).await, "payment_spent");
    assert_eq!((runs.load(Ordering::SeqCst), fake.settles()), (1, 1));
}

#[tokio::test]
async fn the_daily_cap_stops_work_before_it_starts() {
    // Each job costs 10; a cap of 15 allows one.
    let (gate, fake) = gate(15);
    let runs = AtomicUsize::new(0);

    let first = gate
        .charge(&paid(&gate, PAYER, 500, 1), &quote(500), || {
            work(&runs, StatusCode::OK)
        })
        .await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = gate
        .charge(&paid(&gate, PAYER, 500, 2), &quote(500), || {
            work(&runs, StatusCode::OK)
        })
        .await;
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(problem_code(second).await, "daily_cap_reached");
    assert_eq!((runs.load(Ordering::SeqCst), fake.settles()), (1, 1));
}

#[tokio::test]
async fn a_refused_or_broken_payment_never_runs_the_work() {
    let (gate, fake) = gate(1_000);
    let runs = AtomicUsize::new(0);

    let mut garbage = HeaderMap::new();
    garbage.insert(PAYMENT_SIGNATURE, HeaderValue::from_static("not-a-payment"));
    let response = gate
        .charge(&garbage, &quote(500), || work(&runs, StatusCode::OK))
        .await;
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(problem_code(response).await, "payment_malformed");

    fake.refuse_verify(Rejected::Invalid("invalid_signature".to_owned()));
    let response = gate
        .charge(&paid(&gate, PAYER, 500, 1), &quote(500), || {
            work(&runs, StatusCode::OK)
        })
        .await;
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(problem_code(response).await, "payment_invalid");

    fake.refuse_verify(Rejected::Unreachable("timed out".to_owned()));
    let response = gate
        .charge(&paid(&gate, PAYER, 500, 2), &quote(500), || {
            work(&runs, StatusCode::OK)
        })
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(problem_code(response).await, "facilitator_unreachable");

    assert_eq!((runs.load(Ordering::SeqCst), fake.settles()), (0, 0));
}
