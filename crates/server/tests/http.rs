use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use tokio::sync::Notify;
use tower::ServiceExt;
use unbaked_api::config::Config;
use unbaked_api::idempotency::{self, IdempotencyStore};
use unbaked_api::{BODY_LIMIT, app, layered, routes};

const KEY: &str = "0123456789abcdef-key";

fn config() -> Config {
    Config::from_lookup(|_| None).unwrap()
}

async fn body_of(response: Response) -> Bytes {
    to_bytes(response.into_body(), usize::MAX).await.unwrap()
}

async fn problem_of(response: Response) -> serde_json::Value {
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    serde_json::from_slice(&body_of(response).await).unwrap()
}

fn post_with_key(uri: &str, key: &str, body: impl Into<Body>) -> Request<Body> {
    Request::post(uri)
        .header(idempotency::HEADER, key)
        .body(body.into())
        .unwrap()
}

/// The real routes plus `/echo`, which counts its runs, answers 500 for the
/// body "fail", and waits for `gate` on the body "wait".
fn echo_app(store: IdempotencyStore) -> (Router, Arc<AtomicUsize>, Arc<Notify>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let (count, wait) = (runs.clone(), gate.clone());
    let echo = Router::new().route(
        "/echo",
        post(move |body: Bytes| {
            let (count, wait) = (count.clone(), wait.clone());
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                match &body[..] {
                    b"fail" => (StatusCode::INTERNAL_SERVER_ERROR, body),
                    b"wait" => {
                        wait.notified().await;
                        (StatusCode::OK, body)
                    }
                    _ => (StatusCode::OK, body),
                }
            }
        }),
    );
    let app = layered(routes(config()).merge(echo), Arc::new(store));
    (app, runs, gate)
}

#[tokio::test]
async fn health_names_the_network() {
    let response = app(config())
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let health: serde_json::Value = serde_json::from_slice(&body_of(response).await).unwrap();
    assert_eq!(health["ok"], true);
    assert_eq!(health["network"], "eip155:84532");
}

#[tokio::test]
async fn guide_is_layer_one_agents_md() {
    let response = app(config())
        .oneshot(Request::get("/v1/guide").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/markdown; charset=utf-8"
    );
    let text = body_of(response).await;
    assert!(text.starts_with(b"# Making and editing Unbaked files"));
}

#[tokio::test]
async fn errors_are_problem_json() {
    let response = app(config())
        .oneshot(Request::get("/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let problem = problem_of(response).await;
    assert_eq!(problem["status"], 404);
    assert_eq!(problem["code"], "not_found");

    let response = app(config())
        .oneshot(Request::post("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(response.headers().contains_key(header::ALLOW));
    assert_eq!(problem_of(response).await["code"], "method_not_allowed");
}

#[tokio::test]
async fn body_over_the_cap_is_refused_with_or_without_a_key() {
    let (app, runs, _) = echo_app(IdempotencyStore::default());
    let big = vec![b'x'; BODY_LIMIT + 1];

    let response = app
        .clone()
        .oneshot(
            Request::post("/echo")
                .body(Body::from(big.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(problem_of(response).await["code"], "too_large");

    let response = app.oneshot(post_with_key("/echo", KEY, big)).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(problem_of(response).await["code"], "too_large");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_repeat_returns_the_first_result_without_running_again() {
    let (app, runs, _) = echo_app(IdempotencyStore::default());

    let first = app
        .clone()
        .oneshot(post_with_key("/echo", KEY, "hello"))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert!(!first.headers().contains_key(idempotency::REPLAYED));
    assert_eq!(body_of(first).await, "hello");

    let again = app
        .oneshot(post_with_key("/echo", KEY, "hello"))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(again.headers()[idempotency::REPLAYED], "true");
    assert_eq!(body_of(again).await, "hello");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_same_key_for_a_different_request_is_refused() {
    let (app, runs, _) = echo_app(IdempotencyStore::default());
    app.clone()
        .oneshot(post_with_key("/echo", KEY, "hello"))
        .await
        .unwrap();

    let other = app
        .oneshot(post_with_key("/echo", KEY, "goodbye"))
        .await
        .unwrap();
    assert_eq!(other.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem_of(other).await["code"], "idempotency_key_reused");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_failed_result_is_not_kept() {
    let (app, runs, _) = echo_app(IdempotencyStore::default());
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(post_with_key("/echo", KEY, "fail"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_repeat_while_the_first_still_runs_is_refused() {
    let (app, runs, gate) = echo_app(IdempotencyStore::default());
    let first = tokio::spawn(app.clone().oneshot(post_with_key("/echo", KEY, "wait")));
    while runs.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }

    let second = app
        .clone()
        .oneshot(post_with_key("/echo", KEY, "wait"))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(problem_of(second).await["code"], "idempotency_key_running");

    gate.notify_one();
    assert_eq!(first.await.unwrap().unwrap().status(), StatusCode::OK);
    let third = app
        .oneshot(post_with_key("/echo", KEY, "wait"))
        .await
        .unwrap();
    assert_eq!(third.headers()[idempotency::REPLAYED], "true");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_client_that_hangs_up_frees_the_key() {
    let (app, runs, gate) = echo_app(IdempotencyStore::default());
    let first = tokio::spawn(app.clone().oneshot(post_with_key("/echo", KEY, "wait")));
    while runs.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    first.abort();
    let _ = first.await;

    let retry = tokio::spawn(app.oneshot(post_with_key("/echo", KEY, "wait")));
    while runs.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }
    gate.notify_one();
    assert_eq!(retry.await.unwrap().unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn kept_results_expire() {
    let (app, runs, _) = echo_app(IdempotencyStore::new(Duration::from_millis(50), 1024));
    app.clone()
        .oneshot(post_with_key("/echo", KEY, "hello"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    let later = app
        .oneshot(post_with_key("/echo", KEY, "hello"))
        .await
        .unwrap();
    assert!(!later.headers().contains_key(idempotency::REPLAYED));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_oldest_result_goes_when_the_store_is_full() {
    let (app, runs, _) = echo_app(IdempotencyStore::new(idempotency::TTL, 10));
    let first_key = "first-key-0123456789";
    app.clone()
        .oneshot(post_with_key("/echo", first_key, "123456"))
        .await
        .unwrap();
    app.clone()
        .oneshot(post_with_key("/echo", KEY, "789012"))
        .await
        .unwrap();

    let first_again = app
        .clone()
        .oneshot(post_with_key("/echo", first_key, "123456"))
        .await
        .unwrap();
    assert!(!first_again.headers().contains_key(idempotency::REPLAYED));
    assert_eq!(runs.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_short_key_is_refused_and_reads_ignore_keys() {
    let (app, runs, _) = echo_app(IdempotencyStore::default());
    let short = app
        .clone()
        .oneshot(post_with_key("/echo", "short", "hello"))
        .await
        .unwrap();
    assert_eq!(short.status(), StatusCode::BAD_REQUEST);
    assert_eq!(problem_of(short).await["code"], "bad_idempotency_key");
    assert_eq!(runs.load(Ordering::SeqCst), 0);

    let read = Request::get("/health")
        .header(idempotency::HEADER, "short")
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.oneshot(read).await.unwrap().status(), StatusCode::OK);
}
