//! Sound from ElevenLabs: `speech` from text in a chosen voice, `music` from a
//! prompt. Both are paid and return MP3, priced by characters or length
//! before anything is sent. The voice list is free.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use unbaked_pay::Quote;

use crate::files::{bad_request, resource};
use crate::providers::{self, MusicJob, ProviderError, Sounds, SpeechJob, VoiceModel, is_mp3};
use crate::{AppState, prices};

pub const MP3: &str = "audio/mpeg";

/// ElevenLabs' music lengths, in milliseconds.
pub const MIN_MUSIC_MS: u32 = 3_000;
pub const MAX_MUSIC_MS: u32 = 600_000;

/// Our cap on a music prompt, in characters.
pub const MAX_MUSIC_PROMPT_CHARS: usize = 2_000;

/// The longest voice id accepted. Ids go into the ElevenLabs URL, so only
/// letters and digits are allowed.
pub const MAX_VOICE_ID: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpeechRequest {
    text: Option<String>,
    voice_id: Option<String>,
    model: Option<String>,
    language_code: Option<String>,
    seed: Option<u32>,
}

impl SpeechRequest {
    fn job(self) -> Result<SpeechJob, String> {
        let model = self.model.as_deref().unwrap_or("eleven_multilingual_v2");
        let Some(model) = VoiceModel::parse(model) else {
            let names: Vec<_> = VoiceModel::ALL.iter().map(|m| m.id()).collect();
            return Err(format!("\"model\" must be one of {}", names.join(", ")));
        };
        let text = self.text.unwrap_or_default();
        if text.trim().is_empty() || text.chars().count() > model.max_chars() {
            return Err(format!(
                "\"text\" is required, up to {} characters for {}",
                model.max_chars(),
                model.id()
            ));
        }
        let voice_id = self.voice_id.unwrap_or_default();
        let plain = voice_id.bytes().all(|b| b.is_ascii_alphanumeric());
        if voice_id.is_empty() || voice_id.len() > MAX_VOICE_ID || !plain {
            return Err(
                "\"voice_id\" is required: letters and digits, from GET /v1/speech/voices".into(),
            );
        }
        if let Some(code) = &self.language_code {
            let letters = code.bytes().all(|b| b.is_ascii_lowercase());
            if !letters || !(2..=3).contains(&code.len()) {
                return Err("\"language_code\" must be an ISO 639 code such as \"en\"".into());
            }
        }
        Ok(SpeechJob {
            text,
            voice_id,
            model,
            language_code: self.language_code,
            seed: self.seed,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MusicRequest {
    prompt: Option<String>,
    length_ms: Option<u32>,
    instrumental: Option<bool>,
    seed: Option<u32>,
}

impl MusicRequest {
    fn job(self) -> Result<MusicJob, String> {
        let prompt = self.prompt.unwrap_or_default();
        if prompt.trim().is_empty() || prompt.chars().count() > MAX_MUSIC_PROMPT_CHARS {
            return Err(format!(
                "\"prompt\" is required, up to {MAX_MUSIC_PROMPT_CHARS} characters"
            ));
        }
        let length_ms = self.length_ms.unwrap_or(0);
        if !(MIN_MUSIC_MS..=MAX_MUSIC_MS).contains(&length_ms) {
            return Err(format!(
                "\"length_ms\" is required, {MIN_MUSIC_MS} to {MAX_MUSIC_MS}"
            ));
        }
        Ok(MusicJob {
            prompt,
            length_ms,
            instrumental: self.instrumental.unwrap_or(false),
            seed: self.seed,
        })
    }
}

/// JSON: `text`, `voice_id` (required), `model`, `language_code`, `seed`.
pub async fn speech_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let sounds = match service(&state) {
        Ok(sounds) => sounds,
        Err(response) => return *response,
    };
    let job = match serde_json::from_slice::<SpeechRequest>(&body) {
        Ok(request) => request.job(),
        Err(error) => Err(format!("the body is not a speech request: {error}")),
    };
    let job = match job {
        Ok(job) => job,
        Err(detail) => return bad_request(detail),
    };
    let cost = prices::speech_cost(job.model, job.text.chars().count());
    let quote = Quote {
        amount: prices::with_margin(cost),
        cost,
        resource: resource("/v1/speech", "spoken words as MP3", MP3),
    };
    crate::take_payment(&state, &headers, &quote, || async move {
        answer("speech", sounds.speech(job).await)
    })
    .await
}

/// JSON: `prompt`, `length_ms` (both required), `instrumental`, `seed`.
pub async fn music_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let sounds = match service(&state) {
        Ok(sounds) => sounds,
        Err(response) => return *response,
    };
    let job = match serde_json::from_slice::<MusicRequest>(&body) {
        Ok(request) => request.job(),
        Err(error) => Err(format!("the body is not a music request: {error}")),
    };
    let job = match job {
        Ok(job) => job,
        Err(detail) => return bad_request(detail),
    };
    let cost = prices::music_cost(job.length_ms);
    let quote = Quote {
        amount: prices::with_margin(cost),
        cost,
        resource: resource("/v1/music", "music from a prompt as MP3", MP3),
    };
    crate::take_payment(&state, &headers, &quote, || async move {
        answer("music", sounds.music(job).await)
    })
    .await
}

/// Free: the voices this server's ElevenLabs account can use.
pub async fn voices_route(State(state): State<AppState>) -> Response {
    let sounds = match service(&state) {
        Ok(sounds) => sounds,
        Err(response) => return *response,
    };
    match sounds.voices().await {
        Ok(voices) => Json(serde_json::json!({ "voices": voices })).into_response(),
        Err(error) => providers::problem("voice", "voices", error),
    }
}

fn service(state: &AppState) -> Result<Arc<dyn Sounds>, Box<Response>> {
    state.sounds.clone().ok_or_else(|| {
        Box::new(providers::not_configured(
            "ELEVENLABS_API_KEY",
            "speech or music",
        ))
    })
}

/// Only MP3 is a success; everything else is an error, so it is never settled.
fn answer(route: &'static str, result: Result<Vec<u8>, ProviderError>) -> Response {
    match result {
        Ok(bytes) if is_mp3(&bytes) => ([(CONTENT_TYPE, MP3)], bytes).into_response(),
        Ok(_) => providers::problem(
            "sound",
            route,
            ProviderError::Failed("the answer is not MP3".into()),
        ),
        Err(error) => providers::problem("sound", route, error),
    }
}
