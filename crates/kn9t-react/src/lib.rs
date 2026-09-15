//! ReAct loop: turn sequencing, provider calls, tool dispatch, truncation, compaction, and hooks.
//! Owns only trait objects; never depends on concrete implementations.
//! This is the only component that calls providers and emits usage records.

mod assembler;
mod exec;
mod hooks;
mod loop_;
mod turn;

pub use assembler::{assemble, Assembled};
pub use loop_::{
    static_tools, FilteredTools, ReactConfig, ReactError, ReactLoop, ReadMap, RunParams,
    StaticTools, ToolSource,
};

// Re-export hook surface for callers.
pub use kn9t_provider_core::{HookHost, HookVeto, NextTurnPatch, NoopHookHost};

// Re-export internal helpers for integration tests (tests/unit_exec.rs).
// `#[doc(hidden)]` keeps them out of the public docs while making them
// accessible to the test binary, which links the crate in non-test mode.
#[doc(hidden)]
pub use exec::{ensure_nonempty_content, estimated_assembled, synth_error, CallPlan};
