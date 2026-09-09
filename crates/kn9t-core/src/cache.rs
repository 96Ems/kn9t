//! R-CORE-200, R-CORE-210 — cache placement types and the pure `breakpoints()` fn.

use crate::message::{Message, Role};
use serde::{Deserialize, Serialize};

/// R-CORE-200
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum Cache {
    /// System prompt; tools share this prefix.
    System,
    /// Index into `Request::messages`.
    AfterMessage { idx: usize },
}

/// R-CORE-200
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

/// R-CORE-210 — provider-independent cache-breakpoint selection, mirroring the
/// opencode plugin's `applyCaching`.
///
/// 1. returns empty for `CacheMode::None`;
/// 2. for `Automatic`, uses default breakpoints (4 max);
/// 3. builds candidates in this exact order: `System`, `AfterMessage(last_user)`,
///    `AfterMessage(len-2)`, `AfterMessage(len-1)`, skipping any that don't exist;
/// 4. deduplicates positions;
/// 5. returns the first `max_breakpoints` survivors, **in priority order — NOT
///    sorted by position**.
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

