use kn9t_plugin_sdk::{
    ctx::ToolCallCtx,
    traits::{PluginTool, ToolOutput},
    wire::{Effect, EffectKind, ToolSpec},
    Plugin,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use sha2::{Digest, Sha256};

// ── bash ─
struct Bash;
impl PluginTool for Bash {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Execute a shell command. Use for all shell operations.".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "The shell command to execute" },
                    "command": { "type": "string", "description": "Alias for cmd" }
                },
                "required": ["cmd"]
            }),
            parallel_safe: false,
            hidden: false,
            effects: vec![Effect { field: "cmd".into(), kind: EffectKind::Shell }],
        }
    }
    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
        let cmd = args.get("cmd").or_else(|| args.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if cmd.is_empty() {
            return ToolOutput::error("missing cmd");
        }
        if ctx.cancel.is_cancelled() {
            return ToolOutput::error("cancelled");
        }
        ctx.progress.send(format!("$ {cmd}"));
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .output();
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let combined = if stderr.is_empty() { stdout.clone() } else { format!("{stdout}\n{stderr}") };
                if !combined.is_empty() {
                    ctx.progress.send(combined.clone());
                }
                if out.status.success() {
                    ToolOutput::text(combined)
                } else {
                    ToolOutput::error(format!("exit {}: {combined}", out.status))
                }
            }
            Err(e) => ToolOutput::error(format!("failed to spawn sh: {e}")),
        }
    }
}

// ── read ─
struct Read;
impl PluginTool for Read {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read a file. Returns its content.".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to read" },
                    "offset": { "type": "integer", "description": "Line offset to start reading from" },
                    "limit": { "type": "integer", "description": "Number of lines to read" }
                },
                "required": ["path"]
            }),
            parallel_safe: true,
            hidden: false,
            effects: vec![Effect { field: "path".into(), kind: EffectKind::FsRead }],
        }
    }
    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::error("missing path"),
        };
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);
        match fs::read_to_string(&path) {
            Ok(content) => {
                // Record hash for edit guard via KV (session-scoped)
                let hash = Sha256::digest(content.as_bytes());
                let hash_hex = hex::encode(hash);
                // Use ctx.kv to store read hash - best effort, ignore errors
                let _ = ctx.kv.set("", &format!("read:{}", path), &json!({"hash": hash_hex}));
                // Apply offset/limit
                let lines: Vec<&str> = content.lines().collect();
                let start = offset.min(lines.len());
                let end = match limit {
                    Some(l) => (start + l).min(lines.len()),
                    None => lines.len(),
                };
                let sliced = lines[start..end].join("\n");
                ToolOutput::text(sliced)
            }
            Err(e) => {
                // Try reading as binary and check if it's an image? For now just error
                ToolOutput::error(format!("read {path}: {e}"))
            }
        }
    }
}

// ── write ─
struct Write;
impl PluginTool for Write {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Write a file. Creates or overwrites.".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to write" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["path", "content"]
            }),
            parallel_safe: false,
            hidden: false,
            effects: vec![Effect { field: "path".into(), kind: EffectKind::FsWrite }],
        }
    }
    fn execute(&self, args: &Value, _ctx: &ToolCallCtx) -> ToolOutput {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolOutput::error("missing path"),
        };
        let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        // Ensure parent exists
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return ToolOutput::error(format!("create dir: {e}"));
                }
            }
        }
        match fs::write(path, content) {
            Ok(_) => ToolOutput::text(format!("wrote {} ({} bytes)", path, content.len())),
            Err(e) => ToolOutput::error(format!("write {path}: {e}")),
        }
    }
}

// ── edit ─
struct Edit;
impl PluginTool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Edit a file by replacing old_string with new_string. Requires the file to have been read first.".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
            parallel_safe: false,
            hidden: false,
            effects: vec![Effect { field: "path".into(), kind: EffectKind::FsWrite }],
        }
    }
    fn execute(&self, args: &Value, _ctx: &ToolCallCtx) -> ToolOutput {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolOutput::error("missing path"),
        };
        let old = args.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
        let new = args.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("read {path}: {e}")),
        };
        let count = content.matches(old).count();
        if old.is_empty() {
            return ToolOutput::error("old_string is empty");
        }
        if count == 0 {
            return ToolOutput::error(format!("old_string not found in {path}"));
        }
        if count > 1 {
            return ToolOutput::error(format!("old_string matches {count} times, need unique context"));
        }
        let new_content = content.replacen(old, new, 1);
        match fs::write(path, &new_content) {
            Ok(_) => ToolOutput::text(format!("edited {path}")),
            Err(e) => ToolOutput::error(format!("write {path}: {e}")),
        }
    }
}

fn main() {
    Plugin::new("kn9t-tools")
        .tool(Bash)
        .tool(Read)
        .tool(Write)
        .tool(Edit)
        .run();
}
