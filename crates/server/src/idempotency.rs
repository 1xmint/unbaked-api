//! `Idempotency-Key`: a client that sends the same request again with the same
//! key gets the first result back instead of running, and paying for, the work
//! twice. Only successful results are kept, in memory, for 24 hours or until
//! the process stops.
//!
//! A kept result is returned to anyone who sends the same key and the same
//! body, so keys must be long enough that nobody can guess another client's.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::BODY_LIMIT;
use crate::problem::Problem;

pub const HEADER: &str = "idempotency-key";
/// Set on a response that was kept from an earlier request.
pub const REPLAYED: &str = "idempotent-replayed";

pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// The most result bytes kept at once; the oldest go first.
pub const MAX_BYTES: usize = 256 * 1024 * 1024;

const MIN_KEY: usize = 16;
const MAX_KEY: usize = 255;

type Fingerprint = [u8; 32];

pub struct IdempotencyStore {
    ttl: Duration,
    max_bytes: usize,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    /// Kept results, oldest first.
    kept: VecDeque<(Instant, String)>,
    bytes: usize,
}

enum Entry {
    Running,
    Kept {
        fingerprint: Fingerprint,
        at: Instant,
        result: Kept,
    },
}

#[derive(Clone)]
struct Kept {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

enum Begin {
    Run,
    Replay(Kept),
    DifferentRequest,
    StillRunning,
}

impl Default for IdempotencyStore {
    fn default() -> Self {
        Self::new(TTL, MAX_BYTES)
    }
}

impl IdempotencyStore {
    pub fn new(ttl: Duration, max_bytes: usize) -> Self {
        Self {
            ttl,
            max_bytes,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn begin(&self, key: &str, fingerprint: Fingerprint) -> Begin {
        let mut inner = self.lock();
        inner.expire(Instant::now(), self.ttl);
        match inner.entries.get(key) {
            Some(Entry::Running) => Begin::StillRunning,
            Some(Entry::Kept {
                fingerprint: seen,
                result,
                ..
            }) => {
                if *seen == fingerprint {
                    Begin::Replay(result.clone())
                } else {
                    Begin::DifferentRequest
                }
            }
            None => {
                inner.entries.insert(key.to_owned(), Entry::Running);
                Begin::Run
            }
        }
    }

    /// Ends a running request: keeps `result` if there is one and it fits.
    fn finish(&self, key: &str, fingerprint: Fingerprint, result: Option<Kept>) {
        let mut inner = self.lock();
        if !matches!(inner.entries.get(key), Some(Entry::Running)) {
            return;
        }
        inner.entries.remove(key);
        let Some(result) = result else { return };
        let size = result.body.len();
        if size > self.max_bytes {
            return;
        }
        let now = Instant::now();
        inner.expire(now, self.ttl);
        while inner.bytes + size > self.max_bytes && inner.evict_oldest() {}
        inner.bytes += size;
        inner.kept.push_back((now, key.to_owned()));
        inner.entries.insert(
            key.to_owned(),
            Entry::Kept {
                fingerprint,
                at: now,
                result,
            },
        );
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Inner {
    fn expire(&mut self, now: Instant, ttl: Duration) {
        while let Some((at, _)) = self.kept.front() {
            if now.duration_since(*at) < ttl {
                break;
            }
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) -> bool {
        let Some((at, key)) = self.kept.pop_front() else {
            return false;
        };
        if let Some(Entry::Kept {
            at: kept_at,
            result,
            ..
        }) = self.entries.get(&key)
            && *kept_at == at
        {
            self.bytes -= result.body.len();
            self.entries.remove(&key);
        }
        true
    }
}

/// Clears a running entry if the request ends without finishing, for example
/// when the client hangs up, so the key can be used again.
struct Running {
    store: Arc<IdempotencyStore>,
    key: Option<String>,
    fingerprint: Fingerprint,
}

impl Running {
    fn finish(mut self, result: Option<Kept>) {
        if let Some(key) = self.key.take() {
            self.store.finish(&key, self.fingerprint, result);
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.store.finish(&key, self.fingerprint, None);
        }
    }
}

pub async fn layer(
    State(store): State<Arc<IdempotencyStore>>,
    request: Request,
    next: Next,
) -> Response {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let Some(key) = request.headers().get(HEADER).filter(|_| !safe) else {
        return next.run(request).await;
    };
    let Some(key) = key.to_str().ok().filter(|key| valid_key(key)) else {
        return Problem::new(
            StatusCode::BAD_REQUEST,
            "bad_idempotency_key",
            format!("Idempotency-Key must be {MIN_KEY} to {MAX_KEY} visible ASCII characters."),
        )
        .into_response();
    };
    let key = key.to_owned();

    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, BODY_LIMIT).await else {
        return Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!("The request body is over {BODY_LIMIT} bytes or could not be read."),
        )
        .into_response();
    };
    let fingerprint = fingerprint(&parts.method, &parts.uri, &parts.headers, &body);

    match store.begin(&key, fingerprint) {
        Begin::Run => {}
        Begin::Replay(kept) => return replay(kept),
        Begin::DifferentRequest => {
            return Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "idempotency_key_reused",
                "This Idempotency-Key was already used for a different request.",
            )
            .into_response();
        }
        Begin::StillRunning => {
            return Problem::new(
                StatusCode::CONFLICT,
                "idempotency_key_running",
                "A request with this Idempotency-Key is still running; retry after it ends.",
            )
            .into_response();
        }
    }
    let running = Running {
        store,
        key: Some(key),
        fingerprint,
    };

    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
    if !response.status().is_success() {
        running.finish(None);
        return response;
    }
    let (parts, body) = response.into_parts();
    let Ok(body) = to_bytes(body, usize::MAX).await else {
        running.finish(None);
        return Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "The response could not be read.",
        )
        .into_response();
    };
    running.finish(Some(Kept {
        status: parts.status,
        headers: parts.headers.clone(),
        body: body.clone(),
    }));
    Response::from_parts(parts, Body::from(body))
}

fn valid_key(key: &str) -> bool {
    (MIN_KEY..=MAX_KEY).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic())
}

/// What makes two requests "the same": method, path and query, content type
/// and body.
fn fingerprint(
    method: &Method,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Fingerprint {
    let content_type = headers
        .get(CONTENT_TYPE)
        .map(HeaderValue::as_bytes)
        .unwrap_or_default();
    let mut hash = Sha256::new();
    for part in [
        method.as_str().as_bytes(),
        uri.to_string().as_bytes(),
        content_type,
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.update(body);
    hash.finalize().into()
}

fn replay(kept: Kept) -> Response {
    let mut response = Response::new(Body::from(kept.body));
    *response.status_mut() = kept.status;
    *response.headers_mut() = kept.headers;
    response
        .headers_mut()
        .insert(REPLAYED, HeaderValue::from_static("true"));
    response
}
