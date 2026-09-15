//! Ordered tool registry: maintains insertion order for cache stability.

use crate::toolspec::ToolSpec;
use crate::traits::Tool;
use std::sync::Arc;

/// Ordered tool set: lookup by name, deterministic iteration order (insertion).
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

    /// Returns first tool matching the name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.0.iter().find(|t| t.spec().name == name)
    }

    /// Returns all tool specs in stable order (visible and hidden).
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.0.iter().map(|t| t.spec().clone()).collect()
    }

    /// Tool specs for visible tools only (hidden=false).
    /// Use this for the initial system prompt / tools array sent to the LLM.
    /// Hidden tools can still be executed once discovered via meta-tools.
    pub fn visible_specs(&self) -> Vec<ToolSpec> {
        self.0
            .iter()
            .map(|t| t.spec().clone())
            .filter(|s| !s.hidden)
            .collect()
    }

    /// Iterate tools in order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.0.iter()
    }

    /// Returns a filtered registry containing only the named tools (in original order).
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
