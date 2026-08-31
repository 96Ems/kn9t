//! write tool — writes content to a file, with stale-read detection for existing files.
//!
//! For new files: creates the file and emits content as diff-style additions.
//! For existing files: requires prior read (like edit) to prevent accidental overwrites.

use kn9t_plugin_sdk::{ctx::ToolCallCtx, traits::{PluginTool, ToolOutput}, wire::ToolSpec};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::read::read_map;

pub struct Write;

impl PluginTool for Write {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Write content to a file. For existing files, the file must have been \
                read first ('read' tool) and must not have been modified since. \
                New files are created directly. Line endings are preserved for existing files."
                .into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path":    { "type": "string", "description": "Path to the file." },
                    "content": { "type": "string", "description": "Content to write to the file." }
                },
                "required": ["path", "content"]
            }),
            parallel_safe: false,
            hidden: false,
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(p),
            None => return ToolOutput::error("missing 'path'"),
        };
        let content = match args.get("content").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::error("missing 'content'"),
        };

        if ctx.cancel.is_cancelled() {
            return ToolOutput::error("cancelled");
        }

        let is_new_file = !path.exists();

        // For existing files: stale-read check
        if !is_new_file {
            let map = read_map();
            let map = map.lock().unwrap();
            if let Some((_sha, tracked_mtime)) = map.get(&path) {
                let current_mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                if current_mtime > *tracked_mtime {
                    return ToolOutput::error(
                        "file was modified since last read — re-read it before writing",
                    );
                }
            } else {
                return ToolOutput::error(
                    "file exists but has not been read — use 'read' before 'write' on existing files",
                );
            }
        }

        // Write the file
        if let Err(e) = std::fs::write(&path, content.as_bytes()) {
            return ToolOutput::error(format!("write error: {e}"));
        }

        // Update tracking entry
        {
            let map = read_map();
            let mut map = map.lock().unwrap();
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let sha = crate::read::sha256_pub(content.as_bytes());
            map.insert(path.clone(), (sha, mtime));
        }

        // Emit content via progress for TUI display
        let path_str = path.display().to_string();
        emit_write_diff(ctx, &path_str, &content, is_new_file);

        ToolOutput::text(format!("wrote {} bytes to {}", content.len(), path_str))
    }
}

/// Emit file content via ctx.progress for TUI display.
/// For new files: shows as unified diff additions.
/// For overwrites: shows content with line numbers.
fn emit_write_diff(ctx: &ToolCallCtx, path: &str, content: &str, is_new_file: bool) {
    let lines: Vec<&str> = content.lines().collect();
    
    if is_new_file {
        // Show as diff-style additions for new file
        ctx.progress.send(&format!("--- /dev/null"));
        ctx.progress.send(&format!("+++ b/{}", path));
        ctx.progress.send(&format!("@@ -0,0 +1,{} @@", lines.len()));
        for line in &lines {
            ctx.progress.send(&format!("+{}", line));
        }
    } else {
        // Show content with line numbers for overwrite
        ctx.progress.send(&format!("── {} (overwritten) ──", path));
        for (i, line) in lines.iter().enumerate() {
            ctx.progress.send(&format!("{:>4}: {}", i + 1, line));
        }
    }
}
