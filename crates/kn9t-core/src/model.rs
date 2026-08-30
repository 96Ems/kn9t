//! R-CORE-070 .. R-CORE-095 — models, pricing, thinking, quirks.

use crate::cache::CacheMode;
use serde::{Deserialize, Serialize};

/// R-CORE-070
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

/// R-CORE-070
#[derive(Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub r#ref: ModelRef,
    /// May differ from `ref.id`, e.g. the ":1m" pair (NBED).
    pub api_id: String,
    pub ctx_window: u32,
    pub max_out: u32,
    pub price: Price,
    /// Carries `min_tokens`.
    pub cache: CacheMode,
    /// `false` ⇒ synthesize chunks (NBED §8.7.4).
    pub streaming: bool,
    pub quirks: Quirks,
}

/// R-CORE-080 — USD per 1,000,000 tokens, all four tiers, so the write-time cost
/// projection (STOR §6.1) can compute each tier separately.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// R-CORE-090
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

/// R-CORE-090
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Thinking {
    Off,
    Effort(Effort),
    Budget(u32),
}

/// R-CORE-095 — whether persisted thinking reaches the wire on replay.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingReplay {
    Verbatim,
    Strip,
}

impl Default for ThinkingReplay {
    fn default() -> Self {
        ThinkingReplay::Verbatim
    }
}

/// R-CORE-095 — wire divergences that are config data (§8.2), never URL-sniffed.
/// The full field set is enumerated in PCORE/OAI (05); core defines at least
/// `thinking_replay`, the one quirk core behavior depends on. Field ordering when
/// serialized is deterministic (struct field order), never a `HashMap` (GI-3).
/// Constructible with all-default values.
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Quirks {
    pub thinking_replay: ThinkingReplay,
}
