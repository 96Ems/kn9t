//! read tool — reads file content; records (path → sha256, mtime) for edit's stale check.
//!
//! R-PLUG2-130: read-tracking map lives inside kn9t-tools, shared with edit via READ_MAP.
//!
//! Supports reading images (png, jpg, gif, webp, svg) — returns them as base64 data URIs
//! so the model can see the image content directly.

use base64::Engine;
use kn9t_plugin_sdk::{ctx::ToolCallCtx, traits::{ContentBlock, PluginTool, ToolOutput}, wire::{DefaultPolicy, Effect, EffectKind, ToolPolicy, ToolSpec}};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// Process-global read-tracking map: path → (sha256, mtime).
pub static READ_MAP: OnceLock<Arc<Mutex<HashMap<PathBuf, ([u8; 32], SystemTime)>>>> =
    OnceLock::new();

pub fn read_map() -> Arc<Mutex<HashMap<PathBuf, ([u8; 32], SystemTime)>>> {
    Arc::clone(READ_MAP.get_or_init(|| Arc::new(Mutex::new(HashMap::new()))))
}

/// Record a path as read, hashing its current bytes.
///
/// Any tool that puts a file's content in front of the model satisfies the same
/// precondition as `read`, so it must be able to register the path — otherwise
/// `edit` demands a redundant `read` round-trip for content the model already has.
/// Returns false when the path is unreadable (not a file, permission denied), in
/// which case nothing is recorded and the stale guard still applies.
pub fn track_as_read(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    read_map()
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), (sha256(&bytes), mtime));
    true
}

/// Image extensions we support reading as visual content.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg"];

/// Max image size to inline (10 MB). Larger images get an error.
const MAX_IMAGE_SIZE: usize = 10 * 1024 * 1024;

/// Check if a path is an image file based on extension.
fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Get MIME type for an image extension.
fn mime_for_extension(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub struct Read;

impl PluginTool for Read {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read a file or directory. For files: returns contents with line numbers. \
                For directories: lists entries (folders first, with trailing /). \
                For images (png, jpg, gif, webp, svg): returns the image so you can see it. \
                Tracks file hash and mtime so 'edit' can detect stale reads."
                .into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to file or directory." },
                    "offset": { "type": "integer", "description": "Line number to start from (1-indexed)." },
                    "limit":  { "type": "integer", "description": "Maximum number of lines to read." }
                },
                "required": ["path"]
            }),
            parallel_safe: true,
            hidden: false,
            effects: vec![Effect { field: "path".into(), kind: EffectKind::FsRead }],
            // Read is safe — auto-allow by default
            policy: ToolPolicy {
                pattern_field: Some("path".into()),
                default_policy: DefaultPolicy::Allow,
                builtin_allow: vec![],
                builtin_deny: vec![],
            },
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(p),
            None => return ToolOutput::error("missing 'path' argument"),
        };
        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
        let limit  = args.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize);

        if ctx.cancel.is_cancelled() {
            return ToolOutput::error("cancelled");
        }

        // If it's a directory, list its contents
        if path.is_dir() {
            return list_directory(&path, limit);
        }

        // Check if it's an image file
        if is_image_file(&path) {
            return read_image(&path);
        }

        let content = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("read error: {e}")),
        };

        // Record sha256 + mtime for edit's stale check.
        let sha = sha256(&content);
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        read_map().lock().unwrap().insert(path.clone(), (sha, mtime));

        let text = decode_text(&content);
        let lines: Vec<&str> = text.lines().collect();
        let start = (offset - 1).min(lines.len());
        let end = limit.map(|l| (start + l).min(lines.len())).unwrap_or(lines.len());
        let slice: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{}: {l}", start + i + 1))
            .collect();

        ToolOutput::text(slice.join("\n"))
    }
}

/// Read an image file and return it as a base64 data URI.
fn read_image(path: &Path) -> ToolOutput {
    let content = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return ToolOutput::error(format!("read error: {e}")),
    };

    if content.len() > MAX_IMAGE_SIZE {
        return ToolOutput::error(format!(
            "image too large: {} bytes (max {} bytes)",
            content.len(),
            MAX_IMAGE_SIZE
        ));
    }

    // Record sha256 + mtime for edit's stale check (images can be edited too).
    let sha = sha256(&content);
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    read_map().lock().unwrap().insert(path.to_path_buf(), (sha, mtime));

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png");
    let mime = mime_for_extension(ext);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&content);
    let data_uri = format!("data:{};base64,{}", mime, b64);

    // Return image with a text description
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("image");
    let size_kb = content.len() / 1024;
    
    ToolOutput::blocks(vec![
        ContentBlock::Image {
            sha256: data_uri,
            mime: mime.to_string(),
        },
        ContentBlock::Text {
            text: format!("[{} - {} KB]", filename, size_kb),
        },
    ])
}

/// Public alias so edit.rs can reuse the same hash function.
pub fn sha256_pub(data: &[u8]) -> [u8; 32] { sha256(data) }

/// Decode bytes to text, handling UTF-8, UTF-16 LE/BE (with or without BOM).
fn decode_text(data: &[u8]) -> String {
    // UTF-16 LE BOM
    if data.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16_le(&data[2..]);
    }
    // UTF-16 BE BOM
    if data.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16_be(&data[2..]);
    }
    // UTF-8 BOM
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&data[3..]).into_owned();
    }
    // Heuristic: if lots of null bytes interleaved, likely UTF-16 LE without BOM
    if data.len() >= 4 && data[1] == 0 && data[3] == 0 {
        return decode_utf16_le(data);
    }
    // Default: UTF-8
    String::from_utf8_lossy(data).into_owned()
}

fn decode_utf16_le(data: &[u8]) -> String {
    let u16s: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16s)
}

fn decode_utf16_be(data: &[u8]) -> String {
    let u16s: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16s)
}

fn sha256(data: &[u8]) -> [u8; 32] {
    // Simple FNV-based 32-byte hash — good enough for stale detection.
    // (We avoid external crypto deps to keep the plugin minimal.)
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h = h.wrapping_mul(0x100000001b3).wrapping_add(b as u64);
    }
    let mut out = [0u8; 32];
    out[..8].copy_from_slice(&h.to_le_bytes());
    // Repeat the pattern — sufficient for collision detection in practice.
    out[8..16].copy_from_slice(&h.rotate_left(17).to_le_bytes());
    out[16..24].copy_from_slice(&h.rotate_left(31).to_le_bytes());
    out[24..32].copy_from_slice(&h.rotate_left(47).to_le_bytes());
    out
}

/// List directory contents, directories first, with trailing `/` for dirs.
fn list_directory(path: &PathBuf, limit: Option<usize>) -> ToolOutput {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => return ToolOutput::error(format!("read_dir error: {e}")),
    };

    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            dirs.push(format!("{name}/"));
        } else {
            files.push(name);
        }
    }

    dirs.sort();
    files.sort();

    let mut all: Vec<String> = dirs;
    all.extend(files);

    let end = limit.map(|l| l.min(all.len())).unwrap_or(all.len());
    let slice = &all[..end];

    ToolOutput::text(slice.join("\n"))
}
