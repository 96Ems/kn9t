//! Cache placement: where to insert cache breakpoints and how to select them.

use crate::message::{Message, Role};
use serde::{Deserialize, Serialize};

/// Cache breakpoint location: system prompt or after a message.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum Cache {
    /// System prompt; tools share this prefix.
    System,
    /// Index into `Request::messages`.
    AfterMessage { idx: usize },
}

/// Cache mode: explicit breakpoints, automatic selection, or disabled.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum CacheMode {
    Explicit {
        max_breakpoints: u8,
        min_tokens: u32,
    },
    Automatic,
    None,
}

/// Selects cache breakpoints from messages: system prompt, last user message, and final messages.
/// Returns breakpoints in priority order (not position order); deduplicates and respects max limits.
pub fn breakpoints(messages: &[Message], mode: &CacheMode) -> Vec<Cache> {
    let max_breakpoints: u8 = match mode {
        CacheMode::Explicit {
            max_breakpoints, ..
        } => *max_breakpoints,
        CacheMode::Automatic => 4, // Default: system + last_user + last 2 messages
        CacheMode::None => return vec![],
    };
    let last_user = messages.iter().rposition(|m| m.role == Role::User);
    let len = messages.len();
    let candidates = [
        Some(Cache::System),
        last_user.map(|idx| Cache::AfterMessage { idx }),
        len.checked_sub(2).map(|idx| Cache::AfterMessage { idx }),
        len.checked_sub(1).map(|idx| Cache::AfterMessage { idx }),
    ];
    let mut out = Vec::new();
    for c in candidates.into_iter().flatten() {
        if out.contains(&c) {
            continue;
        }
        out.push(c);
        if out.len() == max_breakpoints as usize {
            break;
        }
    }
    out
}

