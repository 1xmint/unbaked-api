//! File work on Unbaked files through the layer 1 library: `estimate` is free;
//! `edit`, `preview`, `listen` and `render` are paid.
//!
//! A paid call is priced before it runs, from its own body. The work runs on a
//! blocking thread with render limits and a deadline, after the payment is
//! verified; a failure answers problem+json and is never settled.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Multipart, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use unbaked_core::edit::EditError;
use unbaked_core::pack::{self, Files};
use unbaked_core::package::Limits;
use unbaked_core::recipe::OutputKind;
use unbaked_pay::{Quote, Resource};
use unbaked_render::estimate::{Estimate, estimate};
use unbaked_render::preview::{MAX_SHEET, PreviewOptions};
use unbaked_render::{Deadline, NoFonts, RenderError, RenderLimits};

use crate::problem::Problem;
use crate::{AppState, prices};

/// How long each kind of job may run.
pub const PREVIEW_MS: u64 = 20_000;
pub const LISTEN_MS: u64 = 20_000;
pub const RENDER_MS: u64 = 60_000;

/// The longest edge a preview may ask for.
pub const MAX_PREVIEW_EDGE: u32 = 2048;

/// Limits for one job, with its deadline starting now. Lower than layer 1's
/// defaults: 36 million pixels per buffer, 10 minutes of stereo at 48 kHz.
fn limits(ms: u64) -> RenderLimits {
    RenderLimits {
        max_pixels: 36_000_000,
        max_samples: 28_800_000,
        max_frames: 18_000,
        deadline: Some(Deadline::after_ms(ms)),
    }
}

pub async fn estimate_route(body: Bytes) -> Response {
    let files = match load(&body) {
        Ok(files) => files,
        Err(response) => return *response,
    };
    match estimate(&files) {
        Ok(size) => Json(estimate_json(&size)).into_response(),
        Err(error) => render_problem(error),
    }
}

pub async fn render_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let files = match load(&body) {
        Ok(files) => files,
        Err(response) => return *response,
    };
    let size = match estimate(&files) {
        Ok(size) => size,
        Err(error) => return render_problem(error),
    };
    let amount = prices::file_work(size.work_units);
    let resource = resource(
        "/v1/render",
        "render an Unbaked file",
        media_type(size.kind),
    );
    charge(
        &state,
        &headers,
        amount,
        resource,
        move || match unbaked_render::render(&files, &NoFonts, limits(RENDER_MS)) {
            Ok(file) => ([(CONTENT_TYPE, media_type(size.kind))], file).into_response(),
            Err(error) => render_problem(error),
        },
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct PreviewQuery {
    pub at_ms: Option<u64>,
    pub max_edge: Option<u32>,
    pub sheet: Option<u32>,
}

pub async fn preview_route(
    State(state): State<AppState>,
    Query(query): Query<PreviewQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let options = PreviewOptions {
        at_ms: query.at_ms,
        max_edge: query.max_edge.unwrap_or(PreviewOptions::default().max_edge),
        sheet: query.sheet,
    };
    if !(1..=MAX_PREVIEW_EDGE).contains(&options.max_edge) {
        return bad_request(format!("max_edge must be 1 to {MAX_PREVIEW_EDGE}"));
    }
    if options.sheet.is_some_and(|n| !(1..=MAX_SHEET).contains(&n)) {
        return bad_request(format!("sheet must be 1 to {MAX_SHEET}"));
    }
    let files = match load(&body) {
        Ok(files) => files,
        Err(response) => return *response,
    };
    let size = match estimate(&files) {
        Ok(size) => size,
        Err(error) => return render_problem(error),
    };
    // A preview draws one frame, or `sheet` frames, not the whole video.
    let per_frame = size.work_units / size.frames.max(1);
    let frames = u64::from(options.sheet.unwrap_or(1));
    let amount = prices::file_work(per_frame.saturating_mul(frames));
    let resource = resource(
        "/v1/preview",
        "a PNG preview of an Unbaked file",
        "image/png",
    );
    charge(
        &state,
        &headers,
        amount,
        resource,
        move || match unbaked_render::preview::preview_png(
            &files,
            &NoFonts,
            limits(PREVIEW_MS),
            options,
        ) {
            Ok(png) => ([(CONTENT_TYPE, "image/png")], png).into_response(),
            Err(error) => render_problem(error),
        },
    )
    .await
}

pub async fn listen_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let files = match load(&body) {
        Ok(files) => files,
        Err(response) => return *response,
    };
    let size = match estimate(&files) {
        Ok(size) => size,
        Err(error) => return render_problem(error),
    };
    let amount = prices::file_work(size.work_units);
    let resource = resource(
        "/v1/listen",
        "loudness and silences of an Unbaked file's sound",
        "application/json",
    );
    charge(&state, &headers, amount, resource, move || {
        match unbaked_render::preview::listen(&files, limits(LISTEN_MS)) {
            Ok(stats) => Json(json!({
                "duration_ms": stats.duration_ms,
                "sample_rate": stats.sample_rate,
                "channels": stats.channels,
                "peak_dbfs": round(stats.peak_dbfs),
                "clipped_samples": stats.clipped_samples,
                "window_ms": unbaked_render::preview::WINDOW_MS,
                "loudness_dbfs": stats.loudness_dbfs.iter().map(|db| round(*db)).collect::<Vec<_>>(),
                "silences": stats.silences.iter().map(|s| json!({"start_ms": s.start_ms, "end_ms": s.end_ms})).collect::<Vec<_>>(),
            }))
            .into_response(),
            Err(error) => render_problem(error),
        }
    })
    .await
}

/// `multipart/form-data` with the part `file` (an Unbaked file), and a `patch`
/// (JSON Patch on `recipe.json`), parts named `asset.<id>` (media stored as
/// asset `id`), or both. Assets are added before the patch runs.
pub async fn edit_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let (mut file, mut patch, mut assets) = (None, None, Vec::new());
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => return error.into_response(),
        };
        let name = field.name().unwrap_or_default().to_owned();
        let bytes = match field.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => return error.into_response(),
        };
        match name.as_str() {
            "file" => file = Some(bytes),
            "patch" => patch = Some(bytes),
            _ if name.starts_with("asset.") => {
                assets.push((name["asset.".len()..].to_owned(), bytes))
            }
            _ => {
                return bad_request(format!(
                    "unknown part {name:?}; use file, patch or asset.<id>"
                ));
            }
        }
    }
    let Some(file) = file else {
        return bad_request("the part \"file\" is required");
    };
    if patch.is_none() && assets.is_empty() {
        return bad_request("send a \"patch\" part, \"asset.<id>\" parts, or both");
    }
    let before = match load(&file) {
        Ok(files) => files,
        Err(response) => return *response,
    };

    let resource = resource(
        "/v1/edit",
        "edit an Unbaked file's recipe and assets",
        "application/octet-stream",
    );
    charge(&state, &headers, prices::EDIT, resource, move || {
        let mut after = before;
        for (id, bytes) in &assets {
            after = match unbaked_core::edit::add_asset(&after, id, bytes, None) {
                Ok(files) => files,
                Err(error) => return edit_problem(error),
            };
        }
        if let Some(patch) = &patch {
            after = match unbaked_core::edit::edit(&after, patch) {
                Ok(files) => files,
                Err(error) => return edit_problem(error),
            };
        }
        let packed = pack::write(&after, Limits::default())
            .map_err(|error| error.to_string())
            .and_then(|package| {
                pack::with_package(&file, &package).map_err(|error| error.to_string())
            });
        match packed {
            Ok(out) => {
                let kind = unbaked_render::checked_recipe(&after).map(|r| r.output.kind);
                let media = kind.map_or("application/octet-stream", media_type);
                ([(CONTENT_TYPE, media)], out).into_response()
            }
            Err(detail) => {
                Problem::new(StatusCode::UNPROCESSABLE_ENTITY, "format", detail).into_response()
            }
        }
    })
    .await
}

/// Takes payment, then runs `work` on a blocking thread.
async fn charge<W>(
    state: &AppState,
    headers: &HeaderMap,
    amount: u64,
    resource: Resource,
    work: W,
) -> Response
where
    W: FnOnce() -> Response + Send + 'static,
{
    let Some(gate) = &state.gate else {
        return Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "payments_not_configured",
            "this server has no UNBAKED_API_PAY_TO, so it cannot take payment",
        )
        .into_response();
    };
    let quote = Quote {
        amount,
        cost: 0,
        resource,
    };
    gate.charge(headers, &quote, || async move {
        match tokio::task::spawn_blocking(work).await {
            Ok(response) => response,
            Err(_) => Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "job_failed",
                "the job stopped unexpectedly; nothing was charged",
            )
            .into_response(),
        }
    })
    .await
}

/// The package inside an Unbaked file.
fn load(body: &[u8]) -> Result<Files, Box<Response>> {
    pack::read_files(body, Limits::default()).map_err(|error| {
        Box::new(
            Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "format",
                format!("the body is not an Unbaked file: {error}"),
            )
            .into_response(),
        )
    })
}

fn estimate_json(size: &Estimate) -> serde_json::Value {
    let per_frame = size.work_units / size.frames.max(1);
    json!({
        "kind": kind_name(size.kind),
        "canvas_pixels": size.canvas_pixels,
        "frames": size.frames,
        "layers": size.layers,
        "output_samples": size.output_samples,
        "asset_bytes": size.asset_bytes,
        "work_units": size.work_units,
        "prices": {
            "unit": "millionths of a USDC",
            "render": prices::file_work(size.work_units),
            "listen": prices::file_work(size.work_units),
            "preview": prices::file_work(per_frame),
            "edit": prices::EDIT,
        },
    })
}

fn resource(path: &str, description: &str, mime_type: &str) -> Resource {
    Resource {
        url: path.to_owned(),
        description: Some(description.to_owned()),
        mime_type: Some(mime_type.to_owned()),
    }
}

fn kind_name(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Image => "image",
        OutputKind::Audio => "audio",
        OutputKind::Video => "video",
    }
}

fn media_type(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Image => "image/png",
        OutputKind::Audio => "audio/mp4",
        OutputKind::Video => "video/mp4",
    }
}

/// Decibels to one decimal place; silence (minus infinity) becomes null.
fn round(db: f64) -> Option<f64> {
    db.is_finite().then(|| (db * 10.0).round() / 10.0)
}

fn bad_request(detail: impl Into<String>) -> Response {
    Problem::new(StatusCode::BAD_REQUEST, "bad_request", detail).into_response()
}

/// Render failures use the same words as the `unbaked` command's `--json`.
fn render_problem(error: RenderError) -> Response {
    let (status, code) = match &error {
        RenderError::Recipe(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid"),
        RenderError::Unsupported(_) => (StatusCode::UNPROCESSABLE_ENTITY, "unsupported"),
        RenderError::Decode { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "decode"),
        RenderError::FontNotFound { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "font_not_found"),
        RenderError::TooLarge { .. }
        | RenderError::TooLong { .. }
        | RenderError::TooManyFrames { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "over_limit"),
        RenderError::TimedOut { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "timed_out"),
        RenderError::Package(_) => (StatusCode::UNPROCESSABLE_ENTITY, "format"),
        RenderError::Encode(_) => (StatusCode::INTERNAL_SERVER_ERROR, "encode"),
    };
    Problem::new(status, code, error.to_string()).into_response()
}

fn edit_problem(error: EditError) -> Response {
    let code = match &error {
        EditError::Patch(_) => "invalid_patch",
        EditError::Recipe(_) => "invalid",
        EditError::Asset(_) => "unsupported",
    };
    Problem::new(StatusCode::UNPROCESSABLE_ENTITY, code, error.to_string()).into_response()
}
