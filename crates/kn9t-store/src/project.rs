//! R-STOR-060, R-STOR-070 — pure `project(event)` function.

use kn9t_core::{Content, Event, Role, StoreErr, UsageKind};
use rusqlite::{params, Connection};

pub enum Row {
    Message {
        session_id: String,
        seq: u64,
        role: String,
        content_json: String,
        est_tokens: i64,
        silent: bool,
    },
    Usage {
        session_id: String,
        seq: u64,
        ts: i64,
        provider: String,
        model: String,
        kind: String,
        tokens_in: i64,
        tokens_out: i64,
        cache_read: i64,
        cache_write: i64,
        reasoning: i64,
        price_in: f64,
        price_out: f64,
        price_cache_read: f64,
        price_cache_write: f64,
        price_in_micros: i64,
        price_out_micros: i64,
        price_cache_read_micros: i64,
        price_cache_write_micros: i64,
        cost_usd: f64,
        cost_micros: i64,
        estimated: i64,
    },
    Compacted {
        session_id: String,
        seq: u64,
        replaced_start: u64,
        replaced_end: u64,
        role: String,
        content_json: String,
        est_tokens: i64,
    },
}

/// Estimate tokens as len/4 (§7.4 — no tokenizer).
fn est_tokens(content: &[Content]) -> i64 {
    let total: usize = content
        .iter()
        .map(|c| match c {
            Content::Text { text } => text.len(),
            Content::ToolCall {
                name, args_json, ..
            } => name.len() + args_json.len(),
            Content::ToolResult { content, .. } => content
                .iter()
                .map(|x| match x {
                    Content::Text { text } => text.len(),
                    _ => 20,
                })
                .sum(),
            Content::Thinking { text, .. } => text.len(),
            Content::Image { .. } => 1000,
        })
        .sum();
    (total / 4).max(1) as i64
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn usage_kind_str(kind: UsageKind) -> &'static str {
    match kind {
        UsageKind::Main => "main",
        UsageKind::Compaction => "compaction",
        UsageKind::Subagent => "subagent",
        UsageKind::Title => "title",
    }
}

/// R-STOR-060 — project a single durable event into rows.
pub fn project(session_id: &str, ts: i64, event: &Event) -> Vec<Row> {
    match event {
        Event::MessageAppended { seq, msg } => {
            let content_json = serde_json::to_string(&msg.content).unwrap_or_default();
            let est = est_tokens(&msg.content);
            vec![Row::Message {
                session_id: session_id.to_owned(),
                seq: *seq,
                role: role_str(msg.role).to_owned(),
                content_json,
                est_tokens: est,
                silent: msg.silent,
            }]
        }
        Event::UsageRecorded {
            seq,
            provider,
            model,
            kind,
            tokens,
            price_snapshot,
            cost_micros,
            estimated,
            ..
        } => {
            // R-STOR-070 — deterministic integer cost (96E-14). Use the event's cost_micros if present,
            // otherwise compute from tokens*price/1e6 via integer arithmetic.
            let computed_micros = kn9t_core::cost_micros(tokens, price_snapshot);
            let micros = if *cost_micros != 0 {
                *cost_micros
            } else {
                computed_micros
            };
            // Keep old REAL columns for migration/compat: derive dollars from micros
            let cost_usd = micros as f64 / 1_000_000.0;
            vec![Row::Usage {
                session_id: session_id.to_owned(),
                seq: *seq,
                ts,
                provider: provider.clone(),
                model: model.clone(),
                kind: usage_kind_str(*kind).to_owned(),
                tokens_in: tokens.input as i64,
                tokens_out: tokens.output as i64,
                cache_read: tokens.cache_read as i64,
                cache_write: tokens.cache_write as i64,
                reasoning: tokens.reasoning as i64,
                price_in: price_snapshot.input as f64 / 1_000_000.0,
                price_out: price_snapshot.output as f64 / 1_000_000.0,
                price_cache_read: price_snapshot.cache_read as f64 / 1_000_000.0,
                price_cache_write: price_snapshot.cache_write as f64 / 1_000_000.0,
                price_in_micros: price_snapshot.input,
                price_out_micros: price_snapshot.output,
                price_cache_read_micros: price_snapshot.cache_read,
                price_cache_write_micros: price_snapshot.cache_write,
                cost_usd,
                cost_micros: micros,
                estimated: if *estimated { 1 } else { 0 },
            }]
        }
        Event::Compacted {
            seq,
            replaced,
            summary,
        } => {
            let content_json = serde_json::to_string(&summary.content).unwrap_or_default();
            let est = est_tokens(&summary.content);
            vec![Row::Compacted {
                session_id: session_id.to_owned(),
                seq: *seq,
                replaced_start: replaced.start,
                replaced_end: replaced.end,
                role: role_str(summary.role).to_owned(),
                content_json,
                est_tokens: est,
            }]
        }
        // SessionForked, ModelChanged → no projection row
        _ => vec![],
    }
}

/// Write projected rows (inside an existing transaction).
pub fn write_rows(conn: &Connection, rows: Vec<Row>) -> Result<(), StoreErr> {
    for row in rows {
        match row {
            Row::Message {
                session_id,
                seq,
                role,
                content_json,
                est_tokens,
                silent,
            } => {
                conn.execute(
                    "INSERT OR REPLACE INTO messages(session_id,seq,role,content,est_tokens,silent)\
                     VALUES(?1,?2,?3,?4,?5,?6)",
                    params![session_id, seq as i64, role, content_json, est_tokens, silent as i64],
                ).map_err(|e| StoreErr(format!("insert message: {e}")))?;
            }
            Row::Usage {
                session_id,
                seq,
                ts,
                provider,
                model,
                kind,
                tokens_in,
                tokens_out,
                cache_read,
                cache_write,
                reasoning,
                price_in,
                price_out,
                price_cache_read,
                price_cache_write,
                price_in_micros,
                price_out_micros,
                price_cache_read_micros,
                price_cache_write_micros,
                cost_usd,
                cost_micros,
                estimated,
            } => {
                conn.execute(
                    "INSERT OR REPLACE INTO usage(\
                       session_id,seq,ts,provider,model,kind,\
                       tokens_in,tokens_out,cache_read,cache_write,reasoning,\
                       price_in_snapshot,price_out_snapshot,\
                       price_cache_read_snapshot,price_cache_write_snapshot,\
                       price_in_micros,price_out_micros,price_cache_read_micros,price_cache_write_micros,\
                       cost_usd,cost_micros,estimated)\
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
                    params![session_id, seq as i64, ts, provider, model, kind,
                        tokens_in, tokens_out, cache_read, cache_write, reasoning,
                        price_in, price_out, price_cache_read, price_cache_write,
                        price_in_micros, price_out_micros, price_cache_read_micros, price_cache_write_micros,
                        cost_usd, cost_micros, estimated],
                ).map_err(|e| StoreErr(format!("insert usage: {e}")))?;
            }
            Row::Compacted {
                session_id,
                seq,
                replaced_start,
                replaced_end,
                role,
                content_json,
                est_tokens,
            } => {
                conn.execute(
                    "DELETE FROM messages WHERE session_id=?1 AND seq>=?2 AND seq<=?3",
                    params![session_id, replaced_start as i64, replaced_end as i64],
                )
                .map_err(|e| StoreErr(format!("delete compacted: {e}")))?;
                // Compacted messages are never silent (they're assistant summaries)
                conn.execute(
                    "INSERT OR REPLACE INTO messages(session_id,seq,role,content,est_tokens,silent)\
                     VALUES(?1,?2,?3,?4,?5,0)",
                    params![session_id, seq as i64, role, content_json, est_tokens],
                ).map_err(|e| StoreErr(format!("insert compact summary: {e}")))?;
            }
        }
    }
    Ok(())
}

/// Increment blob refcounts for any `sha256:<hex>` refs in a content JSON string.
pub fn incr_blob_refs(conn: &Connection, content_json: &str) -> Result<(), StoreErr> {
    for hash in extract_sha256_refs(content_json) {
        conn.execute(
            "UPDATE blobs SET refcount = refcount + 1 WHERE hash = ?1",
            params![hash],
        )
        .map_err(|e| StoreErr(format!("incr blob ref: {e}")))?;
    }
    Ok(())
}

/// Decrement blob refcounts; delete rows that reach zero.
pub fn decr_blob_refs(conn: &Connection, content_json: &str) -> Result<(), StoreErr> {
    for hash in extract_sha256_refs(content_json) {
        conn.execute(
            "UPDATE blobs SET refcount = refcount - 1 WHERE hash = ?1",
            params![hash],
        )
        .map_err(|e| StoreErr(format!("decr blob ref: {e}")))?;
        conn.execute(
            "DELETE FROM blobs WHERE hash = ?1 AND refcount <= 0",
            params![hash],
        )
        .map_err(|e| StoreErr(format!("delete blob: {e}")))?;
    }
    Ok(())
}

/// Returns just the hex digest (no `sha256:` prefix) for each ref found.
fn extract_sha256_refs(json: &str) -> Vec<String> {
    let mut out = Vec::new();
    let prefix = "sha256:";
    let mut s = json;
    while let Some(idx) = s.find(prefix) {
        let rest = &s[idx + prefix.len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(rest.len());
        if end > 0 {
            out.push(rest[..end].to_owned()); // hex only, no "sha256:" prefix
        }
        s = &s[idx + prefix.len() + end..];
    }
    out
}
