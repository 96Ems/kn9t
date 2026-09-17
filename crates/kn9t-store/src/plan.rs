//! R-STOR-100, R-STOR-110 — plan_request and compaction boundary.

use kn9t_core::{
    breakpoints, CallId, CompactSpan, Content, Message, MsgId, RequestPlan, Role, SeqRange,
    SessionId, StoreErr,
};
use rusqlite::params;

use crate::db::SqliteStore;

/// Callback to salvage partial tool output: (output_text, status, is_error).
type SalvageCallback<'a> = &'a dyn Fn(&CallId) -> Option<(String, String, bool)>;

/// SPEC-OPEN compaction threshold: 0.80 × ctx_window
const COMPACT_THRESHOLD: f64 = 0.80;

pub fn plan_request(store: &SqliteStore, session: &SessionId) -> Result<RequestPlan, StoreErr> {
    let sid = session.0.clone();
    let model_spec = store.get_model_spec_for_session(&sid);

    struct MsgRow {
        seq: u64,
        role: String,
        content_json: String,
    }

    // Query last usage (real tokens from provider) and messages in one lock scope.
    let (rows, last_usage_seq, last_real_tokens): (Vec<MsgRow>, u64, i64) = {
        let conn = store
            .conn
            .lock()
            .map_err(|_| StoreErr("lock poisoned".into()))?;

        // Get the last usage record: its tokens_in is the real prompt size at that turn
        let (last_seq, real_tokens): (u64, i64) = conn
            .query_row(
                "SELECT seq, tokens_in FROM usage WHERE session_id=?1 ORDER BY seq DESC LIMIT 1",
                params![sid],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
            )
            .unwrap_or((0, 0)); // No usage yet = first turn

        let mut stmt = conn
            .prepare(
                "SELECT seq, role, content FROM messages \
             WHERE session_id=?1 ORDER BY seq",
            )
            .map_err(|e| StoreErr(format!("plan prepare: {e}")))?;
        let mut out = Vec::new();
        let mut result = stmt
            .query(params![sid])
            .map_err(|e| StoreErr(format!("plan query: {e}")))?;
        while let Some(r) = result
            .next()
            .map_err(|e| StoreErr(format!("plan row: {e}")))?
        {
            out.push(MsgRow {
                seq: r.get::<_, i64>(0).unwrap_or(0) as u64,
                role: r.get(1).unwrap_or_default(),
                content_json: r.get(2).unwrap_or_default(),
            });
        }
        (out, last_seq, real_tokens)
    }; // conn lock released here

    // Now safe to call resolve_image_blobs (which calls get_blob -> needs lock)
    let mut messages: Vec<Message> = rows
        .iter()
        .map(|r| {
            let content: Vec<Content> = serde_json::from_str(&r.content_json).unwrap_or_default();
            // Resolve blob references to inline base64 for provider compatibility.
            let content = resolve_image_blobs(store, content);
            Message {
                id: MsgId::new(),
                role: parse_role(&r.role),
                content,
                silent: false,
            }
        })
        .collect();

    // Token count: real tokens from last provider response + estimate new messages not yet sent.
    let new_messages_est: i64 = rows
        .iter()
        .filter(|r| r.seq > last_usage_seq)
        .map(|r| crate::project::estimate_tokens_json(&r.content_json))
        .sum();
    let total_tokens: i64 = last_real_tokens + new_messages_est;

    let mut seqs: Vec<u64> = rows.iter().map(|r| r.seq).collect();

    // R-STOR-117 — a durable `args_json` the provider cannot parse would otherwise be
    // replayed on every turn forever. Repaired in the fold for the same reason as
    // R-STOR-115: the log stays honest, the read is made usable.
    repair_unparseable_tool_args(&mut messages);

    // R-STOR-115/116 — the fold closes tool calls the process never answered, before
    // `breakpoints`/`compact_span` so all three see the same §7.5-clean list; each synthesized
    // result carries whatever progress the dead process had salvaged.
    close_orphan_tool_calls_with(&mut seqs, &mut messages, &|id| {
        store.get_live_tool_progress(session, id).ok().flatten()
    });

    let cache = model_spec
        .as_ref()
        .map(|s| breakpoints(&messages, &s.cache))
        .unwrap_or_default();

    let compact = model_spec.as_ref().and_then(|spec| {
        let threshold = (spec.ctx_window as f64 * COMPACT_THRESHOLD) as i64;
        if total_tokens >= threshold && messages.len() >= 2 {
            Some(compact_span(&seqs, &messages))
        } else {
            None
        }
    });

    Ok(RequestPlan {
        system: None,
        messages,
        tools: vec![],
        cache,
        compact,
    })
}

fn parse_role(s: &str) -> Role {
    match s {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

/// Resolve `Content::Image` blob references (`sha256:...`) to inline base64 data URIs.
/// This makes images compatible with all providers (OpenAI, Anthropic, etc.).
fn resolve_image_blobs(store: &SqliteStore, content: Vec<Content>) -> Vec<Content> {
    use base64::Engine;

    content
        .into_iter()
        .map(|c| {
            match c {
                Content::Image { sha256, mime } => {
                    // Extract hash from "sha256:<hex>" format.
                    let hash = sha256.strip_prefix("sha256:").unwrap_or(&sha256);
                    eprintln!("[resolve_image_blobs] resolving hash={}", hash);

                    // Try to load blob data from store.
                    match store.get_blob(hash) {
                        Ok(Some((data, stored_mime))) => {
                            let mime = if mime.is_empty() { stored_mime } else { mime };
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                            eprintln!(
                                "[resolve_image_blobs] resolved: {} bytes -> {} base64 chars",
                                data.len(),
                                b64.len()
                            );
                            Content::Image {
                                sha256: format!("data:{};base64,{}", mime, b64),
                                mime,
                            }
                        }
                        Ok(None) => {
                            eprintln!("[resolve_image_blobs] blob not found for hash={}", hash);
                            Content::Image { sha256, mime }
                        }
                        Err(e) => {
                            eprintln!("[resolve_image_blobs] error loading blob: {:?}", e);
                            Content::Image { sha256, mime }
                        }
                    }
                }
                other => other,
            }
        })
        .collect()
}

/// R-STOR-110 — pick the oldest ~half, snap boundary to avoid orphaned ToolCall/Result pairs.
pub fn compact_span(seqs: &[u64], messages: &[Message]) -> CompactSpan {
    let n = messages.len();
    let mut cut = n / 2;
    loop {
        if cut >= n {
            break;
        }
        if has_orphan_tool_call(&messages[..cut]) {
            cut += 1;
        } else {
            break;
        }
    }
    let replaced_msgs = messages[..cut].to_vec();
    let start_seq = seqs.first().copied().unwrap_or(1);
    let end_seq = if cut > 0 { seqs[cut - 1] } else { start_seq };
    CompactSpan {
        replaced: SeqRange {
            start: start_seq,
            end: end_seq,
        },
        messages: replaced_msgs,
    }
}

/// R-STOR-117 → DESIGN §7.5 — replace unparseable `ToolCall::args_json` with `{}` so the provider
/// accepts the folded list. Pre-R-PCORE-050 sessions have the broken bytes durable and append-only
/// (GI-4), so every replay would fail forever. `{}` keeps the call (deleting it would orphan its
/// result) and only invalid values change, preserving key order and the cache (R-CORE-062).
fn repair_unparseable_tool_args(messages: &mut [Message]) {
    for m in messages.iter_mut() {
        for c in m.content.iter_mut() {
            if let Content::ToolCall { args_json, .. } = c {
                if serde_json::from_str::<serde_json::Value>(args_json).is_err() {
                    *args_json = "{}".to_string();
                }
            }
        }
    }
}

/// R-STOR-115 → DESIGN §7.5, §9.1 — close every `ToolCall` with no matching `ToolResult` by
/// inserting a synthesized `is_error` result after its assistant message. §9.1 covers aborts the
/// loop survives; a killed process leaves the orphan durable and append-only (GI-4), so the repair
/// lives in the fold, not the log. `seqs` stays in step for `compact_span`.
pub fn close_orphan_tool_calls(seqs: &mut Vec<u64>, messages: &mut Vec<Message>) {
    close_orphan_tool_calls_with(seqs, messages, &|_| None)
}

/// R-STOR-116 — as [`close_orphan_tool_calls`], but `salvage` may supply the partial output the
/// dead process streamed, turning a bare "interrupted" into a usable (if unverified)
/// observation. It stays `is_error: true`: the tool never confirmed completion.
pub fn close_orphan_tool_calls_with(
    seqs: &mut Vec<u64>,
    messages: &mut Vec<Message>,
    salvage: SalvageCallback<'_>,
) {
    let answered: std::collections::HashSet<CallId> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|c| match c {
            Content::ToolResult { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();

    // Walk backwards so each insert cannot shift an index still to be visited.
    for i in (0..messages.len()).rev() {
        let orphans: Vec<CallId> = messages[i]
            .content
            .iter()
            .filter_map(|c| match c {
                Content::ToolCall { id, .. } if !answered.contains(id) => Some(id.clone()),
                _ => None,
            })
            .collect();
        if orphans.is_empty() {
            continue;
        }
        let content = orphans
            .into_iter()
            .map(|id| {
                let text = match salvage(&id) {
                    Some((_, progress, truncated)) if !progress.trim().is_empty() => {
                        let elision = if truncated { "\n[earlier output dropped]\n" } else { "\n" };
                        format!("{INTERRUPTED_TOOL_RESULT}\nPartial output before the interruption:{elision}{progress}")
                    }
                    _ => INTERRUPTED_TOOL_RESULT.to_string(),
                };
                Content::ToolResult {
                    id,
                    content: vec![Content::Text { text }],
                    is_error: true,
                }
            })
            .collect();
        messages.insert(
            i + 1,
            Message {
                id: MsgId::new(),
                role: Role::Tool,
                content,
                silent: false,
            },
        );
        seqs.insert(i + 1, seqs.get(i).copied().unwrap_or(1));
    }
}

/// The text a synthesized result carries. Distinct from §9.1's "aborted by user": this
/// call did not merely get cancelled, the process died before it could report anything.
const INTERRUPTED_TOOL_RESULT: &str =
    "Tool call interrupted: kn9t exited before this tool reported a result. \
     The call may or may not have taken effect — verify before relying on it.";

/// True if any ToolCall in `msgs` has no matching ToolResult in `msgs`.
pub fn has_orphan_tool_call(msgs: &[Message]) -> bool {
    for m in msgs {
        for c in &m.content {
            if let Content::ToolCall { id, .. } = c {
                let has_result = msgs.iter().any(|m2| {
                    m2.content
                        .iter()
                        .any(|c2| matches!(c2, Content::ToolResult { id: rid, .. } if rid == id))
                });
                if !has_result {
                    return true;
                }
            }
        }
    }
    false
}
