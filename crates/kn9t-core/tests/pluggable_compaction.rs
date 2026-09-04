//! 96E-16 regression: compaction must be pluggable (PluginCompactor) and Handoff must exist.

#[test]
fn p1_96e16_handoff_event_is_durable() {
    use kn9t_core::{CallId, Event};
    // Event::Handoff must exist and be durable (carries seq)
    let ev = Event::Handoff {
        seq: 1,
        keep: vec![CallId("c1".into())],
        summarize: vec![],
        drop_ids: vec![CallId("c2".into())],
        resume_actions: vec!["resume".into()],
    };
    assert!(ev.is_durable(), "Handoff must be durable (seq Some)");
    assert_eq!(ev.seq(), Some(1));
    // with_seq must stamp
    let ev2 = ev.with_seq(42);
    assert_eq!(ev2.seq(), Some(42));
    // serde roundtrip with snake_case kind
    let json = serde_json::to_string(&ev2).unwrap();
    assert!(
        json.contains("\"kind\":\"handoff\"") || json.contains("\"kind\": \"handoff\""),
        "wire must be snake_case handoff, got {json}"
    );
    let back: Event = serde_json::from_str(&json).unwrap();
    assert!(back.is_durable());
}

#[test]
fn p1_96e17_compactor_trait_exists_model_passed() {
    use kn9t_core::{
        CompactSpan, CompactionPlan, Compactor, Content, Message, ModelRef, MsgId, Role, SeqRange,
    };
    // Compactor trait must exist and receive the session model (96E-17 wire needs it).
    struct NoopCompactor;
    impl Compactor for NoopCompactor {
        fn compact(&self, span: CompactSpan, model: &ModelRef) -> Result<CompactionPlan, String> {
            // echo back a summary derived from span + model
            let summary = Message {
                id: MsgId::new(),
                role: Role::Assistant,
                content: vec![Content::Text {
                    text: format!("summary of {} msgs via {}", span.messages.len(), model.id),
                }],
                silent: false,
            };
            Ok(CompactionPlan {
                summary,
                handoff: None,
            })
        }
    }
    let c: Box<dyn Compactor> = Box::new(NoopCompactor);
    let span = CompactSpan {
        replaced: SeqRange { start: 1, end: 2 },
        messages: vec![Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![Content::Text { text: "hi".into() }],
            silent: false,
        }],
    };
    let model = ModelRef {
        provider: "test".into(),
        id: "m-1".into(),
    };
    let plan = c.compact(span, &model).unwrap();
    assert!(plan.summary.content.iter().any(
        |c| matches!(c, Content::Text { text } if text.contains("summary of 1 msgs via m-1"))
    ));
    assert!(plan.handoff.is_none());
}

#[test]
fn p1_96e16_handoff_callid_validation() {
    use kn9t_core::{CallId, Event};
    // Host must be able to validate CallIds in a Handoff against known session CallIds.
    // We test the helper that will be added to core.
    let known = vec![CallId("keep-1".into()), CallId("sum-1".into())];
    let ev = Event::Handoff {
        seq: 1,
        keep: vec![CallId("keep-1".into())],
        summarize: vec![kn9t_core::HandoffSummary {
            id: CallId("sum-1".into()),
            summary: "did X".into(),
        }],
        drop_ids: vec![],
        resume_actions: vec![],
    };
    // This should pass validation
    assert!(
        kn9t_core::validate_handoff(&ev, &known).is_ok(),
        "valid handoff should pass"
    );
    // Unknown ID should fail
    let bad = Event::Handoff {
        seq: 1,
        keep: vec![CallId("hallucinated".into())],
        summarize: vec![],
        drop_ids: vec![],
        resume_actions: vec![],
    };
    assert!(
        kn9t_core::validate_handoff(&bad, &known).is_err(),
        "hallucinated CallId must be rejected"
    );
}
