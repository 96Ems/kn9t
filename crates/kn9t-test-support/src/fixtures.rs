//! Fixture helpers for building test data.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kn9t_core::{
    CacheMode, ModelRef, ModelSpec, Price, ProvErr, Quirks,
    Cancel, Chunk, Provider, Request,
};
use kn9t_provider_replay::fixture::Fixture;
use kn9t_provider_replay::ReplayProvider;

/// Canonical test model reference.
pub fn test_model_ref() -> ModelRef {
    ModelRef {
        provider: "replay".to_string(),
        id: "test".to_string(),
    }
}

/// Canonical test model spec with reasonable defaults.
pub fn test_model_spec() -> ModelSpec {
    ModelSpec {
        r#ref: test_model_ref(),
        api_id: "test".to_string(),
        ctx_window: 100_000,
        max_out: 8_000,
        price: Price {
            input: 1000000,
            output: 2000000,
            cache_read: 100000,
            cache_write: 1250000,
        },
        cache: CacheMode::None,
        streaming: true,
        quirks: Quirks::default(),
    }
}

/// Build a native `kind: replay` fixture from raw SSE body text.
pub fn fixture_from_body(body: &str) -> Fixture {
    Fixture {
        kind: "replay".to_string(),
        status: 200,
        content_type: "text/event-stream".to_string(),
        chunks: Vec::new(),
        extra: Vec::new(),
        body: body.as_bytes().to_vec(),
    }
}

/// Build a native fixture that ends with a `terminal-error:` header value.
pub fn fixture_with_terminal(body: &str, terminal: &str) -> Fixture {
    let mut f = fixture_from_body(body);
    f.extra
        .push(("terminal-error".to_string(), terminal.to_string()));
    f
}

/// Create a ReplayProvider from a fixture.
pub fn replay(fixture: Fixture) -> Arc<ReplayProvider> {
    Arc::new(ReplayProvider::from_fixture_struct(fixture))
}

/// Create an empty ReadMap for tests.
pub fn empty_read_map() -> kn9t_react::ReadMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// A provider that serves a scripted sequence of outcomes, one per `stream()` call, so the
/// truncation ladder (a sequence of `Truncated` then success) and abort paths can be driven
/// deterministically. Each entry is either a fixture or a pre-stream error.
pub enum StreamScript {
    Fixture(Fixture),
    PreStreamErr(ProvErr),
}

pub struct ScriptedProvider {
    scripts: Mutex<std::collections::VecDeque<StreamScript>>,
    pub calls: Arc<Mutex<u32>>,
}

impl ScriptedProvider {
    pub fn new(scripts: Vec<StreamScript>) -> Self {
        ScriptedProvider {
            scripts: Mutex::new(scripts.into()),
            calls: Arc::new(Mutex::new(0)),
        }
    }
}

impl Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    fn stream(
        &self,
        req: &Request,
        cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        *self.calls.lock().unwrap() += 1;
        let next = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("ScriptedProvider ran out of scripts");
        match next {
            StreamScript::Fixture(f) => {
                let p = ReplayProvider::from_fixture_struct(f);
                p.stream(req, cancel)
            }
            StreamScript::PreStreamErr(e) => Err(e),
        }
    }
}
