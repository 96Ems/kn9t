//! Models: reference, specs, pricing, cache modes, and provider quirks.

use crate::cache::CacheMode;
use serde::{Deserialize, Serialize};

/// Model reference: provider name and model ID.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

/// Full model specification: reference, API ID, context window, pricing, and quirks.
#[derive(Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub r#ref: ModelRef,
    /// API ID: may differ from ref.id (e.g., special suffixes).
    pub api_id: String,
    pub ctx_window: u32,
    pub max_out: u32,
    /// Reasoning effort requested for this model's turns. Defaults to `medium`, which is
    /// the OpenAI-documented balance. A provider with `reasoning = "none"` never sends it.
    #[serde(default = "default_thinking")]
    pub thinking: Thinking,
    pub price: Price,
    /// Cache configuration: mode and minimum token threshold.
    pub cache: CacheMode,
    /// Whether provider supports streaming (false means chunks must be synthesized).
    pub streaming: bool,
    pub quirks: Quirks,
}

/// Price per 1M tokens for all four tiers: input, output, cache_read, cache_write.
/// Stored as integer micros (1 USD = 1_000_000 micros) for deterministic accounting.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    #[serde(deserialize_with = "de_micros", serialize_with = "ser_micros")]
    pub input: i64,
    #[serde(deserialize_with = "de_micros", serialize_with = "ser_micros")]
    pub output: i64,
    #[serde(deserialize_with = "de_micros", serialize_with = "ser_micros")]
    pub cache_read: i64,
    #[serde(deserialize_with = "de_micros", serialize_with = "ser_micros")]
    pub cache_write: i64,
}

/// Cost in integer micros: deterministic and avoids floating-point errors.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Debug)]
#[serde(transparent)]
pub struct MoneyMicros(pub i64);

impl MoneyMicros {
    pub fn from_dollars(d: f64) -> Self {
        MoneyMicros((d * 1_000_000.0).round() as i64)
    }
    pub fn as_dollars(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }
    pub fn as_micros(self) -> i64 {
        self.0
    }
}

fn de_micros<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                // Integers are treated as micros; floats are converted from dollars.
                Ok(i)
            } else if let Some(f) = n.as_f64() {
                Ok((f * 1_000_000.0).round() as i64)
            } else {
                Err(D::Error::custom("invalid number for micros"))
            }
        }
        _ => Err(D::Error::custom("expected number for micros")),
    }
}

fn ser_micros<S>(v: &i64, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    s.serialize_i64(*v)
}

/// Computes deterministic cost in micros from tokens and price (micros per 1M).
pub fn cost_micros(tokens: &crate::usage::Tokens, price: &Price) -> i64 {
    // Uses i128 to avoid overflow in multiplication.
    let input = tokens.input as i128 * price.input as i128;
    let output = tokens.output as i128 * price.output as i128;
    let cr = tokens.cache_read as i128 * price.cache_read as i128;
    let cw = tokens.cache_write as i128 * price.cache_write as i128;
    ((input + output + cr + cw) / 1_000_000) as i64
}

/// R-CORE-090
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

/// R-CORE-090 — reasoning is on by default: `medium` is the documented balance between
/// latency, quality and cost (OpenAI defaults its reasoning models to it).
pub fn default_thinking() -> Thinking {
    Thinking::Effort(Effort::Medium)
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
#[derive(Default)]
pub enum ThinkingReplay {
    #[default]
    Verbatim,
    Strip,
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
