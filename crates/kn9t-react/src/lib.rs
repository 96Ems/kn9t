//! # kn9t-react
//!
//! The ReAct loop: turn sequencing, cancellation/abort accounting, the truncation ladder,
//! compaction execution, and hook integration (spec `03-react-tools.md` Part A).
//!
//! The loop ([`ReactLoop`]) owns only trait objects (GI-1): it never names a concrete
//! `Provider`, `Tool`, `Store`, or `Policy`. It is the only component that calls a provider
//! or emits `UsageRecorded` (DESIGN sec.3).
//!
//! GI-1: the only workspace dependency is `kn9t-core`. GI-5: no async anywhere -- the loop
//! is straight-line blocking code on OS threads.
//!
//! The hook surface ([`kn9t_provider_core::HookHost`], `HookVeto`, `NextTurnPatch`) is defined in
//! `kn9t-core` and re-exported here (R-RCT-100).

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

// R-RCT-100: re-export the hook surface so callers can `use kn9t_react::HookHost`.
pub use kn9t_provider_core::{HookHost, HookVeto, NextTurnPatch, NoopHookHost};

// Re-export internal helpers for integration tests (tests/unit_exec.rs).
// `#[doc(hidden)]` keeps them out of the public docs while making them
// accessible to the test binary, which links the crate in non-test mode.
#[doc(hidden)]
pub use exec::{ensure_nonempty_content, estimated_assembled, synth_error, CallPlan};
