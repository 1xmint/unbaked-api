//! The picture endpoints, with a fake provider and a fake facilitator.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use serde_json::{Value, json};
use tower::ServiceExt;
use unbaked_api::config::Config;
use unbaked_api::prices::{picture_cost, with_margin};
use unbaked_api::providers::fake::{FakePictures, TINY_PNG};
use unbaked_api::providers::{PictureFormat, ProviderError, Quality};
use unbaked_api::{Services, app_with};
use unbaked_pay::fake::FakeFacilitator;
use unbaked_pay::gate::{PAYMENT_REQUIRED, PAYMENT_SIGNATURE};
use unbaked_pay::{Requirements, wire};

const PAY_TO: &str = "0x00000000000000000000000000000000000000bb";
const PAYER: &str = "0x00000000000000000000000000000000000000aa";
const BOUNDARY: &str = "unbaked-test-boundary";

static NONCE: AtomicU64 = AtomicU64::new(1);

struct Server {
    app: Router,
    payments: Arc<FakeFacilitator>,
    pictures: Arc<FakePictures>,
}

fn server(daily_cap: Option<&str>) -> Server {
    let config = Config::from_lookup(|name| match name {
        "UNBAKED_API_PAY_TO" => Some(PAY_TO.to_owned()),
        "UNBAKED_API_DAILY_CAP_USD" => daily_cap.map(str::to_owned),
        _ => None,
    })
    .unwrap();
    let payments = Arc::new(FakeFacilitator::new());
    let pictures = Arc::new(FakePictures::new());
    let services = Services {
        facilitator: Some(payments.clone()),
        pictures: Some(pictures.clone()),
    };
    Server {
        app: app_with(config, services),
        payments,
        pictures,
    }
}

#[derive(Clone)]
struct Call {
    uri: &'static str,
    content_type: String,
    body: Bytes,
}

impl Call {
    fn generate(request: Value) -> Self {
        Self {
            uri: "/v1/images/generate",
            content_type: "application/json".to_owned(),
            body: request.to_string().into(),
        }
    }

    fn edit(parts: &[(&str, &[u8])]) -> Self {
        let mut body = Vec::new();
        for (name, data) in parts {
            body.extend_from_slice(
                format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                    .as_bytes(),
            );
            body.extend_from_slice(data);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        Self {
            uri: "/v1/images/edit",
            content_type: format!("multipart/form-data; boundary={BOUNDARY}"),
            body: body.into(),
        }
    }

    async fn send(&self, app: &Router, payment: Option<&str>) -> Response {
        let mut request = Request::post(self.uri).header(header::CONTENT_TYPE, &self.content_type);
        if let Some(payment) = payment {
            request = request.header(PAYMENT_SIGNATURE, payment);
        }
        let request = request.body(Body::from(self.body.clone())).unwrap();
        app.clone().oneshot(request).await.unwrap()
    }

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

async fn json_of(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn generate_charges_by_size_and_quality_and_returns_the_picture() {
    let server = server(None);
    let call = Call::generate(json!({
        "prompt": "a pear on a blue table",
        "size": "1536x1024",
        "quality": "high",
    }));

    let (terms, response) = call.paid(&server.app).await;
    let cost = picture_cost(Quality::High, 1536, 1024, 22, 0);
    assert_eq!(terms.amount, with_margin(cost).to_string());
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], TINY_PNG);

    let jobs = server.pictures.jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!((jobs[0].width, jobs[0].height), (1536, 1024));
    assert_eq!(jobs[0].quality, Quality::High);
    assert_eq!(jobs[0].format, PictureFormat::Png);
    assert_eq!(server.payments.settles(), 1);
}

#[tokio::test]
async fn edit_sends_every_source_and_the_mask() {
    let server = server(None);
    let call = Call::edit(&[
        ("prompt", b"make it night"),
        ("image", TINY_PNG),
        ("image", TINY_PNG),
        ("mask", TINY_PNG),
        ("quality", b"low"),
    ]);

    let (terms, response) = call.paid(&server.app).await;
    let cost = picture_cost(Quality::Low, 1024, 1024, 13, 2);
    assert_eq!(terms.amount, with_margin(cost).to_string());
    assert_eq!(response.status(), StatusCode::OK);

    let edits = server.pictures.edits();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].images.len(), 2);
    assert!(edits[0].mask.is_some());
    assert_eq!(edits[0].job.prompt, "make it night");
    assert_eq!(server.payments.settles(), 1);
}

#[tokio::test]
async fn a_provider_failure_is_passed_on_and_not_charged() {
    let server = server(None);
    let call = Call::generate(json!({ "prompt": "something", "quality": "low" }));

    server
        .pictures
        .fail_with(ProviderError::Refused("the prompt was blocked".into()));
    let (_, response) = call.paid(&server.app).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let problem = json_of(response).await;
    assert_eq!(problem["code"], "provider_refused");
    assert!(problem["detail"].as_str().unwrap().contains("blocked"));

    server.pictures.fail_with(ProviderError::Account);
    let (_, response) = call.paid(&server.app).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_of(response).await["code"], "provider_unavailable");

    assert_eq!(
        (server.payments.verifies(), server.payments.settles()),
        (2, 0)
    );
}

#[tokio::test]
async fn bad_requests_are_refused_before_any_price() {
    let server = server(None);
    let refused = [
        Call::generate(json!({ "size": "1024x1024" })),
        Call::generate(json!({ "prompt": "x", "size": "1000x1000" })),
        Call::generate(json!({ "prompt": "x", "quality": "auto" })),
        Call::generate(json!({ "prompt": "x", "format": "jpeg" })),
        Call::generate(json!({ "prompt": "x", "n": 2 })),
        Call::generate(json!({ "prompt": "x".repeat(32_001) })),
        Call::edit(&[("prompt", b"x")]),
        Call::edit(&[("prompt", b"x"), ("image", b"not a picture")]),
        Call::edit(&[("prompt", b"x"), ("image", TINY_PNG), ("mask", b"GIF89a")]),
        Call::edit(&[("prompt", b"x"), ("image", TINY_PNG), ("colour", b"red")]),
    ];
    for call in refused {
        let response = call.send(&server.app, None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!response.headers().contains_key(PAYMENT_REQUIRED));
    }
    assert_eq!(server.payments.verifies(), 0);
    assert!(server.pictures.jobs().is_empty() && server.pictures.edits().is_empty());
}

#[tokio::test]
async fn the_daily_cap_counts_what_openai_charges() {
    // A high-quality 1024×1024 costs us $0.211, over a $0.10 cap.
    let server = server(Some("0.10"));
    let call = Call::generate(json!({ "prompt": "x", "quality": "high" }));
    let (_, response) = call.paid(&server.app).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_of(response).await["code"], "daily_cap_reached");
    assert!(server.pictures.jobs().is_empty());

    let call = Call::generate(json!({ "prompt": "x", "quality": "low" }));
    let (_, response) = call.paid(&server.app).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn without_an_openai_key_picture_routes_refuse() {
    let config =
        Config::from_lookup(|name| (name == "UNBAKED_API_PAY_TO").then(|| PAY_TO.to_owned()))
            .unwrap();
    let services = Services {
        facilitator: Some(Arc::new(FakeFacilitator::new())),
        pictures: None,
    };
    let app = app_with(config, services);
    let response = Call::generate(json!({ "prompt": "x" }))
        .send(&app, None)
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_of(response).await["code"], "provider_not_configured");
}
