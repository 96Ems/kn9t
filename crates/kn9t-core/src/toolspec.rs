//! R-CORE-120 — tool specification.

use serde::{Deserialize, Serialize};

/// R-CORE-120 — the `schema` value MUST NOT be produced from a `HashMap`; object
/// key order is stable across processes (GI-3). `serde_json::Value::Object` is
/// `BTreeMap`-backed by default, which satisfies this as long as `preserve_order`
/// is off.
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
}
