//! Referenced fonts from a folder the user names (SPEC.md section 4.12).

use std::cell::OnceCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use unbaked_core::sha256_hex;
use unbaked_render::FontSource;

/// Font files larger than this are skipped.
const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;

/// Font files (`.ttf`, `.otf`, `.ttc`, `.otc`) in a folder and its subfolders,
/// found by SHA-256. The folder is only read when a font is first asked for.
/// Links are not followed.
pub struct FontFolder {
    dir: PathBuf,
    by_hash: OnceCell<HashMap<String, PathBuf>>,
}

impl FontFolder {
    pub fn new(dir: PathBuf) -> FontFolder {
        FontFolder {
            dir,
            by_hash: OnceCell::new(),
        }
    }

    fn index(&self) -> &HashMap<String, PathBuf> {
        self.by_hash.get_or_init(|| {
            let mut found = HashMap::new();
            let mut pending = vec![self.dir.clone()];
            while let Some(dir) = pending.pop() {
                let Ok(entries) = fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let Ok(kind) = entry.file_type() else {
                        continue;
                    };
                    let path = entry.path();
                    if kind.is_dir() {
                        pending.push(path);
                    } else if kind.is_file() && is_font_name(&path) {
                        let small = entry.metadata().is_ok_and(|m| m.len() <= MAX_FONT_BYTES);
                        if let (true, Ok(data)) = (small, fs::read(&path)) {
                            found.entry(sha256_hex(&data)).or_insert(path);
                        }
                    }
                }
            }
            found
        })
    }
}

fn is_font_name(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["ttf", "otf", "ttc", "otc"].contains(&e.to_ascii_lowercase().as_str()))
}

impl FontSource for FontFolder {
    fn find(&self, sha256: &str) -> Option<Vec<u8>> {
        fs::read(self.index().get(sha256)?).ok()
    }
}
