//! The file endpoints, paid through the fake facilitator.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use serde_json::Value;
use tower::ServiceExt;
use unbaked_api::config::Config;
use unbaked_api::{Services, app_with};
use unbaked_core::pack;
use unbaked_core::package::Limits;
use unbaked_pay::fake::FakeFacilitator;
use unbaked_pay::gate::{PAYMENT_REQUIRED, PAYMENT_RESPONSE, PAYMENT_SIGNATURE};
use unbaked_pay::{Requirements, wire};

const PICTURE: &[u8] = include_bytes!("fixtures/tiny.unbaked.png");
const SOUND: &[u8] = include_bytes!("fixtures/tiny.unbaked.m4a");
const PAY_TO: &str = "0x00000000000000000000000000000000000000bb";
const PAYER: &str = "0x00000000000000000000000000000000000000aa";
const BOUNDARY: &str = "unbaked-test-boundary";

/// Each payment gets its own nonce, so no two are the same authorisation.
static NONCE: AtomicU64 = AtomicU64::new(1);

fn server(pay_to: Option<&str>) -> (Router, Arc<FakeFacilitator>) {
    let config = Config::from_lookup(|name| match name {
        "UNBAKED_API_PAY_TO" => pay_to.map(str::to_owned),
        _ => None,
    })
    .unwrap();
    let fake = Arc::new(FakeFacilitator::new());
    let services = Services {
        facilitator: Some(fake.clone()),
        ..Services::default()
    };
    (app_with(config, services), fake)
}

/// A request to `uri`; `content_type` and `body` are reused for the paid retry.
#[derive(Clone)]
struct Call {
    uri: &'static str,
    content_type: &'static str,
    body: Bytes,
}

impl Call {
    fn file(uri: &'static str, body: &[u8]) -> Self {
        Self {
            uri,
            content_type: "application/octet-stream",
            body: Bytes::copy_from_slice(body),
        }
    }

    fn multipart(parts: &[(&str, &[u8])]) -> Self {
        let mut body = Vec::new();
        for (name, data) in parts {
            body.extend_from_slice(
                format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(data);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        Self {
            uri: "/v1/edit",
            content_type: "multipart/form-data; boundary=unbaked-test-boundary",
            body: body.into(),
        }
    }

    async fn send(&self, app: &Router, payment: Option<&str>) -> Response {
        let mut request = Request::post(self.uri).header(header::CONTENT_TYPE, self.content_type);
        if let Some(payment) = payment {
            request = request.header(PAYMENT_SIGNATURE, payment);
        }
        let request = request.body(Body::from(self.body.clone())).unwrap();
        app.clone().oneshot(request).await.unwrap()
    }

    /// Sends unpaid, reads the price, and sends again paying exactly that.
    async fn paid(&self, app: &Router) -> (Requirements, Response) {
        let asked = self.send(app, None).await;
        assert_eq!(asked.status(), StatusCode::PAYMENT_REQUIRED, "{}", self.uri);
        let required = asked.headers()[PAYMENT_REQUIRED].to_str().unwrap();
        let terms = wire::decode_object(required).unwrap()["accepts"][0].clone();
        let terms: Requirements = serde_json::from_value(terms).unwrap();
        let payment = FakeFacilitator::payment(PAYER, &terms, NONCE.fetch_add(1, Ordering::SeqCst));
        (terms, self.send(app, Some(&payment)).await)
    }
}

async fn bytes_of(response: Response) -> Bytes {
    to_bytes(response.into_body(), usize::MAX).await.unwrap()
}

async fn json_of(response: Response) -> Value {
    serde_json::from_slice(&bytes_of(response).await).unwrap()
}

#[tokio::test]
async fn estimate_is_free_and_states_the_prices() {
    let (app, fake) = server(Some(PAY_TO));
    let response = Call::file("/v1/estimate", PICTURE).send(&app, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let size = json_of(response).await;
    assert_eq!(size["kind"], "image");
    assert!(size["prices"]["render"].as_u64().unwrap() >= unbaked_api::prices::FLOOR);
    assert_eq!(fake.verifies(), 0);
}

#[tokio::test]
async fn render_charges_the_estimated_price_and_returns_a_fresh_file() {
    let (app, fake) = server(Some(PAY_TO));
    let size = json_of(Call::file("/v1/estimate", PICTURE).send(&app, None).await).await;

    let (terms, response) = Call::file("/v1/render", PICTURE).paid(&app).await;
    assert_eq!(terms.amount, size["prices"]["render"].to_string());
    assert_eq!(terms.pay_to, PAY_TO);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert!(response.headers().contains_key(PAYMENT_RESPONSE));
    let file = bytes_of(response).await;
    let files = pack::read_files(&file, Limits::default()).unwrap();
    assert!(files.contains_key("bake.json"));
    assert_eq!(fake.settles(), 1);
}

#[tokio::test]
async fn preview_returns_a_png_and_listen_returns_numbers() {
    let (app, fake) = server(Some(PAY_TO));

    let (_, response) = Call::file("/v1/preview", PICTURE).paid(&app).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(bytes_of(response).await.starts_with(b"\x89PNG\r\n\x1a\n"));

    let (_, response) = Call::file("/v1/listen", SOUND).paid(&app).await;
    assert_eq!(response.status(), StatusCode::OK);
    let stats = json_of(response).await;
    assert!(stats["duration_ms"].as_u64().unwrap() > 0);
    assert!(stats["loudness_dbfs"].is_array());
    assert_eq!(fake.settles(), 2);
}

#[tokio::test]
async fn edit_adds_an_asset_to_the_file() {
    let (app, fake) = server(Some(PAY_TO));
    let call = Call::multipart(&[("file", PICTURE), ("asset.extra", PICTURE)]);
    let (terms, response) = call.paid(&app).await;
    assert_eq!(terms.amount, unbaked_api::prices::EDIT.to_string());
    assert_eq!(response.status(), StatusCode::OK);
    let files = pack::read_files(&bytes_of(response).await, Limits::default()).unwrap();
    assert!(files.contains_key("assets/extra.png"), "{:?}", files.keys());
    assert_eq!(fake.settles(), 1);
}

#[tokio::test]
async fn failed_work_is_not_charged() {
    let (app, fake) = server(Some(PAY_TO));

    // A sound has no picture: the payment verifies, the preview fails.
    let (_, response) = Call::file("/v1/preview", SOUND).paid(&app).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_of(response).await["code"], "unsupported");

    let call = Call::multipart(&[("file", PICTURE), ("patch", b"not json")]);
    let (_, response) = call.paid(&app).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_of(response).await["code"], "invalid_patch");

    assert_eq!((fake.verifies(), fake.settles()), (2, 0));
}

#[tokio::test]
async fn a_bad_request_is_refused_before_any_price() {
    let (app, fake) = server(Some(PAY_TO));

    let response = Call::file("/v1/render", b"not a file")
        .send(&app, None)
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!response.headers().contains_key(PAYMENT_REQUIRED));
    assert_eq!(json_of(response).await["code"], "format");

    let call = Call::multipart(&[("file", PICTURE)]);
    assert_eq!(
        call.send(&app, None).await.status(),
        StatusCode::BAD_REQUEST
    );

    let mut preview = Call::file("/v1/preview", PICTURE);
    preview.uri = "/v1/preview?max_edge=0";
    assert_eq!(
        preview.send(&app, None).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(fake.verifies(), 0);
}

#[tokio::test]
async fn without_a_receiving_address_paid_routes_refuse() {
    let (app, _) = server(None);
    let response = Call::file("/v1/render", PICTURE).send(&app, None).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_of(response).await["code"], "payments_not_configured");

    let response = Call::file("/v1/estimate", PICTURE).send(&app, None).await;
    assert_eq!(response.status(), StatusCode::OK);
}
