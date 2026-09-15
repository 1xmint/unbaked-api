//! ElevenLabs speech and music, called over HTTPS. The key only goes in the
//! `xi-api-key` header and is never logged. No sound effects: their
//! Prohibited Use Policy 9(c) bans reselling them as files.

use std::time::Duration;

use serde_json::{Value, json};

use crate::providers::{BoxFuture, MusicJob, ProviderError, Sounds, SpeechJob, Voice, is_mp3};

pub const API_BASE: &str = "https://api.elevenlabs.io/v1";

/// MP3, 44.1 kHz, 128 kbit/s.
pub const OUTPUT_FORMAT: &str = "mp3_44100_128";

/// Ten minutes of music can take several minutes to compose.
const TIMEOUT: Duration = Duration::from_secs(600);

const MESSAGE_LIMIT: usize = 500;

pub struct ElevenLabs {
    client: reqwest::Client,
    key: String,
    base: String,
}

impl ElevenLabs {
    pub fn new(key: &str) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(TIMEOUT).build()?,
            key: key.to_owned(),
            base: API_BASE.to_owned(),
        })
    }

    async fn call(&self, request: reqwest::RequestBuilder) -> Result<Vec<u8>, ProviderError> {
        let response = request
            .header("xi-api-key", &self.key)
            .send()
            .await
            .map_err(transport)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(transport)?;
        match failure(status, &body) {
            Some(error) => Err(error),
            None => Ok(body.to_vec()),
        }
    }

    async fn audio(&self, path: &str, body: Value) -> Result<Vec<u8>, ProviderError> {
        let request = self
            .client
            .post(format!("{}{path}?output_format={OUTPUT_FORMAT}", self.base))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        let bytes = self.call(request).await?;
        if is_mp3(&bytes) {
            Ok(bytes)
        } else {
            Err(ProviderError::Failed("the answer is not MP3".into()))
        }
    }
}

impl Sounds for ElevenLabs {
    fn speech(&self, job: SpeechJob) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        Box::pin(async move {
            let path = format!("/text-to-speech/{}", job.voice_id);
            self.audio(&path, speech_body(&job)).await
        })
    }

    fn music(&self, job: MusicJob) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
        Box::pin(async move { self.audio("/music", music_body(&job)).await })
    }

    fn voices(&self) -> BoxFuture<'_, Result<Vec<Voice>, ProviderError>> {
        Box::pin(async move {
            let request = self.client.get(format!("{}/voices", self.base));
            let body = self.call(request).await?;
            read_voices(&body)
        })
    }
}

pub fn speech_body(job: &SpeechJob) -> Value {
    let mut body = json!({ "text": job.text, "model_id": job.model.id() });
    if let Some(language) = &job.language_code {
        body["language_code"] = json!(language);
    }
    if let Some(seed) = job.seed {
        body["seed"] = json!(seed);
    }
    body
}

pub fn music_body(job: &MusicJob) -> Value {
    let mut body = json!({
        "prompt": job.prompt,
        "music_length_ms": job.length_ms,
        "force_instrumental": job.instrumental,
    });
    if let Some(seed) = job.seed {
        body["seed"] = json!(seed);
    }
    body
}

pub fn read_voices(body: &[u8]) -> Result<Vec<Voice>, ProviderError> {
    let answer: Value = serde_json::from_slice(body)
        .map_err(|_| ProviderError::Failed("the voice list is not JSON".into()))?;
    let voices = answer["voices"]
        .as_array()
        .ok_or_else(|| ProviderError::Failed("the answer has no voice list".into()))?;
    let text = |value: &Value| value.as_str().map(str::to_owned);
    Ok(voices
        .iter()
        .filter_map(|voice| {
            Some(Voice {
                voice_id: text(&voice["voice_id"])?,
                name: text(&voice["name"]).unwrap_or_default(),
                category: text(&voice["category"]),
                description: text(&voice["description"]),
                labels: voice.get("labels").cloned().unwrap_or(Value::Null),
            })
        })
        .collect())
}

/// Why a call failed, or `None` for success.
pub fn failure(status: u16, body: &[u8]) -> Option<ProviderError> {
    match status {
        200..=299 => None,
        401..=403 => Some(ProviderError::Account),
        429 => Some(ProviderError::Busy),
        400..=499 => Some(ProviderError::Refused(message(body))),
        _ => Some(ProviderError::Failed(format!("status {status}"))),
    }
}

/// ElevenLabs puts its reason in `detail`: a string, `{message}`, or a list
/// of `{msg}`.
fn message(body: &[u8]) -> String {
    let answer: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let detail = &answer["detail"];
    let message = detail
        .as_str()
        .or_else(|| detail["message"].as_str())
        .or_else(|| detail[0]["msg"].as_str())
        .unwrap_or("the request was refused");
    message.chars().take(MESSAGE_LIMIT).collect()
}

fn transport(error: reqwest::Error) -> ProviderError {
    let error = error.without_url();
    ProviderError::Failed(if error.is_timeout() {
        "no answer in time".to_owned()
    } else {
        error.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::VoiceModel;

    #[test]
    fn requests_carry_only_what_was_asked() {
        let speech = SpeechJob {
            text: "hello".into(),
            voice_id: "v1".into(),
            model: VoiceModel::V3,
            language_code: None,
            seed: Some(7),
        };
        assert_eq!(
            speech_body(&speech),
            json!({ "text": "hello", "model_id": "eleven_v3", "seed": 7 })
        );
        let music = MusicJob {
            prompt: "quiet piano".into(),
            length_ms: 10_000,
            instrumental: true,
            seed: None,
        };
        assert_eq!(
            music_body(&music),
            json!({ "prompt": "quiet piano", "music_length_ms": 10_000, "force_instrumental": true })
        );
    }

    #[test]
    fn failures_carry_elevenlabs_reason() {
        let detail = json!({ "detail": { "status": "bad_prompt", "message": "names an artist" } });
        assert_eq!(
            failure(400, detail.to_string().as_bytes()),
            Some(ProviderError::Refused("names an artist".into()))
        );
        let list = json!({ "detail": [{ "loc": ["body", "text"], "msg": "field required" }] });
        assert_eq!(
            failure(422, list.to_string().as_bytes()),
            Some(ProviderError::Refused("field required".into()))
        );
        assert_eq!(failure(401, b""), Some(ProviderError::Account));
        assert_eq!(failure(429, b""), Some(ProviderError::Busy));
        assert!(matches!(failure(503, b""), Some(ProviderError::Failed(_))));
        assert_eq!(failure(200, b""), None);
    }

    #[test]
    fn the_voice_list_keeps_ids_and_names() {
        let body = json!({ "voices": [
            { "voice_id": "a1", "name": "Ada", "category": "premade", "labels": { "accent": "british" } },
            { "name": "no id is skipped" }
        ]});
        let voices = read_voices(body.to_string().as_bytes()).unwrap();
        assert_eq!(voices.len(), 1);
        assert_eq!(
            (voices[0].voice_id.as_str(), voices[0].name.as_str()),
            ("a1", "Ada")
        );
    }
}
