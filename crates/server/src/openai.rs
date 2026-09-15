//! OpenAI's picture model, called over HTTPS. The key never leaves this file's
//! request headers and is never logged.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};

use crate::providers::{BoxFuture, EditJob, Picture, PictureJob, Pictures, ProviderError, Usage};

/// The pinned snapshot, so pictures and prices do not shift under us.
pub const MODEL: &str = "gpt-image-2-2026-04-21";

pub const API_BASE: &str = "https://api.openai.com/v1";

/// High-quality pictures can take minutes.
const TIMEOUT: Duration = Duration::from_secs(300);

/// The most of a provider message passed on to the caller.
const MESSAGE_LIMIT: usize = 500;

pub struct OpenAi {
    client: reqwest::Client,
    key: String,
    base: String,
}

impl OpenAi {
    pub fn new(key: &str) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(TIMEOUT).build()?,
            key: key.to_owned(),
            base: API_BASE.to_owned(),
        })
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Picture, ProviderError> {
        let response = request
            .bearer_auth(&self.key)
            .send()
            .await
            .map_err(|error| ProviderError::Failed(without_url(error)))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|error| ProviderError::Failed(without_url(error)))?;
        read_answer(status, &body)
    }
}

impl Pictures for OpenAi {
    fn generate(&self, job: PictureJob) -> BoxFuture<'_, Result<Picture, ProviderError>> {
        Box::pin(async move {
            let body = generate_body(&job);
            let request = self
                .client
                .post(format!("{}/images/generations", self.base))
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string());
            self.send(request).await
        })
    }

    fn edit(&self, edit: EditJob) -> BoxFuture<'_, Result<Picture, ProviderError>> {
        Box::pin(async move {
            let request = self
                .client
                .post(format!("{}/images/edits", self.base))
                .multipart(edit_form(edit)?);
            self.send(request).await
        })
    }
}

fn settings(job: &PictureJob) -> [(&'static str, String); 5] {
    [
        ("model", MODEL.to_owned()),
        ("size", format!("{}x{}", job.width, job.height)),
        ("quality", job.quality.name().to_owned()),
        ("output_format", job.format.name().to_owned()),
        ("background", job.background.name().to_owned()),
    ]
}

pub fn generate_body(job: &PictureJob) -> Value {
    let mut body = json!({ "prompt": job.prompt, "n": 1 });
    for (name, value) in settings(job) {
        body[name] = Value::String(value);
    }
    body
}

fn edit_form(edit: EditJob) -> Result<Form, ProviderError> {
    let mut form = Form::new().text("prompt", edit.job.prompt.clone());
    for (name, value) in settings(&edit.job) {
        form = form.text(name, value);
    }
    for (index, image) in edit.images.into_iter().enumerate() {
        let (extension, media) = picture_type(&image);
        let part = Part::bytes(image)
            .file_name(format!("image-{index}.{extension}"))
            .mime_str(media)
            .map_err(|error| ProviderError::Failed(error.to_string()))?;
        form = form.part("image[]", part);
    }
    if let Some(mask) = edit.mask {
        let part = Part::bytes(mask)
            .file_name("mask.png")
            .mime_str("image/png")
            .map_err(|error| ProviderError::Failed(error.to_string()))?;
        form = form.part("mask", part);
    }
    Ok(form)
}

/// The file extension and media type of a source picture, by its first bytes.
pub fn picture_type(bytes: &[u8]) -> (&'static str, &'static str) {
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        ("jpg", "image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        ("webp", "image/webp")
    } else {
        ("png", "image/png")
    }
}

/// Reads OpenAI's answer: the first picture and the token counts, or why not.
pub fn read_answer(status: u16, body: &[u8]) -> Result<Picture, ProviderError> {
    let answer: Option<Value> = serde_json::from_slice(body).ok();
    match status {
        200..=299 => {}
        401 | 403 => return Err(ProviderError::Account),
        429 => {
            // OpenAI also sends 429 for a spent balance.
            let code = answer.as_ref().and_then(|a| a["error"]["code"].as_str());
            return Err(match code {
                Some("insufficient_quota") => ProviderError::Account,
                _ => ProviderError::Busy,
            });
        }
        400..=499 => {
            let message = answer
                .as_ref()
                .and_then(|a| a["error"]["message"].as_str())
                .unwrap_or("the request was refused");
            return Err(ProviderError::Refused(clip(message)));
        }
        _ => return Err(ProviderError::Failed(format!("status {status}"))),
    }

    let answer = answer.ok_or_else(|| ProviderError::Failed("the answer is not JSON".into()))?;
    let encoded = answer["data"][0]["b64_json"]
        .as_str()
        .ok_or_else(|| ProviderError::Failed("the answer has no picture".into()))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| ProviderError::Failed("the picture is not base64".into()))?;
    let usage = &answer["usage"];
    let usage = usage.is_object().then(|| Usage {
        text_tokens: usage["input_tokens_details"]["text_tokens"]
            .as_u64()
            .unwrap_or(0),
        image_tokens: usage["input_tokens_details"]["image_tokens"]
            .as_u64()
            .unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
    });
    Ok(Picture { bytes, usage })
}

fn clip(message: &str) -> String {
    message.chars().take(MESSAGE_LIMIT).collect()
}

/// A transport error's text, without the URL (which holds nothing secret, but
/// is noise to the caller).
fn without_url(error: reqwest::Error) -> String {
    let error = error.without_url();
    if error.is_timeout() {
        "no answer in time".to_owned()
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{Background, PictureFormat, Quality};

    fn job() -> PictureJob {
        PictureJob {
            prompt: "a pear".into(),
            width: 1024,
            height: 1536,
            quality: Quality::Low,
            format: PictureFormat::Webp,
            background: Background::Auto,
        }
    }

    #[test]
    fn the_request_names_the_pinned_model_and_settings() {
        let body = generate_body(&job());
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["size"], "1024x1536");
        assert_eq!(body["quality"], "low");
        assert_eq!(body["output_format"], "webp");
        assert_eq!(body["prompt"], "a pear");
    }

    #[test]
    fn a_good_answer_gives_the_picture_and_tokens() {
        let body = json!({
            "created": 1,
            "data": [{ "b64_json": STANDARD.encode(b"picture") }],
            "usage": {
                "input_tokens": 50, "output_tokens": 272, "total_tokens": 322,
                "input_tokens_details": { "text_tokens": 10, "image_tokens": 40 }
            }
        });
        let picture = read_answer(200, body.to_string().as_bytes()).unwrap();
        assert_eq!(picture.bytes, b"picture");
        let usage = picture.usage.unwrap();
        assert_eq!(
            (usage.text_tokens, usage.image_tokens, usage.output_tokens),
            (10, 40, 272)
        );
    }

    #[test]
    fn failures_are_sorted_by_who_can_fix_them() {
        let error = |message: &str, code: &str| {
            json!({ "error": { "message": message, "code": code } }).to_string()
        };
        assert_eq!(
            read_answer(400, error("unsafe prompt", "moderation_blocked").as_bytes()).unwrap_err(),
            ProviderError::Refused("unsafe prompt".into())
        );
        assert_eq!(read_answer(401, b"").unwrap_err(), ProviderError::Account);
        assert_eq!(
            read_answer(429, error("slow down", "rate_limit_exceeded").as_bytes()).unwrap_err(),
            ProviderError::Busy
        );
        assert_eq!(
            read_answer(429, error("no money", "insufficient_quota").as_bytes()).unwrap_err(),
            ProviderError::Account
        );
        assert!(matches!(
            read_answer(500, b"").unwrap_err(),
            ProviderError::Failed(_)
        ));
        assert!(matches!(
            read_answer(200, b"{}").unwrap_err(),
            ProviderError::Failed(_)
        ));
    }

    #[test]
    fn source_pictures_are_typed_by_their_first_bytes() {
        assert_eq!(picture_type(b"\xFF\xD8\xFF\xE0"), ("jpg", "image/jpeg"));
        assert_eq!(
            picture_type(b"RIFF\0\0\0\0WEBPVP8 "),
            ("webp", "image/webp")
        );
        assert_eq!(picture_type(b"\x89PNG\r\n\x1a\n"), ("png", "image/png"));
    }
}
