//! Stage 01 acceptance tests. Wrapped in `mod core` so each test's path is
//! `core::<name>`, matching the spec's `cargo test core::<name>` accept lines.

mod core {
    use kn9t_core::*;
    use serde::de::DeserializeOwned;
    use serde::Serialize;
    use std::path::PathBuf;
    use std::time::Duration;

    fn assert_serde<T: Serialize + DeserializeOwned>() {}

    // -- helpers ------------------------------------------------------------

    fn model_ref() -> ModelRef {
        ModelRef {
            provider: "p".into(),
            id: "m".into(),
        }
    }

    fn text_msg(t: &str) -> Message {
        Message {
            id: MsgId("m1".into()),
            role: Role::User,
            content: vec![Content::Text { text: t.into() }],
            silent: false,
        }
    }

    fn fork_snapshot() -> ForkSnapshot {
        ForkSnapshot {
            origin_session: SessionId("s0".into()),
            origin_seq: 7,
            reason: ForkReason::Fork,
            inherited_cost_usd: 1.5,
            inherited_cost_micros: 1_500_000,
            inherited_tokens_in: 10,
            inherited_tokens_out: 20,
            inherited_cache_read: 5,
            inherited_messages: 3,
            inherited_ctx_tokens: 100,
            budget_remaining_usd: Some(9.0),
            budget_remaining_micros: Some(9_000_000),
            model_at_fork: model_ref(),
            thinking_at_fork: Thinking::Off,
            cwd_at_fork: PathBuf::from("/tmp"),
        }
    }

    fn price() -> Price {
        Price {
            input: 1_000_000,
            output: 2_000_000,
            cache_read: 100_000,
            cache_write: 200_000,
        }
    }

    // -- R-CORE-030 ---------------------------------------------------------

    #[test]
    fn payload_is_pod() {
        // Compile-time proof each Event-payload type is Serialize + DeserializeOwned.
        assert_serde::<Message>();
        assert_serde::<Content>();
        assert_serde::<Role>();
        assert_serde::<MsgId>();
        assert_serde::<CallId>();
        assert_serde::<SessionId>();
        assert_serde::<ApprovalId>();
        assert_serde::<ModelRef>();
        assert_serde::<Price>();
        assert_serde::<Thinking>();
        assert_serde::<Tokens>();
        assert_serde::<Usage>();
        assert_serde::<StopReason>();
        assert_serde::<UsageKind>();
        assert_serde::<HookName>();
        assert_serde::<ForkReason>();
        assert_serde::<ForkSnapshot>();
        assert_serde::<SeqRange>();
        assert_serde::<Event>();
    }

    // -- R-CORE-040 ---------------------------------------------------------

    #[test]
    fn id_serde() {
        assert_eq!(
            serde_json::to_string(&SessionId("abc".into())).unwrap(),
            "\"abc\""
        );
        assert_eq!(
            serde_json::to_string(&MsgId("m".into())).unwrap(),
            "\"m\""
        );
        assert_eq!(
            serde_json::to_string(&CallId("c".into())).unwrap(),
            "\"c\""
        );
        assert_eq!(serde_json::to_string(&ApprovalId(42)).unwrap(), "42");

        let s: SessionId = serde_json::from_str("\"round\"").unwrap();
        assert_eq!(s.0, "round");
    }

    // -- R-CORE-045 ---------------------------------------------------------

    #[test]
    fn ulid_monotonic() {
        let a = SessionId::new();
        std::thread::sleep(Duration::from_millis(2));
        let b = SessionId::new();
        assert!(a.0 < b.0, "expected {} < {}", a.0, b.0);
        assert_eq!(a.0.len(), 26);
        assert_eq!(b.0.len(), 26);

        let a = MsgId::new();
        std::thread::sleep(Duration::from_millis(2));
        let b = MsgId::new();
        assert!(a.0 < b.0, "expected {} < {}", a.0, b.0);
    }

    // -- R-CORE-060 ---------------------------------------------------------

    #[test]
    fn content_tag() {
        let cases: Vec<(Content, &str)> = vec![
            (Content::Text { text: "t".into() }, "text"),
            (
                Content::Image {
                    sha256: "sha256:ab".into(),
                    mime: "image/png".into(),
                },
                "image",
            ),
            (
                Content::ToolCall {
                    id: CallId("c1".into()),
                    name: "read".into(),
                    args_json: "{}".into(),
                },
                "tool_call",
            ),
            (
                Content::ToolResult {
                    id: CallId("c1".into()),
                    content: vec![Content::Text { text: "ok".into() }],
                    is_error: false,
                },
                "tool_result",
            ),
            (
                Content::Thinking {
                    text: "hmm".into(),
                    signature: None,
                },
                "thinking",
            ),
        ];
        for (c, tag) in cases {
            let v = serde_json::to_value(&c).unwrap();
            assert_eq!(v["type"], tag);
            let back: Content = serde_json::from_value(v).unwrap();
            assert!(back == c);
        }
    }

    // -- R-CORE-062 ---------------------------------------------------------

    #[test]
    fn args_verbatim() {
        // Non-sorted keys must survive serde round-trip byte-identical (the full
        // append->plan->encode pipeline is exercised in later stages; core proves
        // the type never reorders or reparses the bytes).
        let raw = r#"{"b":1,"a":2}"#;
        let msg = Message {
            id: MsgId("m".into()),
            role: Role::Assistant,
            content: vec![Content::ToolCall {
                id: CallId("c".into()),
                name: "x".into(),
                args_json: raw.into(),
            }], silent: false
        };
        let ev = Event::MessageAppended { seq: 1, msg };
        let json = serde_json::to_string(&ev).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        if let Event::MessageAppended { msg, .. } = back {
            match &msg.content[0] {
                Content::ToolCall { args_json, .. } => assert_eq!(args_json, raw),
                _ => panic!("wrong content"),
            }
        } else {
            panic!("wrong event");
        }
    }

    // -- R-CORE-064 ---------------------------------------------------------

    #[test]
    fn thinking_roundtrip() {
        for sig in [None, Some("opaque-sig".to_string())] {
            let c = Content::Thinking {
                text: "reason".into(),
                signature: sig.clone(),
            };
            let back: Content = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
            match back {
                Content::Thinking { signature, .. } => assert_eq!(signature, sig),
                _ => panic!("wrong variant"),
            }
        }
    }

    // -- R-CORE-100 ---------------------------------------------------------

    #[test]
    fn tokens_default_zero() {
        let t = Tokens::default();
        assert_eq!(t.input, 0);
        assert_eq!(t.output, 0);
        assert_eq!(t.cache_read, 0);
        assert_eq!(t.cache_write, 0);
        assert_eq!(t.reasoning, 0);
    }

    // -- R-CORE-140 ---------------------------------------------------------

    #[test]
    fn event_tag() {
        let events: Vec<(Event, &str)> = vec![
            (
                Event::SessionForked {
                    seq: 0,
                    fork: fork_snapshot(),
                },
                "session_forked",
            ),
            (
                Event::MessageAppended {
                    seq: 1,
                    msg: text_msg("hi"),
                },
                "message_appended",
            ),
            (
                Event::ModelChanged {
                    seq: 2,
                    model: model_ref(),
                },
                "model_changed",
            ),
            (
                Event::Compacted {
                    seq: 3,
                    replaced: SeqRange { start: 1, end: 2 },
                    summary: text_msg("sum"),
                },
                "compacted",
            ),
            (
                Event::UsageRecorded {
                    seq: 4,
                    provider: "p".into(),
                    model: "m".into(),
                    kind: UsageKind::Main,
                    tokens: Tokens::default(),
                    price_snapshot: price(),
                    cost_micros: 0,
                    cost_usd: 0.0,
                    estimated: false,
                },
                "usage_recorded",
            ),
            (Event::TurnStarted { turn: 1 }, "turn_started"),
            (
                Event::TextDelta {
                    msg_id: MsgId("m".into()),
                    idx: 0,
                    delta: "d".into(),
                },
                "text_delta",
            ),
            (
                Event::ThinkingDelta {
                    msg_id: MsgId("m".into()),
                    idx: 0,
                    delta: "d".into(),
                },
                "thinking_delta",
            ),
            (
                Event::ToolArgsDelta {
                    msg_id: MsgId("m".into()),
                    idx: 0,
                    delta: "d".into(),
                },
                "tool_args_delta",
            ),
            (
                Event::ToolStarted {
                    call_id: CallId("c".into()),
                    name: "read".into(),
                },
                "tool_started",
            ),
            (
                Event::ToolProgress {
                    call_id: CallId("c".into()),
                    note: "n".into(),
                },
                "tool_progress",
            ),
            (
                Event::ToolFinished {
                    call_id: CallId("c".into()),
                    is_error: false,
                },
                "tool_finished",
            ),
            (
                Event::ApprovalRequest {
                    id: ApprovalId(1),
                    tool: "bash".into(),
                    args: serde_json::json!({"cmd":"ls"}),
                    cwd: PathBuf::from("/tmp"),
                    reason: "policy plugin asked".into(),
                },
                "approval_request",
            ),
            (
                Event::TurnEnded {
                    turn: 1,
                    stop: StopReason::Stop,
                },
                "turn_ended",
            ),
            (
                Event::HookFailed {
                    plugin: "pl".into(),
                    hook: HookName::BeforeToolCall,
                    reason: "boom".into(),
                },
                "hook_failed",
            ),
            (
                Event::Error {
                    message: "err".into(),
                },
                "error",
            ),
        ];
        // AGENTS.md §11: all JSON discriminants are snake_case
        // (`#[serde(tag = "kind", rename_all = "snake_case")]` on Event).
        for (ev, kind) in events {
            let v = serde_json::to_value(&ev).unwrap();
            assert_eq!(v["kind"], kind, "wrong discriminant for {kind}");
            // Round-trips back to the same discriminant.
            let back: Event = serde_json::from_value(v).unwrap();
            assert_eq!(serde_json::to_value(&back).unwrap()["kind"], kind);
        }
    }

    // -- R-CORE-145 ---------------------------------------------------------

    #[test]
    fn seq_partition() {
        let durable = [
            Event::SessionForked {
                seq: 0,
                fork: fork_snapshot(),
            },
            Event::MessageAppended {
                seq: 1,
                msg: text_msg("x"),
            },
            Event::ModelChanged {
                seq: 2,
                model: model_ref(),
            },
            Event::Compacted {
                seq: 3,
                replaced: SeqRange { start: 0, end: 1 },
                summary: text_msg("s"),
            },
            Event::UsageRecorded {
                seq: 4,
                provider: "p".into(),
                model: "m".into(),
                kind: UsageKind::Main,
                tokens: Tokens::default(),
                price_snapshot: price(),
                cost_micros: 0,
                cost_usd: 0.0,
                estimated: false,
            },
        ];
        for (i, e) in durable.iter().enumerate() {
            assert_eq!(e.seq(), Some(i as u64));
            assert!(e.is_durable());
        }

        let transient = [
            Event::TurnStarted { turn: 1 },
            Event::TextDelta {
                msg_id: MsgId("m".into()),
                idx: 0,
                delta: "d".into(),
            },
            Event::ThinkingDelta {
                msg_id: MsgId("m".into()),
                idx: 0,
                delta: "d".into(),
            },
            Event::ToolArgsDelta {
                msg_id: MsgId("m".into()),
                idx: 0,
                delta: "d".into(),
            },
            Event::ToolStarted {
                call_id: CallId("c".into()),
                name: "n".into(),
            },
            Event::ToolProgress {
                call_id: CallId("c".into()),
                note: "n".into(),
            },
            Event::ToolFinished {
                call_id: CallId("c".into()),
                is_error: false,
            },
            Event::ApprovalRequest {
                id: ApprovalId(1),
                tool: "t".into(),
                args: serde_json::Value::Null,
                cwd: PathBuf::from("/"),
                reason: String::new(),
            },
            Event::TurnEnded {
                turn: 1,
                stop: StopReason::Stop,
            },
            Event::HookFailed {
                plugin: "p".into(),
                hook: HookName::AfterToolCall,
                reason: "r".into(),
            },
            Event::Error {
                message: "e".into(),
            },
        ];
        for e in &transient {
            assert_eq!(e.seq(), None);
            assert!(!e.is_durable());
        }
    }

    // -- R-CORE-160 ---------------------------------------------------------

    #[test]
    fn fork_snapshot_serde() {
        let f = fork_snapshot();
        let json = serde_json::to_string(&f).unwrap();
        let back: ForkSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.origin_session.0, "s0");
        assert_eq!(back.origin_seq, 7);
        assert_eq!(back.inherited_messages, 3);
        assert_eq!(back.budget_remaining_usd, Some(9.0));
        assert_eq!(back.cwd_at_fork, PathBuf::from("/tmp"));
        // reason serializes lowercase
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["reason"], "fork");
    }

    // -- R-CORE-210 ---------------------------------------------------------

    fn msg(role: Role) -> Message {
        Message {
            id: MsgId::new(),
            role,
            content: vec![Content::Text { text: "x".into() }],
            silent: false,
        }
    }

    #[test]
    fn breakpoints() {
        let explicit = |max: u8| CacheMode::Explicit {
            max_breakpoints: max,
            min_tokens: 0,
        };

        // `Cache`'s derive list is pinned by R-CORE-200 (no `Debug`), so compare
        // with `==` rather than `assert_eq!`.
        let eq = |a: &[Cache], b: &[Cache]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y);

        // [assistant, user], cap 4 -> [System, AfterMessage{idx:1}, AfterMessage{idx:0}]
        let ms = vec![msg(Role::Assistant), msg(Role::User)];
        assert!(eq(
            &breakpoints_of(&ms, &explicit(4)),
            &[Cache::System, Cache::AfterMessage { idx: 1 }, Cache::AfterMessage { idx: 0 }]
        ));

        // single user message, cap 4 -> [System, AfterMessage{idx:0}]
        let ms = vec![msg(Role::User)];
        assert!(eq(
            &breakpoints_of(&ms, &explicit(4)),
            &[Cache::System, Cache::AfterMessage { idx: 0 }]
        ));

        // Automatic -> same as Explicit(4); None -> []
        assert!(eq(
            &breakpoints_of(&ms, &CacheMode::Automatic),
            &[Cache::System, Cache::AfterMessage { idx: 0 }]
        ));
        assert!(breakpoints_of(&ms, &CacheMode::None).is_empty());

        // cap 2 on a long conversation -> exactly 2, the two stable anchors first.
        let long: Vec<Message> = (0..10)
            .map(|i| msg(if i % 2 == 0 { Role::User } else { Role::Assistant }))
            .collect();
        let bp = breakpoints_of(&long, &explicit(2));
        assert_eq!(bp.len(), 2);
        assert!(bp[0] == Cache::System);
        // last user is index 8; that is the second stable anchor.
        assert!(bp[1] == Cache::AfterMessage { idx: 8 });
    }

    // thin wrapper so the test reads naturally; calls the crate's pure fn.
    fn breakpoints_of(ms: &[Message], mode: &CacheMode) -> Vec<Cache> {
        kn9t_core::breakpoints(ms, mode)
    }

    // -- R-CORE-220 ---------------------------------------------------------

    #[test]
    fn bus_never_blocks() {
        let bus = Bus::new();
        let sub = bus.subscribe(2);

        // Publish more than capacity WITHOUT draining. Must not block.
        for i in 0..4u32 {
            bus.publish(Event::TextDelta {
                msg_id: MsgId("m".into()),
                idx: i,
                delta: i.to_string(),
            });
        }

        // Oldest dropped, newest retained: capacity 2 keeps idx 2 and 3.
        let mut got = Vec::new();
        while let Some(ev) = sub.try_recv() {
            if let Event::TextDelta { idx, .. } = ev {
                got.push(idx);
            }
        }
        assert_eq!(got, vec![2, 3]);
    }

    // -- R-CORE-240 ---------------------------------------------------------

    #[test]
    fn cancel_wakes() {
        let cancel = Cancel::new();
        let c2 = cancel.clone();
        let start = std::time::Instant::now();
        let h = std::thread::spawn(move || c2.wait_timeout(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(50));
        cancel.cancel();
        let cancelled = h.join().unwrap();
        assert!(cancelled);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "wait_timeout did not wake promptly"
        );
        // idempotent
        cancel.cancel();
        assert!(cancel.cancelled());
    }
}
