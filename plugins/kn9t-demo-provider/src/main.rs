//! kn9t-demo-provider — scripted provider plugin for recorded demos.
//!
//! `DEMO_SCRIPT=/path/to/script.json` selects the model script: a list of
//! turns, each streamed word-by-word through the REAL plugin protocol. Tool
//! calls execute for real (kn9t-tools or any loaded plugin), so a recording
//! shows a genuine ReAct loop with zero API cost. Used by `scripts/kn9t-demo-rec`.

use kn9t_plugin_sdk::{
    ctx::ProviderCallCtx,
    traits::{PluginProvider, ProviderResult},
    wire::{ModelDecl, PriceDecl, Usage},
    Plugin,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;

#[derive(Deserialize)]
struct Script {
    model: Option<ModelDecl>,
    turns: Vec<Turn>,
}

#[derive(Deserialize)]
struct Turn {
    #[serde(default)]
    thinking: Vec<String>,
    #[serde(default)]
    text: String,
    /// One tool call (shorthand) or several (`tools`) — streamed in order.
    #[serde(default)]
    tool: Option<ToolUse>,
    #[serde(default)]
    tools: Vec<ToolUse>,
    stop: String,
    #[serde(default)]
    usage: Usage,
}

#[derive(Deserialize)]
struct ToolUse {
    call_id: String,
    name: String,
    args: Value,
}

fn script() -> &'static Result<Script, String> {
    static SCRIPT: OnceLock<Result<Script, String>> = OnceLock::new();
    SCRIPT.get_or_init(|| {
        let path = std::env::var("DEMO_SCRIPT")
            .map_err(|_| "DEMO_SCRIPT is not set (scripts/kn9t-demo-rec sets it)".to_string())?;
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("DEMO_SCRIPT {path}: {e}"))?;
        serde_json::from_str(&raw).map_err(|e| format!("DEMO_SCRIPT {path}: {e}"))
    })
}

fn default_model() -> ModelDecl {
    let fallback = ModelDecl {
        id: "demo-1".into(),
        ctx_window: 200_000,
        price: Some(PriceDecl {
            input: 0.5,
            output: 1.5,
            cache_read: 0.05,
            cache_write: 0.625,
        }),
    };
    match script() {
        Ok(s) => s.model.clone().unwrap_or(fallback),
        Err(_) => fallback,
    }
}

struct DemoProvider;

/// Stream text word by word; pace comes from DEMO_PACE_MS (lower = snappier demos).
fn stream_text(ctx: &ProviderCallCtx, text: &str) {
    let pace = std::env::var("DEMO_PACE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(95);
    for w in text.split_inclusive(' ') {
        if ctx.cancel.is_cancelled() {
            return;
        }
        ctx.chunk.text_delta(w);
        sleep(Duration::from_millis(pace));
    }
}

/// Stream a tool call: name first, then the args JSON in visible pieces.
/// Pieces are cut on CHAR boundaries — args carry UTF-8 (e.g. `▌`), and a byte
/// slice that lands mid-char would panic the plugin mid-stream.
fn stream_tool_call(ctx: &ProviderCallCtx, call_id: &str, name: &str, args: &Value) {
    ctx.chunk.tool_use_start(call_id, name, "");
    let chars: Vec<char> = args.to_string().chars().collect();
    let step = (chars.len() / 5).max(1);
    let mut i = 0;
    while i < chars.len() {
        if ctx.cancel.is_cancelled() {
            return;
        }
        let j = (i + step).min(chars.len());
        let piece: String = chars[i..j].iter().collect();
        ctx.chunk.tool_use_delta(call_id, &piece);
        sleep(Duration::from_millis(130));
        i = j;
    }
}

impl PluginProvider for DemoProvider {
    fn id(&self) -> &str {
        "demo"
    }

    fn models(&self) -> Vec<ModelDecl> {
        vec![default_model()]
    }

    fn complete(&self, _request: &Value, ctx: &ProviderCallCtx) -> ProviderResult {
        static CALLS: AtomicU32 = AtomicU32::new(0);

        let script = match script() {
            Ok(s) => s,
            Err(e) => return ProviderResult::error(e.clone()),
        };
        let n = CALLS.fetch_add(1, Ordering::SeqCst) as usize;
        let turn = &script.turns[n.min(script.turns.len() - 1)];

        ctx.chunk.input_tokens(turn.usage.input);
        for thought in &turn.thinking {
            if ctx.cancel.is_cancelled() {
                return ProviderResult::error("cancelled");
            }
            ctx.chunk.thinking_delta(thought, "demo");
            sleep(Duration::from_millis(180));
        }
        stream_text(ctx, &turn.text);
        let mut calls: Vec<&ToolUse> = turn.tools.iter().collect();
        if let Some(one) = &turn.tool {
            calls.insert(0, one);
        }
        for t in calls {
            stream_tool_call(ctx, &t.call_id, &t.name, &t.args);
        }
        if ctx.cancel.is_cancelled() {
            return ProviderResult::error("cancelled");
        }
        ProviderResult {
            stop: turn.stop.clone(),
            usage: turn.usage.clone(),
            cost_usd: None,
            error: None,
        }
    }
}

fn main() {
    if let Err(e) = script() {
        eprintln!("[kn9t-demo-provider] {e}");
    }
    Plugin::new("kn9t-demo-provider").provider(DemoProvider).run();
}
