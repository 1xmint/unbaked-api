//! The MCP tools: paid ones pay the `unbaked-api` server with x402; free ones
//! run against it or run Unbaked file work locally.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::Value;

use crate::fonts::FontFolder;
use crate::local;
use crate::pay::{Paid, PayError, Payer};

pub struct Server {
    payer: Payer,
    output_dir: PathBuf,
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Server>,
}

fn text(message: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(message.into())])
}

fn error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

fn json_result(value: Value) -> CallToolResult {
    text(serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()))
}

/// A plain file name only: no separators, no "..".
fn plain_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains(['/', '\\']) || name.split('.').any(|part| part == "..") {
        return Err("save_as must be a plain file name: no separators, no \"..\"".to_owned());
    }
    Ok(())
}

/// Writes `bytes` under `dir` as `save_as`, or `<tool>-<unix-ms>.<ext>`.
/// Never overwrites: a name already taken gets a counter.
fn save_bytes(
    dir: &Path,
    save_as: Option<&str>,
    tool: &str,
    ext: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let name = match save_as {
        Some(name) => {
            plain_name(name)?;
            name.to_owned()
        }
        None => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            format!("{tool}-{now}.{ext}")
        }
    };
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let path = Path::new(&name);
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    let mut candidate = dir.join(&name);
    let mut n = 1u32;
    while candidate.exists() {
        candidate = dir.join(match &ext {
            Some(ext) => format!("{stem}-{n}.{ext}"),
            None => format!("{stem}-{n}"),
        });
        n += 1;
    }
    std::fs::write(&candidate, bytes).map_err(|error| error.to_string())?;
    Ok(candidate)
}

fn font_source(fonts: Option<&str>) -> Box<dyn unbaked_render::FontSource> {
    match fonts {
        Some(dir) => Box::new(FontFolder::new(PathBuf::from(dir))),
        None => Box::new(unbaked_render::NoFonts),
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct GenerateImage {
    /// What to draw.
    pub prompt: String,
    /// WIDTHxHEIGHT, e.g. "1024x1024".
    pub size: Option<String>,
    pub quality: Option<String>,
    /// "png" or "webp".
    pub format: Option<String>,
    pub background: Option<String>,
    /// The file name to save the picture as. Defaults to a generated name.
    pub save_as: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct EditImage {
    pub prompt: String,
    /// Paths to source pictures (PNG, JPEG or WebP), 1 to 16.
    pub images: Vec<String>,
    /// Path to a PNG mask; see-through areas mark what to change.
    pub mask: Option<String>,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub format: Option<String>,
    pub background: Option<String>,
    pub save_as: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct Speech {
    pub text: String,
    /// From the `voices` tool.
    pub voice_id: String,
    pub model: Option<String>,
    pub language_code: Option<String>,
    pub seed: Option<u32>,
    pub save_as: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct Music {
    pub prompt: String,
    pub length_ms: u32,
    pub instrumental: Option<bool>,
    pub seed: Option<u32>,
    pub save_as: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct PathArg {
    /// A packed Unbaked file, or an unpacked folder.
    pub path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct PreviewArgs {
    pub path: String,
    pub output: Option<String>,
    pub at_ms: Option<u64>,
    pub max_edge: Option<u32>,
    pub sheet: Option<u32>,
    /// A folder of fonts, looked up by SHA-256.
    pub fonts: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct EditArgs {
    pub path: String,
    /// A JSON Patch (RFC 6902) on `recipe.json`.
    pub patch: Value,
    pub output: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    pub path: String,
    /// Path to the media file to pack as an asset.
    pub media: String,
    pub id: String,
    pub license: Option<Value>,
    pub output: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RenderArgs {
    pub path: String,
    pub output: Option<String>,
    pub fonts: Option<String>,
}

#[tool_router]
impl Server {
    pub fn new(payer: Payer, output_dir: PathBuf) -> Self {
        Self {
            payer,
            output_dir,
            tool_router: Self::tool_router(),
        }
    }

    pub fn payer(&self) -> &Payer {
        &self.payer
    }

    #[tool(
        description = "Make a picture from a text prompt. Costs money; refuses over the session cap."
    )]
    pub async fn generate_image(
        &self,
        Parameters(args): Parameters<GenerateImage>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut body = serde_json::Map::new();
        body.insert("prompt".into(), args.prompt.into());
        if let Some(v) = args.size {
            body.insert("size".into(), v.into());
        }
        if let Some(v) = args.quality {
            body.insert("quality".into(), v.into());
        }
        if let Some(v) = args.format.clone() {
            body.insert("format".into(), v.into());
        }
        if let Some(v) = args.background {
            body.insert("background".into(), v.into());
        }
        let bytes = serde_json::to_vec(&Value::Object(body)).unwrap_or_default();
        let ext = if args.format.as_deref() == Some("webp") {
            "webp"
        } else {
            "png"
        };
        Ok(self
            .paid_binary(
                "/v1/images/generate",
                "application/json",
                bytes,
                "generate_image",
                ext,
                args.save_as.as_deref(),
            )
            .await)
    }

    #[tool(
        description = "Change a picture with a text prompt. Costs money; refuses over the session cap."
    )]
    pub async fn edit_image(
        &self,
        Parameters(args): Parameters<EditImage>,
    ) -> Result<CallToolResult, ErrorData> {
        if args.images.is_empty() {
            return Ok(error("send 1 to 16 \"images\" paths"));
        }
        const BOUNDARY: &str = "unbaked-mcp-boundary";
        let mut body = Vec::new();
        let mut part = |name: &str, value: &str| {
            body.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
        };
        part("prompt", &args.prompt);
        if let Some(v) = &args.size {
            part("size", v);
        }
        if let Some(v) = &args.quality {
            part("quality", v);
        }
        if let Some(v) = &args.format {
            part("format", v);
        }
        if let Some(v) = &args.background {
            part("background", v);
        }
        let mut file_part = |name: &str, path: &str, bytes: &[u8]| -> std::io::Result<()> {
            body.extend_from_slice(
                format!(
                    "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{path}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
            Ok(())
        };
        for image in &args.images {
            let bytes = match std::fs::read(image) {
                Ok(bytes) => bytes,
                Err(err) => return Ok(error(format!("{image}: {err}"))),
            };
            if file_part("image", image, &bytes).is_err() {
                return Ok(error(format!("{image}: could not read")));
            }
        }
        if let Some(mask) = &args.mask {
            let bytes = match std::fs::read(mask) {
                Ok(bytes) => bytes,
                Err(err) => return Ok(error(format!("{mask}: {err}"))),
            };
            if file_part("mask", mask, &bytes).is_err() {
                return Ok(error(format!("{mask}: could not read")));
            }
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
        let ext = if args.format.as_deref() == Some("webp") {
            "webp"
        } else {
            "png"
        };
        Ok(self
            .paid_binary(
                "/v1/images/edit",
                &content_type,
                body,
                "edit_image",
                ext,
                args.save_as.as_deref(),
            )
            .await)
    }

    #[tool(
        description = "Speak text in a chosen voice as MP3. Costs money; refuses over the session cap."
    )]
    pub async fn speech(
        &self,
        Parameters(args): Parameters<Speech>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = serde_json::json!({
            "text": args.text,
            "voice_id": args.voice_id,
            "model": args.model,
            "language_code": args.language_code,
            "seed": args.seed,
        });
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        Ok(self
            .paid_binary(
                "/v1/speech",
                "application/json",
                bytes,
                "speech",
                "mp3",
                args.save_as.as_deref(),
            )
            .await)
    }

    #[tool(
        description = "Compose music from a prompt as MP3. Costs money; refuses over the session cap."
    )]
    pub async fn music(
        &self,
        Parameters(args): Parameters<Music>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = serde_json::json!({
            "prompt": args.prompt,
            "length_ms": args.length_ms,
            "instrumental": args.instrumental,
            "seed": args.seed,
        });
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        Ok(self
            .paid_binary(
                "/v1/music",
                "application/json",
                bytes,
                "music",
                "mp3",
                args.save_as.as_deref(),
            )
            .await)
    }

    #[tool(description = "List the voices available for the speech tool. Free.")]
    pub async fn voices(&self) -> Result<CallToolResult, ErrorData> {
        match self.payer.get("/v1/speech/voices").await {
            Ok(response) => Ok(self.answer_json(response).await),
            Err(err) => Ok(error(err.message())),
        }
    }

    #[tool(description = "Layer 1's agent guide: how Unbaked files work. Free, local.")]
    pub async fn guide(&self) -> Result<CallToolResult, ErrorData> {
        Ok(text(local::GUIDE))
    }

    #[tool(
        description = "Is the file's render fresh, stale, render-modified or invalid? Free, local."
    )]
    pub async fn check(
        &self,
        Parameters(args): Parameters<PathArg>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(local::check(Path::new(&args.path)).map_or_else(error, json_result))
    }

    #[tool(
        description = "How big a render is: pixels, frames, samples, and a price estimate. Free, local."
    )]
    pub async fn estimate(
        &self,
        Parameters(args): Parameters<PathArg>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(local::estimate(Path::new(&args.path)).map_or_else(error, json_result))
    }

    #[tool(description = "Loudness and silences of the file's sound. Free, local.")]
    pub async fn listen(
        &self,
        Parameters(args): Parameters<PathArg>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(local::listen(Path::new(&args.path)).map_or_else(error, json_result))
    }

    #[tool(description = "Draw one moment of an image or video recipe as a PNG. Free, local.")]
    pub async fn preview(
        &self,
        Parameters(args): Parameters<PreviewArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let options = unbaked_render::preview::PreviewOptions {
            at_ms: args.at_ms,
            max_edge: args
                .max_edge
                .unwrap_or_else(|| unbaked_render::preview::PreviewOptions::default().max_edge),
            sheet: args.sheet,
        };
        let path = Path::new(&args.path);
        let (png, mut report) = match local::preview(path, options) {
            Ok(result) => result,
            Err(err) => return Ok(error(err)),
        };
        let ext = "png";
        let saved = match save_bytes(
            &self.output_dir,
            args.output.as_deref(),
            "preview",
            ext,
            &png,
        ) {
            Ok(path) => path,
            Err(err) => return Ok(error(err)),
        };
        if let Value::Object(fields) = &mut report {
            fields.insert("output".into(), saved.display().to_string().into());
        }
        let _ = font_source(args.fonts.as_deref()); // a font folder is honoured once fonts are referenced by id
        Ok(CallToolResult::success(vec![
            ContentBlock::text(serde_json::to_string_pretty(&report).unwrap_or_default()),
            ContentBlock::image(STANDARD.encode(&png), "image/png"),
        ]))
    }

    #[tool(description = "Apply a JSON Patch (RFC 6902) to recipe.json. Free, local.")]
    pub async fn edit(
        &self,
        Parameters(args): Parameters<EditArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let patch = serde_json::to_vec(&args.patch).unwrap_or_default();
        let path = Path::new(&args.path);
        let output = args.output.as_deref().map(Path::new);
        Ok(local::edit(path, &patch, output).map_or_else(error, json_result))
    }

    #[tool(description = "Pack a media file as an asset and point an asset id at it. Free, local.")]
    pub async fn add(
        &self,
        Parameters(args): Parameters<AddArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let media = match std::fs::read(&args.media) {
            Ok(bytes) => bytes,
            Err(err) => return Ok(error(format!("{}: {err}", args.media))),
        };
        let path = Path::new(&args.path);
        let output = args.output.as_deref().map(Path::new);
        Ok(
            local::add(path, &args.id, &media, args.license, output)
                .map_or_else(error, json_result),
        )
    }

    #[tool(description = "Render the recipe and write a fresh Unbaked file. Free, local.")]
    pub async fn render(
        &self,
        Parameters(args): Parameters<RenderArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = Path::new(&args.path);
        let output = args.output.as_deref().map(Path::new);
        let _ = font_source(args.fonts.as_deref());
        Ok(local::render(path, output).map_or_else(error, json_result))
    }
}

impl Server {
    /// A paid POST that saves the successful answer's bytes and reports the
    /// price. Any refusal or failure comes back as a tool error.
    async fn paid_binary(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
        tool: &str,
        ext: &str,
        save_as: Option<&str>,
    ) -> CallToolResult {
        if let Some(name) = save_as
            && let Err(message) = plain_name(name)
        {
            return error(message);
        }
        match self.payer.post(path, content_type, body).await {
            Ok(Paid::Free(response)) => self.answer_bytes(response, tool, ext, save_as).await,
            Ok(Paid::Settled {
                response,
                paid,
                remaining,
                transaction,
            }) => {
                let bytes = match response.bytes().await {
                    Ok(bytes) => bytes,
                    Err(err) => return error(err.to_string()),
                };
                let saved = match save_bytes(&self.output_dir, save_as, tool, ext, &bytes) {
                    Ok(path) => path,
                    Err(message) => return error(message),
                };
                let mut report = serde_json::json!({
                    "output": saved.display().to_string(),
                    "bytes": bytes.len(),
                    "paid_usd": crate::config::dollars(paid),
                    "remaining_usd": crate::config::dollars(remaining),
                });
                if let (Some(transaction), Value::Object(fields)) = (transaction, &mut report) {
                    fields.insert("transaction".into(), transaction.into());
                }
                json_result(report)
            }
            Ok(Paid::WorkFailed(response)) => self.problem_error(response).await,
            Err(err) => error(err.message()),
        }
    }

    async fn answer_bytes(
        &self,
        response: reqwest::Response,
        tool: &str,
        ext: &str,
        save_as: Option<&str>,
    ) -> CallToolResult {
        if !response.status().is_success() {
            return self.problem_error(response).await;
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => return error(err.to_string()),
        };
        match save_bytes(&self.output_dir, save_as, tool, ext, &bytes) {
            Ok(path) => json_result(
                serde_json::json!({ "output": path.display().to_string(), "bytes": bytes.len() }),
            ),
            Err(message) => error(message),
        }
    }

    async fn answer_json(&self, response: reqwest::Response) -> CallToolResult {
        if !response.status().is_success() {
            return self.problem_error(response).await;
        }
        match response.text().await {
            Ok(text_body) => text(text_body),
            Err(err) => error(err.to_string()),
        }
    }

    async fn problem_error(&self, response: reqwest::Response) -> CallToolResult {
        let status = response.status();
        let detail = response
            .text()
            .await
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .and_then(|value| {
                value
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("the server answered {status}"));
        error(detail)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Make pictures and sounds by paying the unbaked-api server with x402 (needs \
                 UNBAKED_WALLET_KEY and stays under UNBAKED_SESSION_CAP_USD), and do Unbaked \
                 file work locally for free: check, estimate, preview, listen, edit, add, render.",
            )
    }
}

impl From<PayError> for ErrorData {
    fn from(error: PayError) -> Self {
        ErrorData::internal_error(error.message(), None)
    }
}
