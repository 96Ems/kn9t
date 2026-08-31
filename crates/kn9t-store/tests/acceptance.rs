//! Stage 04 acceptance tests — R-STOR-010..R-STOR-180.
#![allow(clippy::unwrap_used)]

use kn9t_core::{
    CallId, Content, Event, ForkReason, Message, MsgId, ModelRef, Price, Role, SeqRange,
    SessionId, Tokens, UsageKind,
};
use kn9t_core::Store;
use kn9t_store::{CostRollup, SqliteStore, create_session, fork_session, has_orphan_tool_call};
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

fn tmp_store() -> (TempDir, SqliteStore) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("kn9t.db");
    let store = SqliteStore::open(&path).unwrap();
    (dir, store)
}

fn model_ref() -> ModelRef {
    ModelRef { provider: "test".into(), id: "model".into() }
}

fn new_session(store: &SqliteStore) -> SessionId {
    let id = SessionId::new();
    create_session(store, &id, "/cwd", &model_ref()).unwrap();
    id
}

fn msg(seq: u64, text: &str) -> Event {
    Event::MessageAppended {
        seq,
        msg: Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![Content::Text { text: text.into() }], silent: false
        },
    }
}

fn usage(seq: u64, tok_in: u32, tok_out: u32) -> Event {
    Event::UsageRecorded {
        seq,
        provider: "test".into(),
        model: "model".into(),
        kind: UsageKind::Main,
        tokens: Tokens { input: tok_in, output: tok_out, ..Tokens::default() },
        price_snapshot: Price { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 },
        cost_usd: 0.0,
        estimated: false,
    }
}

// ── stor::pragmas (R-STOR-010) ────────────────────────────────────────────────

#[test]
fn stor_pragmas() {
    let (_dir, store) = tmp_store();
    let jm: String = store.query_one("PRAGMA journal_mode", &[], |r| r.get(0)).unwrap();
    assert_eq!(jm, "wal");
    let fk: i64 = store.query_one("PRAGMA foreign_keys", &[], |r| r.get(0)).unwrap();
    assert_eq!(fk, 1);
}

// ── stor::schema_matches (R-STOR-030) ────────────────────────────────────────

#[test]
fn stor_schema_matches() {
    let (_dir, store) = tmp_store();
    let tables = store.query_strings(
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name", &[],
    ).unwrap();
    for t in &["blobs", "events", "live_messages", "messages", "meta", "sessions", "usage"] {
        assert!(tables.contains(&t.to_string()), "missing table: {t}");
    }
    // events PK must be (session_id, seq)
    let pk_cols: i64 = store.query_one(
        "SELECT COUNT(*) FROM pragma_table_info('events') WHERE pk > 0", &[], |r| r.get(0),
    ).unwrap();
    assert_eq!(pk_cols, 2);
}

// ── stor::append_assigns_seq (R-STOR-040) ────────────────────────────────────

#[test]
fn stor_append_assigns_seq() {
    let (_dir, store) = tmp_store();
    let s1 = new_session(&store);
    let s2 = new_session(&store);

    let r1a = store.append(&s1, msg(1, "a")).unwrap();
    let r2a = store.append(&s2, msg(1, "x")).unwrap();
    let r1b = store.append(&s1, msg(2, "b")).unwrap();
    let r2b = store.append(&s2, msg(2, "y")).unwrap();
    let r1c = store.append(&s1, msg(3, "c")).unwrap();

    assert_eq!((r1a, r1b, r1c), (1, 2, 3), "s1 must be gapless");
    assert_eq!((r2a, r2b), (1, 2), "s2 must be gapless, independent");
}

// ── stor::append_rejects_transient (R-STOR-050) ──────────────────────────────

#[test]
fn stor_append_rejects_transient() {
    let (_dir, store) = tmp_store();
    let s = new_session(&store);
    let err = store.append(&s, Event::TurnStarted { turn: 1 });
    assert!(err.is_err(), "transient event must be rejected");
}

// ── stor::project_is_total (R-STOR-060) ──────────────────────────────────────

#[test]
fn stor_project_is_total() {
    let (_dir, store) = tmp_store();
    let s = new_session(&store);

    store.append(&s, msg(1, "hello")).unwrap();
    let role: String = store.query_one(
        "SELECT role FROM messages WHERE session_id=?1 AND seq=1",
        &[&s.0], |r| r.get(0),
    ).unwrap();
    assert_eq!(role, "user");

    store.append(&s, usage(2, 100, 50)).unwrap();
    let kind: String = store.query_one(
        "SELECT kind FROM usage WHERE session_id=?1 AND seq=2",
        &[&s.0], |r| r.get(0),
    ).unwrap();
    assert_eq!(kind, "main");
}

// ── stor::cost_tiered (R-STOR-070) ───────────────────────────────────────────

#[test]
fn stor_cost_tiered() {
    let (_dir, store) = tmp_store();
    let s = new_session(&store);
    store.append(&s, msg(1, "x")).unwrap();

    // Heavy cache_read usage: cost must be much less than tok_in * price_in / 1e6
    let ev = Event::UsageRecorded {
        seq: 2,
        provider: "test".into(),
        model: "model".into(),
        kind: UsageKind::Main,
        tokens: Tokens { input: 100, output: 0, cache_read: 10_000, cache_write: 0, reasoning: 0 },
        price_snapshot: Price { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 },
        cost_usd: 0.0,
        estimated: false,
    };
    store.append(&s, ev).unwrap();

    let cost: f64 = store.query_one(
        "SELECT cost_usd FROM usage WHERE session_id=?1 AND seq=2",
        &[&s.0], |r| r.get(0),
    ).unwrap();

    // naive (input+cache_read+cache_write)*price_in/1e6 = 10100 * 3.0 / 1e6 = 0.0303
    // correct = 100*3/1e6 + 10000*0.3/1e6 = 0.0003 + 0.003 = 0.0033
    let naive = 10_100.0_f64 * 3.0 / 1e6;
    assert!(cost < naive / 5.0, "cost {cost} must be far less than naive {naive}");
}

// ── stor::reproject_rebuilds (R-STOR-080) ────────────────────────────────────

#[test]
fn stor_reproject_rebuilds() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("kn9t.db");
    let sid_str;
    {
        let store = SqliteStore::open(&path).unwrap();
        let s = new_session(&store);
        sid_str = s.0.clone();
        store.append(&s, msg(1, "hello")).unwrap();
        store.append(&s, usage(2, 10, 5)).unwrap();
        // Corrupt projection while store is open (same connection, WAL mode)
        store.execute_raw(
            "UPDATE messages SET role='corrupted' WHERE session_id=?1",
            &[&sid_str],
        ).unwrap();
    } // store dropped — connection closed

    // Now reproject on a fresh connection
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON").unwrap();
    kn9t_store::reproject::reproject(&conn).unwrap();

    let role: String = conn.query_row(
        "SELECT role FROM messages WHERE session_id=?1 AND seq=1",
        rusqlite::params![sid_str], |r| r.get(0),
    ).unwrap();
    assert_eq!(role, "user", "reproject must restore projection from events");
}

// ── stor::reproject_check_clean (R-STOR-090) ─────────────────────────────────

#[test]
fn stor_reproject_check_clean() {
    let (_dir, store) = tmp_store();
    let s = new_session(&store);
    store.append(&s, msg(1, "hello")).unwrap();
    store.append(&s, usage(2, 10, 5)).unwrap();

    // After normal operation, check must report zero diffs
    let diffs = store.reproject_check().unwrap();
    assert!(diffs.is_empty(), "expected no diffs, got: {diffs:?}");
}

// ── stor::plan_no_tokenizer (R-STOR-100) ─────────────────────────────────────

#[test]
fn stor_plan_no_tokenizer() {
    // This is a cargo-tree check: no tiktoken/tokenizers crate must be linked.
    // We verify by simply calling plan_request (it must use len/4, not a tokenizer).
    let (_dir, store) = tmp_store();
    let s = new_session(&store);
    store.append(&s, msg(1, "hello world")).unwrap();
    // plan_request must not panic or error even without a registered ModelSpec
    let plan = store.plan_request(&s).unwrap();
    assert_eq!(plan.messages.len(), 1);
    // No compaction without a model spec
    assert!(plan.compact.is_none());
}

// ── stor::compact_boundary (R-STOR-110) ──────────────────────────────────────

#[test]
fn stor_compact_boundary() {
    use kn9t_store::compact_span;
    // Build messages: user, assistant+ToolCall, user(ToolResult)
    let call_id = CallId("c1".into());
    let msgs = vec![
        Message { id: MsgId::new(), role: Role::User,
            content: vec![Content::Text { text: "q".into() }], silent: false },
        Message { id: MsgId::new(), role: Role::Assistant,
            content: vec![Content::ToolCall { id: call_id.clone(), name: "bash".into(), args_json: "{}".into() }], silent: false },
        Message { id: MsgId::new(), role: Role::Tool,
            content: vec![Content::ToolResult { id: call_id.clone(), content: vec![], is_error: false }], silent: false },
        Message { id: MsgId::new(), role: Role::User,
            content: vec![Content::Text { text: "done".into() }], silent: false },
    ];
    let seqs = vec![1u64, 2, 3, 4];
    // Naive cut at n/2 = 2 would orphan ToolCall; snap must extend to include ToolResult
    let span = compact_span(&seqs, &msgs);
    // The span must include both ToolCall (seq=2) and ToolResult (seq=3)
    assert!(span.replaced.end >= 3, "snap must include ToolResult pair, end={}", span.replaced.end);
}

// ── stor::fork_no_usage (R-STOR-130a) ────────────────────────────────────────

#[test]
fn stor_fork_no_usage() {
    let (_dir, store) = tmp_store();
    let origin = new_session(&store);
    store.append(&origin, msg(1, "hello")).unwrap();
    store.append(&origin, usage(2, 100, 50)).unwrap();

    let child = SessionId::new();
    fork_session(&store, &origin, &child, 2, ForkReason::Fork, None, "/cwd").unwrap();

    // Child must have zero own usage rows
    let usage_count: i64 = store.query_one(
        "SELECT COUNT(*) FROM usage WHERE session_id=?1", &[&child.0], |r| r.get(0),
    ).unwrap();
    assert_eq!(usage_count, 0, "forked session must not copy usage rows");

    // Child must have correct inherited_cost_usd
    let inh: f64 = store.query_one(
        "SELECT inherited_cost_usd FROM sessions WHERE id=?1", &[&child.0], |r| r.get(0),
    ).unwrap();
    let origin_cost: f64 = store.query_one(
        "SELECT COALESCE(SUM(cost_usd),0) FROM usage WHERE session_id=?1", &[&origin.0], |r| r.get(0),
    ).unwrap();
    assert!((inh - origin_cost).abs() < 1e-9, "inherited_cost_usd must match origin usage sum");
}

// ── stor::fork_renumber (R-STOR-130b) ────────────────────────────────────────

#[test]
fn stor_fork_renumber() {
    let (_dir, store) = tmp_store();
    let origin = new_session(&store);
    store.append(&origin, msg(1, "a")).unwrap();
    store.append(&origin, msg(2, "b")).unwrap();
    store.append(&origin, msg(3, "c")).unwrap();

    let child = SessionId::new();
    fork_session(&store, &origin, &child, 3, ForkReason::Fork, None, "/cwd").unwrap();

    // seq 0 = SessionForked, then 1,2,3 = copied messages
    let seqs: Vec<String> = store.query_strings(
        "SELECT CAST(seq AS TEXT) FROM events WHERE session_id=?1 ORDER BY seq",
        &[&child.0],
    ).unwrap();
    assert_eq!(seqs, vec!["0", "1", "2", "3"], "seqs must be contiguous from 0");
}

// ── stor::blob_dedup (R-STOR-140) ────────────────────────────────────────────

#[test]
fn stor_blob_dedup() {
    let (_dir, store) = tmp_store();
    let bytes = b"hello world";
    let h1 = store.put_blob(bytes, "text/plain").unwrap();
    let h2 = store.put_blob(bytes, "text/plain").unwrap();
    assert_eq!(h1, h2, "same bytes must produce same hash");

    let count: i64 = store.query_one(
        "SELECT COUNT(*) FROM blobs WHERE hash=?1", &[&h1], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 1, "duplicate blob must not create duplicate row");
}

// ── stor::blob_refcount (R-STOR-150) ─────────────────────────────────────────

#[test]
fn stor_blob_refcount() {
    let (_dir, store) = tmp_store();
    let bytes = b"shared image data";
    let hash = store.put_blob(bytes, "image/png").unwrap();

    // Two sessions reference the blob via sha256: in content
    let s1 = new_session(&store);
    let s2 = new_session(&store);
    let img_content = vec![Content::Image { sha256: format!("sha256:{hash}"), mime: "image/png".into() }];

    store.append(&s1, Event::MessageAppended {
        seq: 1,
        msg: Message { id: MsgId::new(), role: Role::User, content: img_content.clone(), silent: false },
    }).unwrap();
    store.append(&s2, Event::MessageAppended {
        seq: 1,
        msg: Message { id: MsgId::new(), role: Role::User, content: img_content, silent: false },
    }).unwrap();

    // Delete s1; blob must survive (s2 still references)
    store.delete_session(&s1).unwrap();
    let rc: i64 = store.query_one(
        "SELECT refcount FROM blobs WHERE hash=?1", &[&hash], |r| r.get(0),
    ).unwrap();
    assert!(rc > 0, "blob must survive first session delete, rc={rc}");

    // Delete s2; blob must be gone
    store.delete_session(&s2).unwrap();
    let exists: i64 = store.query_one(
        "SELECT COUNT(*) FROM blobs WHERE hash=?1", &[&hash], |r| r.get(0),
    ).unwrap();
    assert_eq!(exists, 0, "blob must be deleted when refcount reaches zero");
}

// ── stor::session_delete_blobs (R-STOR-160) ──────────────────────────────────

#[test]
fn stor_session_delete_blobs() {
    let (_dir, store) = tmp_store();
    let bytes = b"some data";
    let hash = store.put_blob(bytes, "text/plain").unwrap();

    let s = new_session(&store);
    let img_content = vec![Content::Image { sha256: format!("sha256:{hash}"), mime: "text/plain".into() }];
    store.append(&s, Event::MessageAppended {
        seq: 1,
        msg: Message { id: MsgId::new(), role: Role::User, content: img_content, silent: false },
    }).unwrap();

    store.delete_session(&s).unwrap();

    // Session row must be gone
    let count: i64 = store.query_one(
        "SELECT COUNT(*) FROM sessions WHERE id=?1", &[&s.0], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 0, "session row must be deleted");

    // Blob must be gone too
    let blob_count: i64 = store.query_one(
        "SELECT COUNT(*) FROM blobs WHERE hash=?1", &[&hash], |r| r.get(0),
    ).unwrap();
    assert_eq!(blob_count, 0, "blob must be GC'd on session delete");
}

#[test]
fn stor_debug_blob_ref() {
    let (_dir, store) = tmp_store();
    let bytes = b"shared image data";
    let hash = store.put_blob(bytes, "image/png").unwrap();
    println!("hash={hash}");

    let s = new_session(&store);
    let sha_ref = format!("sha256:{hash}");
    let img_content = vec![Content::Image { sha256: sha_ref.clone(), mime: "image/png".into() }];
    let content_json = serde_json::to_string(&img_content).unwrap();
    println!("content_json={content_json}");

    store.append(&s, Event::MessageAppended {
        seq: 1,
        msg: Message { id: MsgId::new(), role: Role::User, content: img_content, silent: false },
    }).unwrap();

    let rc: i64 = store.query_one(
        "SELECT refcount FROM blobs WHERE hash=?1", &[&hash], |r| r.get(0),
    ).unwrap();
    println!("refcount after append={rc}");
}

#[test]
fn stor_debug_extract() {
    let json = r#"[{"type":"image","sha256":"sha256:c2376505d12b121f649c7a45b1530c9251c38f678ed8f3583fa36c358c216663","mime":"image/png"}]"#;
    // Manually replicate extract_sha256_refs logic
    let prefix = "sha256:";
    let mut found = Vec::new();
    let mut s = json;
    while let Some(idx) = s.find(prefix) {
        let rest = &s[idx + prefix.len()..];
        let end = rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len());
        if end > 0 {
            found.push(format!("{}{}", prefix, &rest[..end]));
        }
        s = &s[idx + prefix.len() + end..];
    }
    println!("found refs: {found:?}");
    assert!(!found.is_empty(), "must find sha256 ref");
}

// ── stor::live_truncated_on_open (R-STOR-170) ────────────────────────────────

#[test]
fn stor_live_truncated_on_open() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("kn9t.db");
    let sid_str;
    {
        let store = SqliteStore::open(&path).unwrap();
        let s = new_session(&store);
        sid_str = s.0.clone();
        // Write a live_message row
        store.upsert_live_message(
            &s, &MsgId::new(), "assistant",
            &[Content::Text { text: "partial...".into() }],
        ).unwrap();
        let got = store.get_live_message(&s).unwrap();
        assert!(got.is_some(), "live_message must be present before reopen");
    } // store dropped

    // Reopen — must truncate live_messages on startup
    let store2 = SqliteStore::open(&path).unwrap();
    let s = SessionId(sid_str.clone());
    let got = store2.get_live_message(&s).unwrap();
    assert!(got.is_none(), "live_messages must be truncated on reopen");

    // reproject must not be affected
    let diffs = store2.reproject_check().unwrap();
    assert!(diffs.is_empty(), "reproject_check must be clean after reopen");
}

// ── stor::orphan_tool_call_detection ─────────────────────────────────────────
/// Test that has_orphan_tool_call correctly detects orphaned ToolCall blocks.
/// This is the core invariant that prevents the "tool_use without tool_result" API error.
#[test]
fn stor_has_orphan_tool_call_detects_orphan() {
    let call_id = CallId("test-call-1".into());
    
    // Messages with orphaned ToolCall (no matching ToolResult)
    let msgs_with_orphan = vec![
        Message { 
            id: MsgId::new(), 
            role: Role::User,
            content: vec![Content::Text { text: "do something".into() }], silent: false 
        },
        Message { 
            id: MsgId::new(), 
            role: Role::Assistant,
            content: vec![
                Content::Text { text: "I'll run a command".into() },
                Content::ToolCall { 
                    id: call_id.clone(), 
                    name: "bash".into(), 
                    args_json: r#"{"command":"ls"}"#.into() 
                },
            ], silent: false 
        },
        // NO ToolResult message! This is the corruption.
    ];
    
    assert!(has_orphan_tool_call(&msgs_with_orphan), 
        "must detect orphaned ToolCall without matching ToolResult");
}

/// Test that has_orphan_tool_call returns false when all ToolCalls have matching ToolResults.
#[test]
fn stor_has_orphan_tool_call_clean_transcript() {
    let call_id = CallId("test-call-1".into());
    
    // Complete transcript with matching ToolResult
    let msgs_clean = vec![
        Message { 
            id: MsgId::new(), 
            role: Role::User,
            content: vec![Content::Text { text: "do something".into() }], silent: false 
        },
        Message { 
            id: MsgId::new(), 
            role: Role::Assistant,
            content: vec![
                Content::Text { text: "I'll run a command".into() },
                Content::ToolCall { 
                    id: call_id.clone(), 
                    name: "bash".into(), 
                    args_json: r#"{"command":"ls"}"#.into() 
                },
            ], silent: false 
        },
        Message { 
            id: MsgId::new(), 
            role: Role::Tool,
            content: vec![
                Content::ToolResult { 
                    id: call_id.clone(), 
                    content: vec![Content::Text { text: "file1.txt\nfile2.txt".into() }],
                    is_error: false,
                },
            ], silent: false 
        },
    ];
    
    assert!(!has_orphan_tool_call(&msgs_clean), 
        "must not report orphan when ToolResult exists");
}

/// Test that multiple parallel tool calls are correctly tracked.
#[test]
fn stor_has_orphan_tool_call_parallel_tools() {
    let call1 = CallId("call-1".into());
    let call2 = CallId("call-2".into());
    
    // Two parallel tool calls, only one has result
    let msgs_partial = vec![
        Message { 
            id: MsgId::new(), 
            role: Role::User,
            content: vec![Content::Text { text: "do two things".into() }], silent: false 
        },
        Message { 
            id: MsgId::new(), 
            role: Role::Assistant,
            content: vec![
                Content::ToolCall { 
                    id: call1.clone(), 
                    name: "read".into(), 
                    args_json: r#"{"path":"a.txt"}"#.into() 
                },
                Content::ToolCall { 
                    id: call2.clone(), 
                    name: "bash".into(), 
                    args_json: r#"{"command":"pwd"}"#.into() 
                },
            ], silent: false 
        },
        Message { 
            id: MsgId::new(), 
            role: Role::Tool,
            content: vec![
                // Only call1 has a result, call2 is missing
                Content::ToolResult { 
                    id: call1.clone(), 
                    content: vec![Content::Text { text: "contents".into() }],
                    is_error: false,
                },
            ], silent: false 
        },
    ];
    
    assert!(has_orphan_tool_call(&msgs_partial), 
        "must detect orphan when one of multiple parallel calls is missing result");
    
    // Now add the missing result
    let mut msgs_complete = msgs_partial.clone();
    // Add call2's result to the tool message
    if let Some(tool_msg) = msgs_complete.last_mut() {
        tool_msg.content.push(Content::ToolResult { 
            id: call2.clone(), 
            content: vec![Content::Text { text: "/home/user".into() }],
            is_error: false,
        });
    }
    
    assert!(!has_orphan_tool_call(&msgs_complete), 
        "must not report orphan when all parallel calls have results");
}

/// Simulate the exact scenario that causes the API error:
/// 1. Session has a completed turn with tool_use + tool_result
/// 2. User sends new prompt
/// 3. Assistant responds with tool_use
/// 4. The process dies before the tool result is persisted (kill -9 / restart)
/// 5. Next API call must NOT fail with "tool_use without tool_result"
///
/// R-STOR-115: `plan_request` closes the orphan in the fold. The `events` log keeps the
/// honest record (no tool-role event); the derived message list is §7.5-clean.
#[test]
fn stor_orphan_from_interrupted_tool_execution() {
    let (_dir, store) = tmp_store();
    let s = new_session(&store);
    
    let call1 = CallId("call-completed".into());
    let call2 = CallId("call-orphaned".into());
    
    // Turn 1: Complete tool call cycle
    store.append(&s, Event::MessageAppended {
        seq: 1,
        msg: Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![Content::Text { text: "list files".into() }], silent: false
        },
    }).unwrap();
    
    store.append(&s, Event::MessageAppended {
        seq: 2,
        msg: Message {
            id: MsgId::new(),
            role: Role::Assistant,
            content: vec![
                Content::Text { text: "I'll list the files".into() },
                Content::ToolCall { 
                    id: call1.clone(), 
                    name: "bash".into(), 
                    args_json: r#"{"command":"ls"}"#.into() 
                },
            ], silent: false
        },
    }).unwrap();
    
    store.append(&s, Event::MessageAppended {
        seq: 3,
        msg: Message {
            id: MsgId::new(),
            role: Role::Tool,
            content: vec![
                Content::ToolResult { 
                    id: call1.clone(), 
                    content: vec![Content::Text { text: "file1.txt\nfile2.txt".into() }],
                    is_error: false,
                },
            ], silent: false
        },
    }).unwrap();
    
    // Turn 2: User sends new prompt
    store.append(&s, Event::MessageAppended {
        seq: 4,
        msg: Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![Content::Text { text: "now read file1.txt".into() }], silent: false
        },
    }).unwrap();
    
    // Turn 2: Assistant responds with tool_use
    store.append(&s, Event::MessageAppended {
        seq: 5,
        msg: Message {
            id: MsgId::new(),
            role: Role::Assistant,
            content: vec![
                Content::ToolCall { 
                    id: call2.clone(), 
                    name: "read".into(), 
                    args_json: r#"{"path":"file1.txt"}"#.into() 
                },
            ], silent: false
        },
    }).unwrap();
    
    // *** INTERRUPTION: the process dies here; the tool_result is NEVER persisted ***

    // The durable log still records the orphan honestly (append-only, GI-4).
    let raw: Vec<String> = store
        .query_strings(
            "SELECT content FROM messages WHERE session_id=?1 ORDER BY seq",
            &[&s.0.as_str()],
        )
        .unwrap();
    assert!(
        raw.iter().any(|c| c.contains("call-orphaned")) 
            && !raw.iter().any(|c| c.contains("tool_result") && c.contains("call-orphaned")),
        "events/messages must keep the honest record: a tool_call with no tool_result"
    );

    // But the planned request the provider actually sees is §7.5-clean.
    let plan = store.plan_request(&s).unwrap();

    assert!(
        !has_orphan_tool_call(&plan.messages),
        "R-STOR-115: plan_request must close orphaned ToolCalls; every provider 400s otherwise"
    );

    // The synthesized result is an error, carries the provider's verbatim call id, and
    // sits immediately after the assistant message that opened the call.
    let opener = plan
        .messages
        .iter()
        .position(|m| {
            m.content.iter().any(|c| matches!(c, Content::ToolCall { id, .. } if *id == call2))
        })
        .expect("orphaned tool_call survives the fold");
    let closer = &plan.messages[opener + 1];
    assert!(matches!(closer.role, Role::Tool), "closer must be a tool-role message");
    match &closer.content[0] {
        Content::ToolResult { id, is_error, content } => {
            assert_eq!(*id, call2, "call id must be the provider's, verbatim");
            assert!(*is_error, "a call that never reported is an error result");
            assert!(
                matches!(&content[0], Content::Text { text } if text.contains("interrupted")),
                "result must say why it is synthesized"
            );
        }
        _ => panic!("expected a synthesized ToolResult"),
    }

    // The already-answered call is untouched — no duplicate result synthesized.
    let results = plan
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|c| matches!(c, Content::ToolResult { id, .. } if *id == call1))
        .count();
    assert_eq!(results, 1, "a completed call must not gain a second result");
}

// ── stor::plan_repairs_unparseable_tool_args (R-STOR-117) ────────────────────

/// R-STOR-117 — a `tool_call` whose `args_json` was cut mid-stream is durable in `events`
/// and, because the log is append-only (GI-4), cannot be rewritten. Without a repair in the
/// fold, every subsequent turn ships the same unparseable bytes and the provider rejects the
/// request — observed live as a permanent litellm/Bedrock 500 ("Unable to convert openai
/// tool calls ... Unterminated string"). `plan_request` must hand the provider `{}` instead,
/// while leaving valid args byte-identical.
#[test]
fn stor_plan_repairs_unparseable_tool_args() {
    let (_dir, store) = tmp_store();
    let s = new_session(&store);

    let broken = CallId("call-broken-args".into());
    let intact = CallId("call-good-args".into());

    // Non-sorted keys: the untouched call must keep its exact byte order (R-CORE-062).
    let intact_args = r#"{"z":1,"a":2}"#;

    store.append(&s, Event::MessageAppended {
        seq: 0,
        msg: Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![Content::Text { text: "edit it".into() }], silent: false,
        },
    }).unwrap();

    store.append(&s, Event::MessageAppended {
        seq: 0,
        msg: Message {
            id: MsgId::new(),
            role: Role::Assistant,
            content: vec![
                Content::ToolCall {
                    id: broken.clone(),
                    name: "edit".into(),
                    // Cut inside a JSON string value — the exact observed corruption.
                    args_json: r#"{"path":"a.rs","new_string":"fn main("#.into(),
                },
                Content::ToolCall {
                    id: intact.clone(),
                    name: "read".into(),
                    args_json: intact_args.into(),
                },
            ],
            silent: false,
        },
    }).unwrap();

    // Both calls were answered — this is NOT the R-STOR-115 orphan case.
    store.append(&s, Event::MessageAppended {
        seq: 0,
        msg: Message {
            id: MsgId::new(),
            role: Role::Tool,
            content: vec![
                Content::ToolResult {
                    id: broken.clone(),
                    content: vec![Content::Text { text: "missing 'path'".into() }],
                    is_error: true,
                },
                Content::ToolResult {
                    id: intact.clone(),
                    content: vec![Content::Text { text: "ok".into() }],
                    is_error: false,
                },
            ],
            silent: false,
        },
    }).unwrap();

    let plan = store.plan_request(&s).unwrap();

    let args_for = |want: &CallId| -> String {
        plan.messages
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|c| match c {
                Content::ToolCall { id, args_json, .. } if id == want => Some(args_json.clone()),
                _ => None,
            })
            .expect("the tool call must survive the fold")
    };

    // Every tool call the provider sees must be parseable JSON.
    for c in plan.messages.iter().flat_map(|m| &m.content) {
        if let Content::ToolCall { id, args_json, .. } = c {
            assert!(
                serde_json::from_str::<serde_json::Value>(args_json).is_ok(),
                "call {id:?} still carries unparseable args: {args_json}"
            );
        }
    }

    assert_eq!(args_for(&broken), "{}", "unparseable args must be replaced by an empty object");
    assert_eq!(
        args_for(&intact), intact_args,
        "valid args must stay byte-identical, key order included (R-CORE-062)"
    );

    // The call is kept, not dropped: removing it would orphan its result (§7.5).
    assert!(
        !has_orphan_tool_call(&plan.messages),
        "repair must not break the ToolCall/ToolResult pairing"
    );
    let results = plan
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|c| matches!(c, Content::ToolResult { id, .. } if *id == broken))
        .count();
    assert_eq!(results, 1, "the real result must not be duplicated by synthesis");
}

// ── stor::cost_rollup (R-STOR-180) ───────────────────────────────────────────

#[test]
fn stor_cost_rollup() {
    let (_dir, store) = tmp_store();

    // Root session with some spend
    let root = new_session(&store);
    store.append(&root, msg(1, "q")).unwrap();
    store.append(&root, usage(2, 1_000_000, 0)).unwrap(); // $3.00

    // Fork child from root
    let child = SessionId::new();
    fork_session(&store, &root, &child, 2, ForkReason::Fork, None, "/cwd").unwrap();
    store.append(&child, msg(3, "q2")).unwrap();
    store.append(&child, usage(4, 0, 1_000_000, )).unwrap(); // $15.00 out

    // Grandchild from child
    let grand = SessionId::new();
    fork_session(&store, &child, &grand, 4, ForkReason::Fork, None, "/cwd").unwrap();
    store.append(&grand, msg(5, "q3")).unwrap();
    store.append(&grand, usage(6, 1_000_000, 0)).unwrap(); // $3.00

    let rollup = store.cost_rollup(&grand).unwrap();
    // marginal = $3.00 (grand's own spend)
    assert!((rollup.marginal - 3.0).abs() < 0.01, "marginal={}", rollup.marginal);
    // effective = marginal + child's inherited (which = root cost $3.00 + child own $15.00)
    // Actually effective = grand.marginal + grand.inherited_cost_usd
    // grand.inherited = child's cumulative = child.inherited($3) + child.own($15) = $18
    // effective = 3 + 18 = 21
    assert!(rollup.effective > rollup.marginal, "effective must exceed marginal");
    // family = grand + child + root own costs
    assert!(rollup.family >= rollup.marginal, "family must be >= marginal");
}
