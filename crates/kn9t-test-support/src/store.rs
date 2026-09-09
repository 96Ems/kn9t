//! Stub store for testing.

use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

use kn9t_core::{
    Cache, CompactSpan, Event, Message, ModelRef,
    RequestPlan, SessionId, SessionSnapshot, StoreErr, ToolSpec,
};
use crate::tags::event_tag;

/// What one `plan_request` returns.
#[derive(Clone)]
pub struct PlanScript {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub compact: bool,
}

impl PlanScript {
    /// A simple plan with messages and no compaction.
    pub fn plain(messages: Vec<Message>) -> Self {
        PlanScript {
            messages,
            tools: Vec::new(),
            compact: false,
        }
    }

    /// A plan that triggers compaction.
    pub fn compacting() -> Self {
        PlanScript {
            messages: Vec::new(),
            tools: Vec::new(),
            compact: true,
        }
    }

    /// Builder: add tools to this plan.
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }
}

/// A store stub that:
/// - records every appended event (assigning a monotonic seq);
/// - serves a scripted sequence of `plan_request` results (so compaction can be forced).
pub struct StubStore {
    pub appended: Arc<Mutex<Vec<Event>>>,
    seq: Arc<Mutex<u64>>,
    plans: Arc<Mutex<VecDeque<PlanScript>>>,
    default_plan: PlanScript,
    pub plan_calls: Arc<Mutex<u32>>,
}

impl StubStore {
    pub fn new(default_plan: PlanScript) -> Self {
        StubStore {
            appended: Arc::new(Mutex::new(Vec::new())),
            seq: Arc::new(Mutex::new(0)),
            plans: Arc::new(Mutex::new(VecDeque::new())),
            default_plan,
            plan_calls: Arc::new(Mutex::new(0)),
        }
    }

    /// Queue scripted plan responses; when exhausted, `default_plan` is used.
    pub fn script(mut self, plans: Vec<PlanScript>) -> Self {
        self.plans = Arc::new(Mutex::new(plans.into()));
        self
    }

    /// Returns the event type names of all appended events.
    pub fn appended_tags(&self) -> Vec<String> {
        self.appended
            .lock()
            .unwrap()
            .iter()
            .map(event_tag)
            .collect()
    }
}

fn test_model_ref() -> ModelRef {
    ModelRef {
        provider: "replay".to_string(),
        id: "test".to_string(),
    }
}

impl kn9t_core::Store for StubStore {
    fn plan_request(&self, _session: &SessionId) -> Result<RequestPlan, StoreErr> {
        *self.plan_calls.lock().unwrap() += 1;
        let script = self
            .plans
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.default_plan.clone());
        let cache: Vec<Cache> = Vec::new();
        let compact = if script.compact {
            Some(CompactSpan {
                replaced: kn9t_core::SeqRange { start: 0, end: 1 },
                messages: script.messages.clone(),
            })
        } else {
            None
        };
        Ok(RequestPlan {
            system: None,
            messages: script.messages,
            tools: script.tools,
            cache,
            compact,
        })
    }

    fn append(&self, _session: &SessionId, event: Event) -> Result<u64, StoreErr> {
        let mut seq = self.seq.lock().unwrap();
        *seq += 1;
        self.appended.lock().unwrap().push(event);
        Ok(*seq)
    }

    fn snapshot(&self, _session: &SessionId) -> Result<SessionSnapshot, StoreErr> {
        Ok(SessionSnapshot {
            head_seq: *self.seq.lock().unwrap(),
            ctx_tokens: 0,
            cost_usd: 0.0,
            cost_micros: 0,
            model: test_model_ref(),
            disabled_tools: Vec::new(),
        })
    }
}
