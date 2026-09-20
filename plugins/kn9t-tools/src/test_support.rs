//! Test-only helpers: a `ToolCallCtx` with no host behind it.
//!
//! The SDK's context structs have public fields and `for_test()` constructors
//! precisely so an external plugin can call `execute()` directly. Without this
//! every tool test would have to go through a live host to exercise logic that
//! is pure file I/O.

use kn9t_plugin_sdk::ctx::{CancelToken, HostApiClient, KvClient, ProgressSender, ToolCallCtx};
use kn9t_plugin_sdk::traits::{ContentBlock, ToolOutput};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A context whose progress sink discards output and whose host is unreachable.
pub fn ctx(cwd: Option<PathBuf>) -> ToolCallCtx {
    ToolCallCtx {
        cancel: CancelToken::new(),
        progress: ProgressSender {
            id: 0,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
        },
        kv: KvClient::for_test(),
        host: HostApiClient::for_test(),
        cwd,
    }
}

/// The text of a tool result, joined across content blocks.
pub fn text(out: &ToolOutput) -> String {
    out.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A unique scratch directory for one test, created empty.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kn9t_tools_test_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}
