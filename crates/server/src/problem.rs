//! Errors as `application/problem+json` (RFC 9457), so an agent reads every
//! failure the same way.

use axum::body::to_bytes;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

pub const CONTENT_TYPE_PROBLEM: &str = "application/problem+json";

/// The most of a plain error body kept as the problem's detail.
const DETAIL_LIMIT: usize = 4096;

#[derive(Debug, Serialize)]
pub struct Problem {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub title: String,
    pub status: u16,
    /// A stable word for the failure, for code to match on.
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Problem {
    pub fn new(status: StatusCode, code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: "about:blank",
            title: status.canonical_reason().unwrap_or("Error").to_owned(),
            status: status.as_u16(),
            code: code.into(),
            detail: Some(detail.into()),
        }
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::to_vec(&self).unwrap_or_default();
        (status, [(CONTENT_TYPE, CONTENT_TYPE_PROBLEM)], body).into_response()
    }
}

/// Turns any error response that is not already problem+json (an unknown
/// route, a wrong method, a body over the cap) into one, keeping its status,
/// its other headers, and its text as the detail.
pub async fn plain_errors(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) || is_problem(response.headers()) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let detail = to_bytes(body, DETAIL_LIMIT)
        .await
        .ok()
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty());
    parts.headers.remove(CONTENT_TYPE);
    parts.headers.remove(CONTENT_LENGTH);

    let mut problem = Problem::new(status, code_for(status), "");
    problem.detail = detail;
    let mut out = problem.into_response();
    out.headers_mut().extend(parts.headers);
    out
}

fn is_problem(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with(CONTENT_TYPE_PROBLEM))
}

fn code_for(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "bad_request",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
        StatusCode::PAYLOAD_TOO_LARGE => "too_large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
        StatusCode::UNPROCESSABLE_ENTITY => "unprocessable",
        status if status.is_server_error() => "server_error",
        _ => "client_error",
    }
}
