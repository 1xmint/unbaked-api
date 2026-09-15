//! The AI services the server resells, behind traits so tests use fakes.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

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

pub trait Pictures: Send + Sync {
    fn generate(&self, job: PictureJob) -> BoxFuture<'_, Result<Picture, ProviderError>>;
    fn edit(&self, job: EditJob) -> BoxFuture<'_, Result<Picture, ProviderError>>;
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
