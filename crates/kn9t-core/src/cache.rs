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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MsgId;
    use crate::message::Content;

    fn msg(role: Role) -> Message {
        Message {
            id: MsgId::new(),
            role,
            content: vec![Content::Text {
                text: "test".into(),
            }],
            silent: false,
        }
    }

    #[test]
    fn test_breakpoints_none_mode_returns_empty() {
        let messages = vec![msg(Role::User), msg(Role::Assistant)];
        let result = breakpoints(&messages, &CacheMode::None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_breakpoints_automatic_includes_system() {
        let messages = vec![msg(Role::User), msg(Role::Assistant)];
        let result = breakpoints(&messages, &CacheMode::Automatic);
        assert!(result.contains(&Cache::System));
    }

    #[test]
    fn test_breakpoints_automatic_includes_last_user() {
        let messages = vec![
            msg(Role::User),
            msg(Role::Assistant),
            msg(Role::User), // idx 2 - last user
            msg(Role::Assistant),
        ];
        let result = breakpoints(&messages, &CacheMode::Automatic);
        assert!(result.contains(&Cache::AfterMessage { idx: 2 }));
    }

    #[test]
    fn test_breakpoints_respects_max_breakpoints() {
        let messages = vec![
            msg(Role::User),
            msg(Role::Assistant),
            msg(Role::User),
            msg(Role::Assistant),
            msg(Role::User),
        ];
        let mode = CacheMode::Explicit {
            max_breakpoints: 2,
            min_tokens: 0,
        };
        let result = breakpoints(&messages, &mode);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_breakpoints_deduplicates() {
        // When last_user == len-1, those positions are the same
        let messages = vec![msg(Role::User)]; // idx 0 is both last_user and len-1
        let result = breakpoints(&messages, &CacheMode::Automatic);
        // Should not have duplicates - check manually
        // With 1 message at idx 0: candidates are System, AfterMessage{0}, AfterMessage{-1 invalid}, AfterMessage{0}
        // After dedup should be System, AfterMessage{0}
        assert!(
            result.len() <= 2,
            "should have at most 2 unique breakpoints"
        );

        // Verify no adjacent duplicates by manual check
        for i in 1..result.len() {
            assert!(result[i] != result[i - 1], "adjacent duplicates found");
        }
    }

    #[test]
    fn test_breakpoints_empty_messages() {
        let messages: Vec<Message> = vec![];
        let result = breakpoints(&messages, &CacheMode::Automatic);
        // Should only have System (no message-based breakpoints possible)
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], Cache::System));
    }

    #[test]
    fn test_breakpoints_priority_order() {
        let messages = vec![
            msg(Role::User),      // idx 0
            msg(Role::Assistant), // idx 1
            msg(Role::User),      // idx 2 - last user
            msg(Role::Assistant), // idx 3
        ];
        let result = breakpoints(&messages, &CacheMode::Automatic);
        // Priority order: System, last_user(2), len-2(2 - dedup), len-1(3)
        assert!(matches!(result[0], Cache::System));
        assert!(matches!(result[1], Cache::AfterMessage { idx: 2 })); // last_user
    }

    #[test]
    fn test_cache_mode_explicit() {
        let mode = CacheMode::Explicit {
            max_breakpoints: 1,
            min_tokens: 100,
        };
        let messages = vec![msg(Role::User), msg(Role::Assistant)];
        let result = breakpoints(&messages, &mode);
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], Cache::System)); // First priority
    }
}
