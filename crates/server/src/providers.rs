//! The AI services the server resells, behind traits so tests use fakes.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::problem::Problem;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Low,
    Medium,
    High,
}

impl Quality {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureFormat {
    Png,
    Webp,
}

impl PictureFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "png" => Some(Self::Png),
            "webp" => Some(Self::Webp),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }

    pub fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    /// Whether `bytes` start the way this format does.
    pub fn matches(self, bytes: &[u8]) -> bool {
        match self {
            Self::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            Self::Webp => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Background {
    Auto,
    Opaque,
    Transparent,
}

impl Background {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "opaque" => Some(Self::Opaque),
            "transparent" => Some(Self::Transparent),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Opaque => "opaque",
            Self::Transparent => "transparent",
        }
    }
}

/// What to draw.
#[derive(Clone, Debug)]
pub struct PictureJob {
    pub prompt: String,
    pub width: u32,
    pub height: u32,
    pub quality: Quality,
    pub format: PictureFormat,
    pub background: Background,
}

/// What to change, in which pictures.
#[derive(Clone, Debug)]
pub struct EditJob {
    pub job: PictureJob,
    /// Source pictures (PNG, JPEG or WebP), at least one.
    pub images: Vec<Vec<u8>>,
    /// A PNG the size of the first picture; see-through areas mark what to change.
    pub mask: Option<Vec<u8>>,
}

/// Tokens the provider reports, for checking the price table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub text_tokens: u64,
    pub image_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug)]
pub struct Picture {
    pub bytes: Vec<u8>,
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// The provider said no to this request (a safety refusal, a bad picture).
    /// The message is the provider's and is safe to pass on.
    Refused(String),
    /// Our key or account is at fault; the caller cannot fix it.
    Account,
    /// Too many requests right now.
    Busy,
    /// No answer, or one we could not read.
    Failed(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(message) => write!(f, "the provider refused: {message}"),
            Self::Account => write!(f, "the provider rejected this server's account"),
            Self::Busy => write!(f, "the provider is busy"),
            Self::Failed(message) => write!(f, "the provider failed: {message}"),
        }
    }
}

/// A provider failure as problem+json. Every one is an error status, so the
/// payment is never settled. `service` names it for the caller ("picture").
pub fn problem(service: &str, route: &'static str, error: ProviderError) -> Response {
    let (status, code, detail) = match &error {
        ProviderError::Refused(message) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "provider_refused",
            message.clone(),
        ),
        ProviderError::Account => (
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_unavailable",
            format!("the {service} service is not available on this server right now"),
        ),
        ProviderError::Busy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_busy",
            format!("the {service} service is busy; try again shortly"),
        ),
        ProviderError::Failed(message) => {
            (StatusCode::BAD_GATEWAY, "provider_failed", message.clone())
        }
    };
    if !matches!(error, ProviderError::Refused(_)) {
        tracing::warn!(route, %error, "provider call failed");
    }
    Problem::new(status, code, format!("{detail}; nothing was charged")).into_response()
}

/// 503 for a service with no key, before any price is asked.
pub fn not_configured(key: &str, what: &str) -> Response {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "provider_not_configured",
        format!("this server has no {key}, so it cannot make {what}"),
    )
    .into_response()
}

pub trait Pictures: Send + Sync {
    fn generate(&self, job: PictureJob) -> BoxFuture<'_, Result<Picture, ProviderError>>;
    fn edit(&self, job: EditJob) -> BoxFuture<'_, Result<Picture, ProviderError>>;
}

/// ElevenLabs speech models we sell, each with its own limit and price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceModel {
    MultilingualV2,
    V3,
    FlashV2_5,
}

impl VoiceModel {
    pub const ALL: [Self; 3] = [Self::MultilingualV2, Self::V3, Self::FlashV2_5];

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|model| model.id() == value)
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::MultilingualV2 => "eleven_multilingual_v2",
            Self::V3 => "eleven_v3",
            Self::FlashV2_5 => "eleven_flash_v2_5",
        }
    }

    /// The most characters ElevenLabs takes in one request.
    pub fn max_chars(self) -> usize {
        match self {
            Self::MultilingualV2 => 10_000,
            Self::V3 => 5_000,
            Self::FlashV2_5 => 40_000,
        }
    }
}

/// Words to say.
#[derive(Clone, Debug)]
pub struct SpeechJob {
    pub text: String,
    pub voice_id: String,
    pub model: VoiceModel,
    pub language_code: Option<String>,
    pub seed: Option<u32>,
}

/// Music to compose.
#[derive(Clone, Debug)]
pub struct MusicJob {
    pub prompt: String,
    pub length_ms: u32,
    pub instrumental: bool,
    pub seed: Option<u32>,
}

/// A voice the caller may pick.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Voice {
    pub voice_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub labels: serde_json::Value,
}

/// Whether `bytes` look like MP3: an ID3 tag or an MPEG frame header.
pub fn is_mp3(bytes: &[u8]) -> bool {
    bytes.starts_with(b"ID3") || (bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0)
}

pub trait Sounds: Send + Sync {
    /// MP3 bytes.
    fn speech(&self, job: SpeechJob) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>>;
    /// MP3 bytes.
    fn music(&self, job: MusicJob) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>>;
    fn voices(&self) -> BoxFuture<'_, Result<Vec<Voice>, ProviderError>>;
}

#[cfg(feature = "fake")]
pub mod fake {
    //! Providers that answer from memory.

    use std::sync::Mutex;

    use super::*;

    /// A 1×1 transparent PNG.
    pub const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[derive(Default)]
    pub struct FakePictures {
        refusal: Mutex<Option<ProviderError>>,
        jobs: Mutex<Vec<PictureJob>>,
        edits: Mutex<Vec<EditJob>>,
    }

    impl FakePictures {
        pub fn new() -> Self {
            Self::default()
        }

        /// Every later call fails with `error`.
        pub fn fail_with(&self, error: ProviderError) {
            *self.refusal.lock().unwrap() = Some(error);
        }

        /// Generate calls so far.
        pub fn jobs(&self) -> Vec<PictureJob> {
            self.jobs.lock().unwrap().clone()
        }

        /// Edit calls so far.
        pub fn edits(&self) -> Vec<EditJob> {
            self.edits.lock().unwrap().clone()
        }

        fn answer(&self) -> Result<Picture, ProviderError> {
            match self.refusal.lock().unwrap().clone() {
                Some(error) => Err(error),
                None => Ok(Picture {
                    bytes: TINY_PNG.to_vec(),
                    usage: Some(Usage::default()),
                }),
            }
        }
    }

    /// One silent MPEG-1 Layer III frame (128 kbit/s, 44.1 kHz).
    pub fn tiny_mp3() -> Vec<u8> {
        let mut frame = vec![0; 417];
        frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x64]);
        frame
    }

    #[derive(Default)]
    pub struct FakeSounds {
        refusal: Mutex<Option<ProviderError>>,
        speeches: Mutex<Vec<SpeechJob>>,
        songs: Mutex<Vec<MusicJob>>,
    }

    impl FakeSounds {
        pub fn new() -> Self {
            Self::default()
        }

        /// Every later call fails with `error`.
        pub fn fail_with(&self, error: ProviderError) {
            *self.refusal.lock().unwrap() = Some(error);
        }

        pub fn speeches(&self) -> Vec<SpeechJob> {
            self.speeches.lock().unwrap().clone()
        }

        pub fn songs(&self) -> Vec<MusicJob> {
            self.songs.lock().unwrap().clone()
        }

        fn answer(&self) -> Result<Vec<u8>, ProviderError> {
            match self.refusal.lock().unwrap().clone() {
                Some(error) => Err(error),
                None => Ok(tiny_mp3()),
            }
        }
    }

    impl Sounds for FakeSounds {
        fn speech(&self, job: SpeechJob) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
            self.speeches.lock().unwrap().push(job);
            Box::pin(async move { self.answer() })
        }

        fn music(&self, job: MusicJob) -> BoxFuture<'_, Result<Vec<u8>, ProviderError>> {
            self.songs.lock().unwrap().push(job);
            Box::pin(async move { self.answer() })
        }

        fn voices(&self) -> BoxFuture<'_, Result<Vec<Voice>, ProviderError>> {
            Box::pin(async move {
                self.answer().map(|_| {
                    vec![Voice {
                        voice_id: "fake-voice".to_owned(),
                        name: "Fake".to_owned(),
                        category: Some("premade".to_owned()),
                        description: None,
                        labels: serde_json::json!({ "accent": "none" }),
                    }]
                })
            })
        }
    }

    impl Pictures for FakePictures {
        fn generate(&self, job: PictureJob) -> BoxFuture<'_, Result<Picture, ProviderError>> {
            self.jobs.lock().unwrap().push(job);
            Box::pin(async move { self.answer() })
        }

        fn edit(&self, job: EditJob) -> BoxFuture<'_, Result<Picture, ProviderError>> {
            self.edits.lock().unwrap().push(job);
            Box::pin(async move { self.answer() })
        }
    }
}
