//! Pictures from OpenAI: `generate` from a prompt, `edit` from source pictures
//! and a prompt. Both are paid, priced from the size, quality, prompt length
//! and number of source pictures before anything is sent to OpenAI.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Multipart, State};
use axum::http::HeaderMap;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use unbaked_pay::Quote;

use crate::files::{bad_request, resource};
use crate::providers::{
    Background, EditJob, Picture, PictureFormat, PictureJob, Pictures, ProviderError, Quality,
};
use crate::{AppState, prices};

/// The longest prompt accepted, in characters.
pub const MAX_PROMPT_CHARS: usize = 32_000;

/// The most source pictures in one edit.
pub const MAX_SOURCES: usize = 16;

/// OpenAI's limits for custom sizes.
pub const MAX_EDGE: u32 = 3840;
pub const MIN_PIXELS: u32 = 655_360;
pub const MAX_PIXELS: u32 = 8_294_400;

pub const DEFAULT_SIZE: &str = "1024x1024";

/// The request's settings, before checking.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    prompt: Option<String>,
    size: Option<String>,
    quality: Option<String>,
    format: Option<String>,
    background: Option<String>,
}

impl Settings {
    fn job(self) -> Result<PictureJob, Box<Response>> {
        let bad = |detail: String| Box::new(bad_request(detail));
        let prompt = self.prompt.unwrap_or_default();
        let chars = prompt.chars().count();
        if prompt.trim().is_empty() || chars > MAX_PROMPT_CHARS {
            return Err(bad(format!(
                "\"prompt\" is required, up to {MAX_PROMPT_CHARS} characters"
            )));
        }
        let size = self.size.as_deref().unwrap_or(DEFAULT_SIZE);
        let Some((width, height)) = parse_size(size) else {
            return Err(bad(format!(
                "\"size\" {size:?} must be WIDTHxHEIGHT: both multiples of 16, each at most \
                 {MAX_EDGE}, sides within 3:1, {MIN_PIXELS} to {MAX_PIXELS} pixels"
            )));
        };
        let quality = self.quality.as_deref().unwrap_or("medium");
        let Some(quality) = Quality::parse(quality) else {
            return Err(bad("\"quality\" must be low, medium or high".into()));
        };
        let format = self.format.as_deref().unwrap_or("png");
        let Some(format) = PictureFormat::parse(format) else {
            return Err(bad("\"format\" must be png or webp".into()));
        };
        let background = self.background.as_deref().unwrap_or("auto");
        let Some(background) = Background::parse(background) else {
            return Err(bad(
                "\"background\" must be auto, opaque or transparent".into()
            ));
        };
        Ok(PictureJob {
            prompt,
            width,
            height,
            quality,
            format,
            background,
        })
    }
}

/// "1536x1024" as (1536, 1024), if OpenAI accepts that size.
pub fn parse_size(size: &str) -> Option<(u32, u32)> {
    let (width, height) = size.split_once('x')?;
    let (width, height): (u32, u32) = (width.parse().ok()?, height.parse().ok()?);
    let pixels = width.checked_mul(height)?;
    let fits = width % 16 == 0
        && height % 16 == 0
        && width.max(height) <= MAX_EDGE
        && width.max(height) <= 3 * width.min(height)
        && (MIN_PIXELS..=MAX_PIXELS).contains(&pixels);
    fits.then_some((width, height))
}

pub async fn generate_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let pictures = match service(&state) {
        Ok(pictures) => pictures,
        Err(response) => return *response,
    };
    let settings: Settings = match serde_json::from_slice(&body) {
        Ok(settings) => settings,
        Err(error) => return bad_request(format!("the body is not a picture request: {error}")),
    };
    let job = match settings.job() {
        Ok(job) => job,
        Err(response) => return *response,
    };
    let cost = prices::picture_cost(
        job.quality,
        job.width,
        job.height,
        job.prompt.chars().count(),
        0,
    );
    let quote = Quote {
        amount: prices::with_margin(cost),
        cost,
        resource: resource(
            "/v1/images/generate",
            "a picture from a prompt",
            job.format.media_type(),
        ),
    };
    let format = job.format;
    crate::take_payment(&state, &headers, &quote, || async move {
        answer("generate", format, pictures.generate(job).await)
    })
    .await
}

/// Multipart parts: `prompt` (required), `image` (1 to 16 source pictures,
/// PNG, JPEG or WebP), `mask` (optional PNG), and `size`, `quality`, `format`,
/// `background` as for generate.
pub async fn edit_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let pictures = match service(&state) {
        Ok(pictures) => pictures,
        Err(response) => return *response,
    };
    let (mut settings, mut images, mut mask) = (Settings::default(), Vec::new(), None);
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
        let text = || String::from_utf8(bytes.to_vec()).ok();
        let slot = match name.as_str() {
            "image" => {
                images.push(bytes.to_vec());
                continue;
            }
            "mask" => {
                mask = Some(bytes.to_vec());
                continue;
            }
            "prompt" => &mut settings.prompt,
            "size" => &mut settings.size,
            "quality" => &mut settings.quality,
            "format" => &mut settings.format,
            "background" => &mut settings.background,
            _ => {
                return bad_request(format!(
                    "unknown part {name:?}; use prompt, image, mask, size, quality, format or \
                     background"
                ));
            }
        };
        let Some(value) = text() else {
            return bad_request(format!("the part {name:?} is not UTF-8 text"));
        };
        *slot = Some(value);
    }
    if images.is_empty() || images.len() > MAX_SOURCES {
        return bad_request(format!(
            "send 1 to {MAX_SOURCES} \"image\" parts (PNG, JPEG or WebP)"
        ));
    }
    if !images.iter().all(|image| is_source_picture(image)) {
        return bad_request("every \"image\" part must be a PNG, JPEG or WebP picture");
    }
    if mask
        .as_ref()
        .is_some_and(|mask| !PictureFormat::Png.matches(mask))
    {
        return bad_request("the \"mask\" part must be a PNG");
    }
    let job = match settings.job() {
        Ok(job) => job,
        Err(response) => return *response,
    };
    let cost = prices::picture_cost(
        job.quality,
        job.width,
        job.height,
        job.prompt.chars().count(),
        images.len(),
    );
    let quote = Quote {
        amount: prices::with_margin(cost),
        cost,
        resource: resource(
            "/v1/images/edit",
            "a picture changed by a prompt",
            job.format.media_type(),
        ),
    };
    let format = job.format;
    let edit = EditJob { job, images, mask };
    crate::take_payment(&state, &headers, &quote, || async move {
        answer("edit", format, pictures.edit(edit).await)
    })
    .await
}

/// The picture service, or 503 before any price is asked.
fn service(state: &AppState) -> Result<Arc<dyn Pictures>, Box<Response>> {
    state.pictures.clone().ok_or_else(|| {
        Box::new(crate::providers::not_configured(
            "OPENAI_API_KEY",
            "pictures",
        ))
    })
}

fn is_source_picture(bytes: &[u8]) -> bool {
    PictureFormat::Png.matches(bytes)
        || PictureFormat::Webp.matches(bytes)
        || bytes.starts_with(b"\xFF\xD8\xFF")
}

/// The provider's answer as a response. Only a picture in the asked format
/// is a success; everything else is an error, so it is never settled.
fn answer(
    route: &'static str,
    format: PictureFormat,
    result: Result<Picture, ProviderError>,
) -> Response {
    match result {
        Ok(picture) if format.matches(&picture.bytes) => {
            if let Some(usage) = picture.usage {
                // Token counts only, never the prompt, to check the price table.
                tracing::info!(
                    route,
                    text_tokens = usage.text_tokens,
                    image_tokens = usage.image_tokens,
                    output_tokens = usage.output_tokens,
                    "picture made"
                );
            }
            ([(CONTENT_TYPE, format.media_type())], picture.bytes).into_response()
        }
        Ok(_) => provider_problem(
            route,
            ProviderError::Failed(format!("the answer is not a {} picture", format.name())),
        ),
        Err(error) => provider_problem(route, error),
    }
}

fn provider_problem(route: &'static str, error: ProviderError) -> Response {
    crate::providers::problem("picture", route, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_follow_openai_rules() {
        assert_eq!(parse_size("1024x1024"), Some((1024, 1024)));
        assert_eq!(parse_size("3840x2160"), Some((3840, 2160)));
        assert_eq!(parse_size("1536x864"), Some((1536, 864)));
        for bad in [
            "1000x1000", // not multiples of 16
            "3856x2160", // edge too long
            "2400x768",  // wider than 3:1
            "512x512",   // too few pixels
            "3840x3840", // too many pixels
            "1024",
            "axb",
            "",
        ] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
    }
}
