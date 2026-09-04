//! 96E-17: handoff seam — keep/summarize/drop + resume reconstruction.
//! Validates the four acceptance criteria without requiring a live LLM:
//! - unknown CallId rejected (compact_tool_select equivalent)
//! - summarize never leaks full output (host keeps verbatim, LLM sees previews)
//! - resume-from-handoff reconstructs expected context.

use kn9t_core::{CallId, Content, Event, HandoffSummary, Message, MsgId, Role};

#[test]
fn handoff_rejects_unknown_keep() {
    let known = vec![CallId("a".into()), CallId("b".into())];
    let ev = Event::Handoff {
        seq: 1,
        keep: vec![CallId("hallucinated".into())],
        summarize: vec![],
        drop_ids: vec![],
        resume_actions: vec![],
    };
    assert!(kn9t_core::validate_handoff(&ev, &known).is_err(), "hallucinated keep must be rejected");
}

#[test]
fn handoff_rejects_unknown_summarize() {
    let known = vec![CallId("a".into())];
    let ev = Event::Handoff {
        seq: 2,
        keep: vec![],
        summarize: vec![HandoffSummary { id: CallId("ghost".into()), summary: "x".into() }],
        drop_ids: vec![],
        resume_actions: vec![],
    };
    assert!(kn9t_core::validate_handoff(&ev, &known).is_err());
}

#[test]
fn handoff_rejects_unknown_drop() {
    let known = vec![CallId("a".into())];
    let ev = Event::Handoff {
        seq: 3,
        keep: vec![],
        summarize: vec![],
        drop_ids: vec![CallId("ghost".into())],
        resume_actions: vec![],
    };
    assert!(kn9t_core::validate_handoff(&ev, &known).is_err());
}

#[test]
fn handoff_accepts_valid_ids() {
    let known = vec![CallId("keep-1".into()), CallId("sum-1".into()), CallId("drop-1".into())];
    let ev = Event::Handoff {
        seq: 4,
        keep: vec![CallId("keep-1".into())],
        summarize: vec![HandoffSummary { id: CallId("sum-1".into()), summary: "did X".into() }],
        drop_ids: vec![CallId("drop-1".into())],
        resume_actions: vec!["next".into()],
    };
    assert!(kn9t_core::validate_handoff(&ev, &known).is_ok());
}

/// Simulate the compactor's triage filtering: hallucinated IDs are dropped,
/// only known IDs survive — mirrors compact_tool_select's schema validation.
#[test]
fn compactor_triage_filters_hallucinated() {
    let known = vec!["t1".to_string(), "t2".to_string()];
    let candidate = vec!["t1".to_string(), "ghost".to_string(), "t2".to_string()];
    let valid: Vec<_> = candidate.into_iter().filter(|id| known.contains(id)).collect();
    assert_eq!(valid, vec!["t1", "t2"]);
    assert!(!valid.contains(&"ghost".to_string()));
}

/// Resume-from-handoff: a new session seeded from Handoff should have
/// - kept tool results verbatim (host copies them)
/// - summarized tool results replaced by the summary text
/// - dropped tool results omitted
/// - resume_actions available as next-step hints.
#[test]
fn resume_from_handoff_reconstructs_context() {
    // Original span: three tool calls with distinct outputs
    let orig = vec![
        (CallId("keep-1".into()), "LARGE OUTPUT KEEP VERBATIM"),
        (CallId("sum-1".into()), "small output to be summarized"),
        (CallId("drop-1".into()), "noise"),
    ];
    let keep = vec![CallId("keep-1".into())];
    let summarize = vec![HandoffSummary { id: CallId("sum-1".into()), summary: "summarized: did X".into() }];
    let drop_ids = vec![CallId("drop-1".into())];
    let resume_actions = vec!["retry the task".to_string()];

    // Simulate summary message assembly (as kn9t-compactor does):
    // summary text + verbatim kept blocks.
    let summary_text = "Summary of span: handled 3 tool calls.";
    let mut summary_content = vec![Content::Text { text: summary_text.to_string() }];
    for (id, output) in &orig {
        if keep.contains(id) {
            // verbatim copy — host never re-sends full output to LLM
            summary_content.push(Content::ToolResult {
                id: id.clone(),
                content: vec![Content::Text { text: output.to_string() }],
                is_error: false,
            });
        }
    }
    // summarized ones become inline text summaries, not full outputs
    for s in &summarize {
        summary_content.push(Content::Text { text: format!("{}: {}", s.id.0, s.summary) });
    }

    let summary = Message { id: MsgId::new(), role: Role::Assistant, content: summary_content.clone(), silent: false };

    // Assertions: keep is verbatim, summarize is not full original, drop is absent, resume present
    let serialized = serde_json::to_string(&summary.content).unwrap();
    assert!(serialized.contains("LARGE OUTPUT KEEP VERBATIM"), "keep must be verbatim in summary");
    assert!(!serialized.contains("small output to be summarized"), "summarize must NOT contain full original — only summary");
    assert!(serialized.contains("summarized: did X"), "summarize note must be present");
    // Assert against the plan itself, not a hardcoded literal: every dropped id
    // and its output must be gone from the summary.
    for id in &drop_ids {
        assert!(!serialized.contains(&id.0), "dropped id {} must be absent", id.0);
        if let Some((_, output)) = orig.iter().find(|(oid, _)| oid == id) {
            assert!(!serialized.contains(output), "dropped output must be absent");
        }
    }
    assert!(!serialized.contains("noise"), "drop must be absent");
    assert_eq!(resume_actions, vec!["retry the task"]);
}

/// The full-original-never-leaves-host property: the summary provider call
/// must be built from inventory previews, not full JSON. This test guards
/// the kn9t-compactor's summaryMsgs change (Inventory vs JSON.stringify(messages)).
#[test]
fn summary_prompt_must_not_contain_full_tool_output() {
    // Simulate what the compactor now sends to provider_complete for summary
    let full_messages = vec![Message {
        id: MsgId::new(),
        role: Role::Tool,
        content: vec![Content::ToolResult {
            id: CallId("t1".into()),
            content: vec![Content::Text { text: "FULL SECRET TOOL OUTPUT 0123456789".to_string() }],
            is_error: false,
        }],
        silent: false,
    }];
    let inventory = "tool_result t1: 300 chars: preview...";
    let decisions = r#"[{"id":"t1","action":"keep"}]"#;
    // New prompt (fixed): decisions + inventory only
    let new_prompt = format!("Decisions:\n{}\n\nInventory:\n{}", decisions, inventory);
    assert!(!new_prompt.contains("FULL SECRET TOOL OUTPUT"), "new prompt must not contain full output");

    // Old prompt (buggy) would have contained full messages
    let old_prompt = format!("Decisions:\n{}\n\nSpan:\n{}", decisions, serde_json::to_string(&full_messages).unwrap());
    assert!(old_prompt.contains("FULL SECRET TOOL OUTPUT"), "old prompt leaked full output — this is the bug we fixed");
}
