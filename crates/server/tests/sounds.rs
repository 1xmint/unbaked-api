//! The speech and music endpoints, with a fake provider and a fake facilitator.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use serde_json::{Value, json};
use tower::ServiceExt;
use unbaked_api::config::Config;
use unbaked_api::prices::{music_cost, speech_cost, with_margin};
use unbaked_api::providers::fake::{FakeSounds, tiny_mp3};
use unbaked_api::providers::{ProviderError, VoiceModel};
use unbaked_api::{Services, app_with};
use unbaked_pay::fake::FakeFacilitator;
use unbaked_pay::gate::{PAYMENT_REQUIRED, PAYMENT_SIGNATURE};
use unbaked_pay::{Requirements, wire};

const PAY_TO: &str = "0x00000000000000000000000000000000000000bb";
const PAYER: &str = "0x00000000000000000000000000000000000000aa";

static NONCE: AtomicU64 = AtomicU64::new(1);

struct Server {
    app: Router,
    payments: Arc<FakeFacilitator>,
    sounds: Arc<FakeSounds>,
}

fn server(daily_cap: Option<&str>) -> Server {
    let config = Config::from_lookup(|name| match name {
        "UNBAKED_API_PAY_TO" => Some(PAY_TO.to_owned()),
        "UNBAKED_API_DAILY_CAP_USD" => daily_cap.map(str::to_owned),
        _ => None,
    })
    .unwrap();
    let payments = Arc::new(FakeFacilitator::new());
    let sounds = Arc::new(FakeSounds::new());
    let services = Services {
        facilitator: Some(payments.clone()),
        sounds: Some(sounds.clone()),
        ..Services::default()
    };
    Server {
        app: app_with(config, services),
        payments,
        sounds,
    }
}

async fn send(app: &Router, uri: &str, body: &Value, payment: Option<&str>) -> Response {
    let mut request = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(payment) = payment {
        request = request.header(PAYMENT_SIGNATURE, payment);
    }
    let request = request.body(Body::from(body.to_string())).unwrap();
    app.clone().oneshot(request).await.unwrap()
}

async fn paid(app: &Router, uri: &str, body: Value) -> (Requirements, Response) {
    let asked = send(app, uri, &body, None).await;
    assert_eq!(asked.status(), StatusCode::PAYMENT_REQUIRED, "{uri}");
    let required = asked.headers()[PAYMENT_REQUIRED].to_str().unwrap();
    let terms = wire::decode_object(required).unwrap()["accepts"][0].clone();
    let terms: Requirements = serde_json::from_value(terms).unwrap();
    let payment = FakeFacilitator::payment(PAYER, &terms, NONCE.fetch_add(1, Ordering::SeqCst));
    (terms, send(app, uri, &body, Some(&payment)).await)
}

async fn json_of(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn speech_charges_by_characters_and_returns_mp3() {
    let server = server(None);
    let text = "Fresh bread, every morning.";
    let body =
        json!({ "text": text, "voice_id": "abc123", "model": "eleven_flash_v2_5", "seed": 4 });

    let (terms, response) = paid(&server.app, "/v1/speech", body).await;
    let cost = speech_cost(VoiceModel::FlashV2_5, text.chars().count());
    assert_eq!(terms.amount, with_margin(cost).to_string());
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/mpeg");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes.to_vec(), tiny_mp3());

    let speeches = server.sounds.speeches();
    assert_eq!(speeches.len(), 1);
    assert_eq!(speeches[0].voice_id, "abc123");
    assert_eq!(speeches[0].model, VoiceModel::FlashV2_5);
    assert_eq!(speeches[0].seed, Some(4));
    assert_eq!(server.payments.settles(), 1);
}

#[tokio::test]
async fn music_charges_by_length() {
    let server = server(None);
    let body = json!({ "prompt": "quiet warm piano", "length_ms": 10_000, "instrumental": true });

    let (terms, response) = paid(&server.app, "/v1/music", body).await;
    assert_eq!(terms.amount, with_margin(music_cost(10_000)).to_string());
    assert_eq!(response.status(), StatusCode::OK);

    let songs = server.sounds.songs();
    assert_eq!(songs.len(), 1);
    assert_eq!(songs[0].length_ms, 10_000);
    assert!(songs[0].instrumental);
    assert_eq!(server.payments.settles(), 1);
}

#[tokio::test]
async fn a_provider_failure_is_passed_on_and_not_charged() {
    let server = server(None);
    let body = json!({ "prompt": "in the style of a named band", "length_ms": 5_000 });

    server
        .sounds
        .fail_with(ProviderError::Refused("the prompt names an artist".into()));
    let (_, response) = paid(&server.app, "/v1/music", body.clone()).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let problem = json_of(response).await;
    assert_eq!(problem["code"], "provider_refused");
    assert!(problem["detail"].as_str().unwrap().contains("artist"));

    server.sounds.fail_with(ProviderError::Busy);
    let (_, response) = paid(&server.app, "/v1/music", body).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_of(response).await["code"], "provider_busy");

    assert_eq!(
        (server.payments.verifies(), server.payments.settles()),
        (2, 0)
    );
}

#[tokio::test]
async fn bad_requests_are_refused_before_any_price() {
    let server = server(None);
    let refused = [
        ("/v1/speech", json!({ "voice_id": "abc" })),
        ("/v1/speech", json!({ "text": "hi" })),
        (
            "/v1/speech",
            json!({ "text": "hi", "voice_id": "../voices" }),
        ),
        (
            "/v1/speech",
            json!({ "text": "hi", "voice_id": "abc", "model": "eleven_v1" }),
        ),
        (
            "/v1/speech",
            json!({ "text": "hi", "voice_id": "abc", "language_code": "EN" }),
        ),
        (
            "/v1/speech",
            json!({ "text": "x".repeat(5_001), "voice_id": "abc", "model": "eleven_v3" }),
        ),
        (
            "/v1/speech",
            json!({ "text": "hi", "voice_id": "abc", "speed": 2 }),
        ),
        ("/v1/music", json!({ "prompt": "piano" })),
        (
            "/v1/music",
            json!({ "prompt": "piano", "length_ms": 2_999 }),
        ),
        (
            "/v1/music",
            json!({ "prompt": "piano", "length_ms": 600_001 }),
        ),
        ("/v1/music", json!({ "length_ms": 10_000 })),
    ];
    for (uri, body) in refused {
        let response = send(&server.app, uri, &body, None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri} {body}");
        assert!(!response.headers().contains_key(PAYMENT_REQUIRED));
    }
    assert_eq!(server.payments.verifies(), 0);
    assert!(server.sounds.speeches().is_empty() && server.sounds.songs().is_empty());
}

#[tokio::test]
async fn the_daily_cap_counts_what_elevenlabs_charges() {
    // Ten minutes of music costs us $1.50, over a $1 cap.
    let server = server(Some("1"));
    let (_, response) = paid(
        &server.app,
        "/v1/music",
        json!({ "prompt": "piano", "length_ms": 600_000 }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_of(response).await["code"], "daily_cap_reached");
    assert!(server.sounds.songs().is_empty());
}

#[tokio::test]
async fn the_voice_list_is_free() {
    let server = server(None);
    let request = Request::get("/v1/speech/voices")
        .body(Body::empty())
        .unwrap();
    let response = server.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_of(response).await["voices"][0]["voice_id"],
        "fake-voice"
    );
    assert_eq!(server.payments.verifies(), 0);
}

#[tokio::test]
async fn without_an_elevenlabs_key_sound_routes_refuse() {
    let config =
        Config::from_lookup(|name| (name == "UNBAKED_API_PAY_TO").then(|| PAY_TO.to_owned()))
            .unwrap();
    let services = Services {
        facilitator: Some(Arc::new(FakeFacilitator::new())),
        ..Services::default()
    };
    let app = app_with(config, services);
    let body = json!({ "text": "hi", "voice_id": "abc" });
    let response = send(&app, "/v1/speech", &body, None).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_of(response).await["code"], "provider_not_configured");
}
