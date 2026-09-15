//! Provider response replay for offline testing: replays raw bytes through the genuine parser
//! to regression-test edge cases in parsing, token accounting, and SSE buffering.
//! Fixtures are immutable: tests must pass here and unchanged at later stages.

pub mod fixture;
pub mod record;
pub mod replay;
pub mod sse;

pub use fixture::Fixture;
pub use record::{
    encode_chunks_sse, redact_header_value, serialize_fixture, write_raw_fixture, RecordingProvider,
};
pub use replay::{fixtures_dir, ReplayProvider};
pub use sse::{data_events, SegmentedReader};
