//! Free, local Unbaked file work, mirroring the `unbaked` CLI. Paths may name
//! a packed file or a folder, as the CLI accepts both.

use std::path::Path;

use serde_json::{Value as Json, json};
use unbaked_core::bake::Change;
use unbaked_core::edit::{self, EditError};
use unbaked_core::json::Problem;
use unbaked_core::package::Limits;
use unbaked_core::{Status, open, pack};
use unbaked_render::preview::{self, PreviewOptions};
use unbaked_render::{Deadline, NoFonts, RenderError, RenderLimits};

/// Layer 1's agent guide, copied at the pinned commit.
pub const GUIDE: &str = include_str!("../guide.md");

/// Limits for one local job, matching the server's.
pub fn limits(ms: u64) -> RenderLimits {
    RenderLimits {
        max_pixels: 36_000_000,
        max_samples: 28_800_000,
        max_frames: 18_000,
        deadline: Some(Deadline::after_ms(ms)),
    }
}

pub const PREVIEW_MS: u64 = 20_000;
pub const LISTEN_MS: u64 = 20_000;
pub const RENDER_MS: u64 = 60_000;

/// The package inside a file, or a folder in the directory form.
pub fn load(path: &Path) -> Result<pack::Files, String> {
    if path.is_dir() {
        pack::read_folder(path, Limits::default())
            .map_err(|error| format!("{}: {error}", path.display()))
    } else {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        pack::read_files(&bytes, Limits::default())
            .map_err(|error| format!("{}: {error}", path.display()))
    }
}

/// Writes changed package files back: into the file's package, or into the
/// folder file by file. With `output`, a file goes to `output` and a folder
/// to the new folder `output`.
pub fn save(
    input: &Path,
    before: &pack::Files,
    after: &pack::Files,
    output: Option<&Path>,
) -> Result<Json, String> {
    let target = output.unwrap_or(input);
    if input.is_dir() {
        if let Some(output) = output {
            pack::write_folder(after, output).map_err(|error| error.to_string())?;
        } else {
            for (name, data) in after {
                if before.get(name) != Some(data) {
                    let path = input.join(name);
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    write_replacing(&path, data)?;
                }
            }
            for name in before.keys().filter(|name| !after.contains_key(*name)) {
                let path = input.join(name);
                std::fs::remove_file(&path).map_err(|error| error.to_string())?;
            }
        }
    } else {
        let original =
            std::fs::read(input).map_err(|error| format!("{}: {error}", input.display()))?;
        let package = pack::write(after, Limits::default()).map_err(|error| error.to_string())?;
        let result =
            pack::with_package(&original, &package).map_err(|error| error.to_string())?;
        write_replacing(target, &result)?;
    }
    Ok(json!({ "ok": true, "output": target.display().to_string() }))
}

/// Writes to a temporary file beside `path`, then renames it over `path`.
pub fn write_replacing(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut temp_name = path.file_name().unwrap_or_default().to_owned();
    temp_name.push(".unbaked-tmp");
    let temp = path.with_file_name(temp_name);
    let written = std::fs::File::create(&temp).and_then(|mut file| {
        use std::io::Write;
        file.write_all(bytes).and_then(|()| file.sync_all())
    });
    let result = written.and_then(|()| std::fs::rename(&temp, path));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("{}: {error}", path.display()));
    }
    Ok(())
}

pub fn check(path: &Path) -> Result<Json, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let opened = open(&bytes, Limits::default())
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let status = opened.check();
    let mut report = json!({ "ok": true });
    if let (Some(all), Json::Object(fields)) = (report.as_object_mut(), status_json(&status)) {
        all.extend(fields);
    }
    Ok(report)
}

fn change_json(change: &Change) -> Json {
    match change {
        Change::Recipe => json!({ "kind": "recipe" }),
        Change::Carrier => json!({ "kind": "carrier" }),
        Change::AssetChanged(p) => json!({ "kind": "asset-changed", "path": p }),
        Change::AssetAdded(p) => json!({ "kind": "asset-added", "path": p }),
        Change::AssetRemoved(p) => json!({ "kind": "asset-removed", "path": p }),
    }
}

fn problems_json<'a>(file: &'a str, problems: &'a [Problem]) -> impl Iterator<Item = Json> + 'a {
    problems
        .iter()
        .map(move |p| json!({ "file": file, "path": p.path, "message": p.message }))
}

fn status_json(status: &Status) -> Json {
    match status {
        Status::Fresh => json!({ "status": "fresh" }),
        Status::RenderModified => json!({ "status": "render-modified" }),
        Status::Stale(changes) => json!({
            "status": "stale",
            "changes": changes.iter().map(change_json).collect::<Vec<_>>(),
        }),
        Status::Invalid { recipe, bake } => json!({
            "status": "invalid",
            "problems": problems_json("recipe.json", recipe)
                .chain(problems_json("bake.json", bake))
                .collect::<Vec<_>>(),
        }),
    }
}

pub fn estimate(path: &Path) -> Result<Json, String> {
    let files = load(path)?;
    let e = unbaked_render::estimate::estimate(&files).map_err(render_error)?;
    let kind = match e.kind {
        unbaked_core::recipe::OutputKind::Image => "image",
        unbaked_core::recipe::OutputKind::Video => "video",
        unbaked_core::recipe::OutputKind::Audio => "audio",
    };
    Ok(json!({
        "ok": true,
        "kind": kind,
        "canvas_pixels": e.canvas_pixels,
        "frames": e.frames,
        "layers": e.layers,
        "extra_buffers": e.extra_buffers,
        "blurs": e.blurs,
        "max_blur_radius": e.max_blur_radius,
        "image_pixels": e.image_pixels,
        "video_pixels_per_frame": e.video_pixels_per_frame,
        "output_samples": e.output_samples,
        "source_samples": e.source_samples,
        "asset_bytes": e.asset_bytes,
        "work_units": e.work_units,
    }))
}

pub fn listen(path: &Path) -> Result<Json, String> {
    let files = load(path)?;
    let stats = preview::listen(&files, limits(LISTEN_MS)).map_err(render_error)?;
    let level = |db: f64| {
        if db.is_finite() {
            json!((db * 10.0).round() / 10.0)
        } else {
            Json::Null
        }
    };
    Ok(json!({
        "ok": true,
        "duration_ms": stats.duration_ms,
        "sample_rate": stats.sample_rate,
        "channels": stats.channels,
        "peak_dbfs": level(stats.peak_dbfs),
        "clipped_samples": stats.clipped_samples,
        "window_ms": preview::WINDOW_MS,
        "loudness_dbfs": stats.loudness_dbfs.iter().map(|&db| level(db)).collect::<Vec<_>>(),
        "silences": stats.silences.iter().map(|s| json!({"start_ms": s.start_ms, "end_ms": s.end_ms})).collect::<Vec<_>>(),
    }))
}

/// A PNG preview, and its JSON report.
pub fn preview(path: &Path, options: PreviewOptions) -> Result<(Vec<u8>, Json), String> {
    let files = load(path)?;
    let pixels =
        preview::preview(&files, &NoFonts, limits(PREVIEW_MS), options).map_err(render_error)?;
    let png = unbaked_render::image::encode_png(pixels.width, pixels.height, &pixels.to_rgba8())
        .map_err(|error| format!("encoding the preview: {error}"))?;
    let report = json!({
        "ok": true,
        "width": pixels.width,
        "height": pixels.height,
    });
    Ok((png, report))
}

pub fn render(path: &Path, output: Option<&Path>) -> Result<Json, String> {
    let (files, out) = if path.is_dir() {
        let output = output.ok_or_else(|| "render of a folder needs an output path".to_owned())?;
        (
            pack::read_folder(path, Limits::default()).map_err(|error| error.to_string())?,
            output,
        )
    } else {
        (load(path)?, output.unwrap_or(path))
    };
    let file = unbaked_render::render(&files, &NoFonts, limits(RENDER_MS)).map_err(render_error)?;
    write_replacing(out, &file)?;
    Ok(json!({ "ok": true, "output": out.display().to_string(), "bytes": file.len() }))
}

pub fn edit(path: &Path, patch: &[u8], output: Option<&Path>) -> Result<Json, String> {
    let before = load(path)?;
    let after = edit::edit(&before, patch).map_err(edit_error)?;
    save(path, &before, &after, output)
}

pub fn add(
    path: &Path,
    id: &str,
    media: &[u8],
    license: Option<Json>,
    output: Option<&Path>,
) -> Result<Json, String> {
    let before = load(path)?;
    let after = edit::add_asset(&before, id, media, license).map_err(edit_error)?;
    save(path, &before, &after, output)
}

fn render_error(error: RenderError) -> String {
    error.to_string()
}

fn edit_error(error: EditError) -> String {
    error.to_string()
}
