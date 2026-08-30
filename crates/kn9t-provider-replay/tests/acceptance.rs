//! Stage-02 acceptance tests. Wrapped in `mod rply` so the spec's `cargo test rply::<name>`
//! paths resolve. All tests run offline with no API key present.

mod rply {
    use kn9t_provider_core::{
        Cancel, CacheMode, Chunk, ModelRef, ModelSpec, Price, Provider, Request,
        StopReason, Thinking,
    };
    // kn9t_core::Quirks = model quirks; distinct from kn9t_provider_core::Quirks (HTTP quirks)
    use kn9t_core::Quirks;
    use kn9t_provider_replay::{
        encode_chunks_sse, redact_header_value, serialize_fixture, Fixture, RecordingProvider,
        ReplayProvider,
    };

    // ---- helpers -----------------------------------------------------------------

    /// A minimal `ModelSpec` for building a `Request` (replay ignores request content).
    fn model() -> ModelSpec {
        ModelSpec {
            r#ref: ModelRef {
                provider: "replay".into(),
                id: "test".into(),
            },
            api_id: "test".into(),
            ctx_window: 200_000,
            max_out: 8_000,
            price: Price {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            cache: CacheMode::None,
            streaming: true,
            quirks: Quirks::default(),
        }
    }

    fn empty_request(model: &ModelSpec) -> Request<'_> {
        Request {
            model,
            system: None,
            messages: &[],
            tools: &[],
            thinking: Thinking::Off,
            max_tokens: None,
            cache: &[],
        }
    }

    /// `Chunk` has no `PartialEq`/`Debug` (its derive list is pinned by R-CORE-180), so
    /// compare element-by-element via the stable JSON wire form instead.
    fn as_json(c: &Chunk) -> String {
        serde_json::to_string(c).unwrap()
    }

    fn assert_chunks_eq(got: &[Chunk], want: &[Chunk]) {
        let g: Vec<String> = got.iter().map(as_json).collect();
        let w: Vec<String> = want.iter().map(as_json).collect();
        assert_eq!(g, w);
    }

    /// Drain a provider stream into `(chunks, terminal_error)`.
    fn drain(p: &dyn Provider) -> (Vec<Chunk>, Option<kn9t_provider_core::ProvErr>) {
        let m = model();
        let req = empty_request(&m);
        let cancel = Cancel::new();
        let iter = p.stream(&req, &cancel).expect("pre-stream ok");
        let mut chunks = Vec::new();
        let mut err = None;
        for item in iter {
            match item {
                Ok(c) => chunks.push(c),
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        (chunks, err)
    }

    // ---- R-RPLY-010 --------------------------------------------------------------

    #[test]
    fn header_parse() {
        // A header with the three MUST fields + extras, then a verbatim body containing
        // its own blank lines and CRLFs, must round-trip with the body byte range untouched.
        let body = b"data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"x\"}\r\n\r\ndata: [DONE]\r\n\r\n";
        let mut raw = Vec::new();
        raw.extend_from_slice(b"kind: replay\n");
        raw.extend_from_slice(b"status: 200\n");
        raw.extend_from_slice(b"content-type: text/event-stream\n");
        raw.extend_from_slice(b"note: header parse test\n");
        raw.extend_from_slice(b"\n");
        raw.extend_from_slice(body);

        let f = Fixture::parse(&raw).expect("parse");
        assert_eq!(f.kind, "replay");
        assert_eq!(f.status, 200);
        assert_eq!(f.content_type, "text/event-stream");
        assert_eq!(f.header("note"), Some("header parse test"));
        // Body is byte-identical, including its embedded CRLF and blank lines.
        assert_eq!(f.body, body);
    }

    #[test]
    fn header_parse_crlf_separator() {
        // The blank-line separator may itself be CRLF.
        let raw = b"kind: replay\r\nstatus: 200\r\ncontent-type: text/event-stream\r\n\r\nBODY";
        let f = Fixture::parse(raw).expect("parse");
        assert_eq!(f.kind, "replay");
        assert_eq!(f.body, b"BODY");
    }

    #[test]
    fn header_missing_field_errors() {
        let raw = b"kind: replay\nstatus: 200\n\nBODY"; // no content-type
        assert!(Fixture::parse(raw).is_err());
    }

    // ---- R-RPLY-015 --------------------------------------------------------------

    #[test]
    fn chunk_boundary() {
        // The same fixture must parse identically whether delivered whole or split
        // mid-`data:` line by the `chunks:` header.
        let with_split = ReplayProvider::from_fixture("replay", "chunk_boundary").unwrap();
        let (split_chunks, split_err) = drain(&with_split);
        assert!(split_err.is_none());

        // Build the identical fixture with no `chunks:` split and compare.
        let mut whole = with_split.fixture().clone();
        whole.chunks = Vec::new();
        let whole_provider = ReplayProvider::from_fixture_struct(whole);
        let (whole_chunks, _) = drain(&whole_provider);

        assert_chunks_eq(&split_chunks, &whole_chunks);
        // And the content is what we expect.
        assert_chunks_eq(
            &split_chunks,
            &[
                Chunk::Text {
                    idx: 0,
                    delta: "hello world".into(),
                },
                Chunk::Stop(StopReason::Stop),
            ],
        );
    }

    // ---- R-RPLY-030 --------------------------------------------------------------

    #[test]
    fn yields_expected_chunks() {
        let p = ReplayProvider::from_fixture("replay", "parallel_tools").unwrap();
        assert_eq!(p.name(), "replay");
        let (chunks, err) = drain(&p);
        assert!(err.is_none());
        assert_chunks_eq(
            &chunks,
            &[
                Chunk::Text {
                    idx: 0,
                    delta: "I'll read both files.".into(),
                },
                Chunk::ToolCall {
                    idx: 0,
                    id: kn9t_provider_core::CallId("call_a".into()),
                    name: "read".into(),
                },
                Chunk::ToolCall {
                    idx: 1,
                    id: kn9t_provider_core::CallId("call_b".into()),
                    name: "read".into(),
                },
                Chunk::ToolArgs {
                    idx: 0,
                    delta: "{\"path\":\"a".into(),
                },
                Chunk::ToolArgs {
                    idx: 1,
                    delta: "{\"path\":\"b".into(),
                },
                Chunk::ToolArgs {
                    idx: 0,
                    delta: ".txt\"}".into(),
                },
                Chunk::ToolArgs {
                    idx: 1,
                    delta: ".txt\"}".into(),
                },
                Chunk::Usage(kn9t_provider_core::Usage {
                    tokens: kn9t_provider_core::Tokens {
                        input: 42,
                        output: 18,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    model: ModelRef {
                        provider: "replay".into(),
                        id: "parallel-tools".into(),
                    },
                }),
                Chunk::Stop(StopReason::ToolUse),
            ],
        );
    }

    // ---- R-RPLY-035 --------------------------------------------------------------

    #[test]
    fn status_maps_to_prestream_error() {
        // A 429 fixture returns the pre-stream ProvErr from `stream()` itself, yielding
        // no chunks.
        let p = ReplayProvider::from_fixture("raw-sse", "rate_limited_429").unwrap();
        let m = model();
        let req = empty_request(&m);
        let cancel = Cancel::new();
        match p.stream(&req, &cancel) {
            Err(kn9t_provider_core::ProvErr::Http { status, .. }) => assert_eq!(status, 429),
            other => panic!("expected pre-stream Http 429, got {:?}", other.map(|_| "iter")),
        }
    }

    // ---- R-RPLY-040 --------------------------------------------------------------

    #[test]
    fn classification_fixtures_exist_and_map() {
        // clean context-deadline -> StopReason::Length, NOT an error.
        let (chunks, err) = drain(&ReplayProvider::from_fixture("replay", "stop_length").unwrap());
        assert!(err.is_none());
        assert!(matches!(chunks.last(), Some(Chunk::Stop(StopReason::Length))));

        // prompt too long -> ContextOverflow (via terminal-error, twin carries raw text).
        let (_c, err) =
            drain(&ReplayProvider::from_fixture("replay", "context_overflow").unwrap());
        assert!(matches!(err, Some(kn9t_provider_core::ProvErr::ContextOverflow)));

        // unfinished tool call -> Truncated.
        let (chunks, err) =
            drain(&ReplayProvider::from_fixture("replay", "truncated_toolcall").unwrap());
        assert!(matches!(err, Some(kn9t_provider_core::ProvErr::Truncated)));
        // partial content was delivered before the error.
        assert!(!chunks.is_empty());

        // mid-stream error frame -> Stream(msg).
        let (_c, err) = drain(&ReplayProvider::from_fixture("replay", "stream_error").unwrap());
        assert!(matches!(err, Some(kn9t_provider_core::ProvErr::Stream(_))));

        // The raw twins are checked-in and load as verbatim bytes (parsed for real at 09).
        let raw = ReplayProvider::from_fixture("raw-sse", "prompt_too_long").unwrap();
        assert!(String::from_utf8_lossy(&raw.fixture().body).contains("prompt is too long"));
        let raw = ReplayProvider::from_fixture("raw-sse", "truncated_econnreset").unwrap();
        assert!(raw.fixture().body.len() > 0);
    }

    // ---- R-RPLY-050 --------------------------------------------------------------

    #[test]
    fn record_roundtrip() {
        // Record a source provider's chunk stream to a fixture, then replay it and assert
        // byte-identical chunk output.
        let source = ReplayProvider::from_fixture("replay", "parallel_tools").unwrap();
        let (source_chunks, _) = drain(&source);

        let tmp = std::env::temp_dir().join(format!("kn9t-rec-{}", std::process::id()));
        let recorder = RecordingProvider::new(&source, &tmp);
        let m = model();
        let req = empty_request(&m);
        let cancel = Cancel::new();
        let (path, teed) = recorder.record(&req, &cancel, "roundtrip").expect("record");

        // The tee did not alter the chunks.
        assert_chunks_eq(&teed, &source_chunks);

        // Reload the produced fixture and replay it.
        let replayed = ReplayProvider::from_file(&path).unwrap();
        let (replay_chunks, err) = drain(&replayed);
        assert!(err.is_none());
        assert_chunks_eq(&replay_chunks, &source_chunks);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn recorder_redacts_secrets() {
        // R-RPLY-020: Authorization and api_key header values are redacted on write.
        assert_eq!(redact_header_value("Authorization", "token sgp_secret"), "<redacted>");
        assert_eq!(redact_header_value("x-api_key", "abc123"), "<redacted>");
        assert_eq!(redact_header_value("note", "keep me"), "keep me");

        // And serialize_fixture applies it.
        let f = Fixture {
            kind: "replay".into(),
            status: 200,
            content_type: "text/event-stream".into(),
            chunks: Vec::new(),
            extra: vec![("authorization".into(), "token sgp_leak".into())],
            body: b"data: [DONE]\n\n".to_vec(),
        };
        let out = String::from_utf8(serialize_fixture(&f)).unwrap();
        assert!(out.contains("authorization: <redacted>"));
        assert!(!out.contains("sgp_leak"));
    }

    // ---- SSE encode/decode symmetry (supports R-RPLY-050/070) --------------------

    #[test]
    fn encode_then_replay_is_identity() {
        let chunks = vec![
            Chunk::Thinking {
                idx: 0,
                delta: "hmm".into(),
            },
            Chunk::Text {
                idx: 0,
                delta: "ok".into(),
            },
            Chunk::Stop(StopReason::Stop),
        ];
        let body = encode_chunks_sse(&chunks).unwrap();
        let mut raw = b"kind: replay\nstatus: 200\ncontent-type: text/event-stream\n\n".to_vec();
        raw.extend_from_slice(&body);
        let f = Fixture::parse(&raw).unwrap();
        let p = ReplayProvider::from_fixture_struct(f);
        let (got, err) = drain(&p);
        assert!(err.is_none());
        assert_chunks_eq(&got, &chunks);
    }
}
