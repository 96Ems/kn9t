//! R-TOOL-010 — the ordered tool registry.
//!
//! **Cross-crate note (DB-03).** R-TOOL-010 names this type in `kn9t-tools`, but R-RCT-010
//! gives `ReactLoop` a field `tools: ToolRegistry` while `kn9t-react` may depend on
//! `kn9t-core` only (GI-1) and "sees the tool only as `dyn Trait`" (spec 03). A concrete
//! type from `kn9t-tools` in the loop would be a second workspace dependency. Since the
//! registry is pure vocabulary — an ordered container of `Arc<dyn Tool>` with lookup — it
//! lives here (like [`crate::Cancel`], the other `Arc`-holding, non-payload core type,
//! R-CORE-240) and `kn9t-tools` re-exports it. GI-2 is preserved: `ToolRegistry` is never
//! an event payload.
//!
//! Order matters: the serialized `tools` array is part of the level-1 cache prefix
//! (§8.4.2.1), so it MUST be byte-stable across processes. A `HashMap` would reorder it
//! run to run and silently break the cache; the registry is therefore a `Vec` whose
//! iteration order is exactly insertion order (GI-3).

use crate::toolspec::ToolSpec;
use crate::traits::Tool;
use std::sync::Arc;

/// R-TOOL-010 — an ordered set of tools. Lookup by name is linear (v1 has four tools);
/// iteration/`specs()` order is deterministic (insertion order).
#[derive(Clone, Default)]
pub struct ToolRegistry(Vec<Arc<dyn Tool>>);

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry(Vec::new())
    }

    /// Append a tool. Later insertions keep a stable position after earlier ones.
    pub fn push(&mut self, tool: Arc<dyn Tool>) {
        self.0.push(tool);
    }

    /// Build from an ordered list.
    pub fn from_tools(tools: Vec<Arc<dyn Tool>>) -> Self {
        ToolRegistry(tools)
    }

    /// R-TOOL-010 — first tool whose `spec().name` equals `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.0.iter().find(|t| t.spec().name == name)
    }

    /// R-TOOL-010 — the tool specs in stable order, for the request `tools` array.
    /// Includes ALL tools (visible and hidden).
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.0.iter().map(|t| t.spec().clone()).collect()
    }

    /// Tool specs for visible tools only (hidden=false).
    /// Use this for the initial system prompt / tools array sent to the LLM.
    /// Hidden tools can still be executed once discovered via meta-tools.
    pub fn visible_specs(&self) -> Vec<ToolSpec> {
        self.0.iter()
            .map(|t| t.spec().clone())
            .filter(|s| !s.hidden)
            .collect()
    }

    /// Iterate tools in order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.0.iter()
    }

    /// 96E-17: restrict the registry to the given tool names (sub-agent toolset).
    /// Unknown names are silently dropped; order follows `self.0`.
    pub fn filter_names(&self, names: &[String]) -> ToolRegistry {
        ToolRegistry::from_tools(
            self.0
                .iter()
                .filter(|t| names.iter().any(|n| n == &t.spec().name))
                .cloned()
                .collect(),
        )
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
