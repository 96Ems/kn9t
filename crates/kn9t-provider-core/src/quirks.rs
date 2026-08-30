//! R-PCORE-080 — the full Quirks field set.
//! Note: `kn9t-core` declares a minimal `Quirks` with `thinking_replay`.
//! This module declares the *extended* Quirks used by provider-core and openai.
//! The two are kept separate: core's Quirks travels with ModelSpec (serialized
//! to SQLite), while ProviderQuirks lives only in provider config.

use serde::{Deserialize, Serialize};

/// R-PCORE-080 — wire divergences that are config data, never URL-sniffed.
/// Merged field-by-field: model block overrides provider block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quirks {
    /// `"max_tokens"` | `"max_completion_tokens"`
    #[serde(default = "default_max_tokens_field")]
    pub max_tokens_field: String,
    /// `"system"` | `"developer"`
    #[serde(default = "default_system_role")]
    pub system_role: String,
    /// Send `stream_options.include_usage = true` in request.
    #[serde(default)]
    pub usage_in_stream: bool,
    /// True when the stream carries a `finish_reason` field; false → infer from tool presence.
    #[serde(default = "default_true")]
    pub finish_reason: bool,
    /// `"reasoning_effort"` | `"budget_tokens"` | `"adaptive"` | `"none"`
    #[serde(default = "default_reasoning")]
    pub reasoning: String,
    /// True → include `name` field on tool-result messages.
    #[serde(default)]
    pub tool_result_name: bool,
    /// `"reasoning_content"` | `"tags"` | `"none"`
    #[serde(default = "default_thinking_style")]
    pub thinking_style: String,
    /// `"verbatim"` | `"strip"` — how persisted thinking reaches the wire on replay.
    #[serde(default = "default_verbatim")]
    pub thinking_replay: String,
    /// True → inject a placeholder tool when tools array is empty (adaptive gateway quirk).
    #[serde(default)]
    pub require_tools: bool,
    /// True → disable streaming, synthesize chunks from the full response.
    #[serde(default = "default_true")]
    pub streaming: bool,
    /// Arbitrary extra JSON to merge into the request body (e.g. LiteLLM metadata).
    #[serde(default)]
    pub extra_body: serde_json::Value,
}

fn default_max_tokens_field() -> String { "max_tokens".into() }
fn default_system_role()      -> String { "system".into() }
fn default_reasoning()        -> String { "none".into() }
fn default_thinking_style()   -> String { "none".into() }
fn default_verbatim()         -> String { "verbatim".into() }
fn default_true()             -> bool   { true }

impl Default for Quirks {
    fn default() -> Self {
        Quirks {
            max_tokens_field: default_max_tokens_field(),
            system_role:      default_system_role(),
            usage_in_stream:  false,
            finish_reason:    true,
            reasoning:        default_reasoning(),
            tool_result_name: false,
            thinking_style:   default_thinking_style(),
            thinking_replay:  default_verbatim(),
            require_tools:    false,
            streaming:        true,
            extra_body:       serde_json::Value::Null,
        }
    }
}

impl Quirks {
    /// R-PCORE-080 — merge: `model` override replaces exactly its named fields, inherits rest.
    pub fn merge(&self, model: &Quirks) -> Quirks {
        // In TOML, absent fields are left at default. We do a field-by-field override.
        Quirks {
            max_tokens_field: model.max_tokens_field.clone(),
            system_role:      model.system_role.clone(),
            usage_in_stream:  model.usage_in_stream,
            finish_reason:    model.finish_reason,
            reasoning:        model.reasoning.clone(),
            tool_result_name: model.tool_result_name,
            thinking_style:   model.thinking_style.clone(),
            thinking_replay:  model.thinking_replay.clone(),
            require_tools:    model.require_tools,
            streaming:        model.streaming,
            extra_body:       if model.extra_body.is_null() {
                                  self.extra_body.clone()
                              } else {
                                  model.extra_body.clone()
                              },
        }
    }
}
