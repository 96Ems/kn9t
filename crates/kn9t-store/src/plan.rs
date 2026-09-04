//! R-STOR-100, R-STOR-110 — plan_request and compaction boundary.

use kn9t_core::{
    breakpoints, CallId, CompactSpan, Content, Message, MsgId, RequestPlan, Role, SeqRange,
    SessionId, StoreErr,
};
use rusqlite::params;

use crate::db::SqliteStore;

/// SPEC-OPEN compaction threshold: 0.80 × ctx_window
const COMPACT_THRESHOLD: f64 = 0.80;

pub fn plan_request(store: &SqliteStore, session: &SessionId) -> Result<RequestPlan, StoreErr> {
    let sid = session.0.clone();
    let model_spec = store.get_model_spec_for_session(&sid);

    struct MsgRow {
        seq: u64,
        role: String,
        content_json: String,
        est_tokens: i64,
    }

    // Scope the lock to just the query - release before resolve_image_blobs
    // to avoid deadlock (get_blob also needs the lock).
    let rows: Vec<MsgRow> = {
        let conn = store
            .conn
            .lock()
            .map_err(|_| StoreErr("lock poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT seq, role, content, est_tokens FROM messages \
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
                est_tokens: r.get(3).unwrap_or(0),
            });
        }
        out
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

    let total_est: i64 = rows.iter().map(|r| r.est_tokens).sum();
    let mut seqs: Vec<u64> = rows.iter().map(|r| r.seq).collect();

    // R-STOR-117 — a durable `args_json` the provider cannot parse would otherwise be
    // replayed on every turn forever. Repaired in the fold for the same reason as
    // R-STOR-115: the log stays honest, the read is made usable.
    repair_unparseable_tool_args(&mut messages);

    // R-STOR-115 — the fold, not the log, closes tool calls the process never lived
    // to answer. Runs before `breakpoints`/`compact_span` so both see the same
    // §7.5-clean message list that the provider will. R-STOR-116: each synthesized
    // result carries whatever `ToolProgress` the dead process had salvaged.
    close_orphan_tool_calls_with(&mut seqs, &mut messages, &|id| {
        store.get_live_tool_progress(session, id).ok().flatten()
    });

    let cache = model_spec
        .as_ref()
        .map(|s| breakpoints(&messages, &s.cache))
        .unwrap_or_default();

    let compact = model_spec.as_ref().and_then(|spec| {
        let threshold = (spec.ctx_window as f64 * COMPACT_THRESHOLD) as i64;
        if total_est >= threshold && messages.len() >= 2 {
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

/// R-STOR-117 → DESIGN §7.5 — replace any `ToolCall::args_json` that is not parseable JSON
/// with an empty object, so the folded message list is one the provider will accept.
///
/// R-PCORE-050 now rejects an incomplete args concat at assemble time, so no *new* message
/// can carry one. But sessions written before that guard have the broken bytes durable in
/// `events`, and append-only (GI-4) means they can never be rewritten: every `plan_request`
/// replays them, litellm/Bedrock fails to convert the tool call, and the turn 500s — the
/// session is unusable forever. Same failure shape as R-STOR-115, so same remedy and same
/// seam: the log keeps the honest record of what the model actually emitted, and the read
/// derives a usable message list from it.
///
/// `{}` is the substitute because the *call* must survive: deleting it would orphan the
/// matching `ToolResult` (§7.5) and drop a turn the transcript already accounts for. The
/// paired result — which for these calls is the tool's own error — carries the real story,
/// so the model is not misled into thinking the call did anything.
///
/// Only invalid values are touched; a valid `args_json` is left byte-identical, preserving
/// key order and the message-level cache (R-CORE-062).
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

/// R-STOR-115 → DESIGN §7.5, §9.1 — close every `ToolCall` in `messages` that has no
/// matching `ToolResult`, by inserting a synthesized `is_error` result immediately after
/// the assistant message that opened it.
///
/// §9.1 makes the loop synthesize these at abort time, but that only covers aborts the
/// loop survives. A killed process (`kill -9`, server restart, panic) leaves the assistant
/// `MessageAppended` durable with no tool-role message after it, and every provider 400s
/// on the orphan — permanently, because the log is append-only (GI-4) so the missing
/// result can never be back-filled.
///
/// The repair therefore belongs in the fold, not the log: `events` keeps the honest
/// record that the call never answered, and `plan_request` derives a §7.5-clean message
/// list from it on every read. `seqs` is kept in step with `messages` so `compact_span`
/// still reports real `SeqRange`s (synthesized messages borrow the opening message's seq).
pub fn close_orphan_tool_calls(seqs: &mut Vec<u64>, messages: &mut Vec<Message>) {
    close_orphan_tool_calls_with(seqs, messages, &|_| None)
}

/// R-STOR-116 — as [`close_orphan_tool_calls`], but `salvage` may supply the partial
/// output the dead process had streamed for a call: `(tool, progress, truncated)`.
///
/// A bare "interrupted" result is honest but useless — the model cannot tell a command
/// that never started from one that ran for two minutes and printed the answer. Replaying
/// the salvaged `ToolProgress` turns the synthesized result from a dead end into a usable
/// (if unverified) observation. It stays `is_error: true` regardless: the tool never
/// confirmed completion, so the content is evidence, not a return value.
pub fn close_orphan_tool_calls_with(
    seqs: &mut Vec<u64>,
    messages: &mut Vec<Message>,
    salvage: &dyn Fn(&CallId) -> Option<(String, String, bool)>,
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
