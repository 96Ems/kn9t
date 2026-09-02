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
/// 96E-14: stored as integer micros (1 USD = 1_000_000 micros) for deterministic
/// persisted accounting. Wire accepts both f64 dollars (legacy) and i64 micros.
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

/// 96E-14: integer money in micros (1_000_000 micros = 1 USD). Deterministic.
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
                // Already micros (integer). Heuristic: if absolute value > 1000, treat as micros,
                // otherwise it could be dollars like 3.0 -> 3, which would be ambiguous.
                // We treat any integer as micros; callers that pass 3 (meaning $3) should have
                // used 3_000_000. Config TOML uses f64 like 3.0, which will be Number with is_f64,
                // not is_i64, so this branch is for new integer payloads only.
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

/// 96E-14 helper: deterministic cost in micros from tokens and price (micros per 1M).
pub fn cost_micros(tokens: &crate::usage::Tokens, price: &Price) -> i64 {
    // price is micros per 1M tokens, so cost = tokens * price / 1_000_000, rounded.
    // Use i128 to avoid overflow: tokens up to ~1e9, price up to ~75e6 => product ~7.5e16 fits in i64 but sum may exceed, so use i128.
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
