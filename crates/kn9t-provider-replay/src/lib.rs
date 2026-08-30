//! # kn9t-provider-replay
//!
//! Replays recorded provider responses so the whole test suite runs with **no network
//! and no API key** (DESIGN §16). A fixture is the *raw bytes a real provider returned*
//! plus a small header, replayed through the genuine parser — the only form that
//! regression-tests the three places bugs actually live: the `delta_tool_calls` index
//! bug, token accounting, and SSE chunk-boundary buffering.
//!
//! At stage 2 there is no real parser yet, so an inline SSE splitter ([`sse`]) stands in
//! for `kn9t-provider-core::sse_lines`; it is swapped for PCORE's at stage 5 (R-RPLY-070)
//! and every fixture that passes here must pass unchanged afterward.
//!
//! GI-1: the only workspace dependency is `kn9t-core` (plus budgeted `serde_json`).

pub mod fixture;
pub mod record;
pub mod replay;
pub mod sse;

pub use fixture::Fixture;
pub use record::{
    encode_chunks_sse, redact_header_value, serialize_fixture, write_raw_fixture,
    RecordingProvider,
};
pub use replay::{fixtures_dir, ReplayProvider};
pub use sse::{data_events, sse_lines, SegmentedReader, SseLines};
