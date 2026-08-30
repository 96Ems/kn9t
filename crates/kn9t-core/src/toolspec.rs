//! R-CORE-120 — tool specification.

use serde::{Deserialize, Serialize};

/// R-CORE-120 — the `schema` value MUST NOT be produced from a `HashMap`; object
/// key order is stable across processes (GI-3). `serde_json::Value::Object` is
/// `BTreeMap`-backed by default, which satisfies this as long as `preserve_order`
/// is off.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Shell,
    FsRead,
    FsWrite,
    Network,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Effect {
    /// JSON field name in `args` (e.g. `"cmd"` or `"path"`), or JSON pointer
    /// with leading `/` (e.g. `"/command"`). Bare string is treated as top-level key.
    pub field: String,
    pub kind: EffectKind,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// Hand-written `json!({...})`, ordered.
    pub schema: serde_json::Value,
    /// If true, tool is registered but not shown in the initial system prompt.
    /// Used for lazy tool discovery: hidden tools can still be executed once
    /// the agent discovers them via a meta-tool like `mcp_search_tools`.
    #[serde(default)]
    pub hidden: bool,
    /// Effects declared by the plugin (ADR-0002). Empty = strictest default
    /// (ask_on_mutation → Ask, deny_all → HardDeny).
    #[serde(default)]
    pub effects: Vec<Effect>,
}
