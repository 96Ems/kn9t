//! Render cache — R-TUI-040.
//!
//! Caches expensive computations to avoid re-rendering unchanged content.
//! Key optimizations:
//! - Markdown rendering is cached per-message (content hash → rendered lines)
//! - Syntax highlighting is cached per code block
//! - Transcript lines are only rebuilt when messages change
//!
//! Cache invalidation:
//! - Message content change → invalidate that message's cache entry
//! - Live delta → always re-render (streaming)
//! - Tool status change (expanded, scroll, status) → invalidate that message's cache entry

use std::collections::HashMap;

use ratatui::text::Line;

/// Hash of message content for cache invalidation.
/// Includes tool info hash to invalidate when tools change status.
fn content_hash(content: &str, tool_info_hash: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    tool_info_hash.hash(&mut hasher);
    hasher.finish()
}

/// Compute a hash of tool statuses for cache invalidation.
/// Includes ALL state that affects rendering: expanded, scroll, status, output, progress.
pub fn compute_tool_info_hash(tools: &[crate::message_handler::ToolCard]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for t in tools {
        t.call_id.hash(&mut hasher);
        t.status.hash(&mut hasher);
        t.expanded.hash(&mut hasher);
        t.scroll_offset.hash(&mut hasher);
        t.active_tab.hash(&mut hasher);
        // Hash output length (not content, too expensive)
        t.output.as_ref().map(|o| o.len()).hash(&mut hasher);
        // Hash progress lines count
        t.progress_lines.len().hash(&mut hasher);
    }
    hasher.finish()
}

/// Tool position info for click detection.
#[derive(Debug, Clone)]
pub struct CachedToolInfo {
    pub call_id: String,
    /// Line index of header within the cached lines (relative to message start).
    pub header_line_offset: usize,
    /// Line index of content end within the cached lines (relative to message start).
    pub content_end_offset: usize,
}

/// Cached rendered lines for a single message.
#[derive(Debug, Clone)]
struct CachedMessage {
    content_hash: u64,
    lines: Vec<Line<'static>>,
    /// Tool positions relative to message start (for click detection).
    tool_infos: Vec<CachedToolInfo>,
}

/// Render cache for transcript messages.
#[derive(Debug, Default)]
pub struct RenderCache {
    /// Cache: message index → (content_hash, rendered lines).
    /// Key is message index because messages are append-only.
    messages: HashMap<usize, CachedMessage>,
    
    /// Last known message count (for invalidation).
    last_message_count: usize,
    
    /// Last known live delta length (to detect streaming changes).
    last_delta_len: usize,
    
    /// Last terminal width (re-render on resize).
    last_width: usize,
    
    /// Dirty flag: set when cache needs full rebuild.
    dirty: bool,
}

impl RenderCache {
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Mark cache as dirty (full rebuild needed).
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }
    
    /// Check if cache needs rebuild based on current state.
    pub fn needs_rebuild(
        &self,
        message_count: usize,
        delta_len: usize,
        width: usize,
    ) -> bool {
        self.dirty
            || message_count != self.last_message_count
            || width != self.last_width
            || delta_len != self.last_delta_len
    }
    
    /// Get cached lines and tool infos for a message, or None if not cached/stale.
    pub fn get_message(&self, index: usize, content: &str, tool_info_hash: u64) -> Option<(&[Line<'static>], &[CachedToolInfo])> {
        let entry = self.messages.get(&index)?;
        let hash = content_hash(content, tool_info_hash);
        if entry.content_hash == hash {
            Some((&entry.lines, &entry.tool_infos))
        } else {
            None
        }
    }
    
    /// Cache rendered lines for a message with tool position info.
    pub fn set_message(
        &mut self, 
        index: usize, 
        content: &str, 
        tool_info_hash: u64, 
        lines: Vec<Line<'static>>,
        tool_infos: Vec<CachedToolInfo>,
    ) {
        let hash = content_hash(content, tool_info_hash);
        self.messages.insert(index, CachedMessage {
            content_hash: hash,
            lines,
            tool_infos,
        });
    }
    
    /// Update state tracking after a render pass.
    pub fn update_state(&mut self, message_count: usize, delta_len: usize, width: usize) {
        self.last_message_count = message_count;
        self.last_delta_len = delta_len;
        self.last_width = width;
        self.dirty = false;
    }
    
    /// Prune stale cache entries (messages that no longer exist).
    pub fn prune(&mut self, current_message_count: usize) {
        self.messages.retain(|&idx, _| idx < current_message_count);
    }
    
    /// Clear all cached data (e.g., on session switch).
    pub fn clear(&mut self) {
        self.messages.clear();
        self.last_message_count = 0;
        self.last_delta_len = 0;
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_cache_state() {
        let mut cache = RenderCache::new();
        cache.update_state(5, 100, 80);
        
        // Same state - no rebuild
        assert!(!cache.needs_rebuild(5, 100, 80));
        
        // Different width - needs rebuild
        assert!(cache.needs_rebuild(5, 100, 100));
        
        // Different message count - needs rebuild
        assert!(cache.needs_rebuild(6, 100, 80));
        
        // Different delta - needs rebuild
        assert!(cache.needs_rebuild(5, 200, 80));
    }
    
    #[test]
    fn test_cache_clear() {
        let mut cache = RenderCache::new();
        cache.update_state(5, 100, 80);
        
        cache.clear();
        
        // After clear, needs rebuild
        assert!(cache.needs_rebuild(0, 0, 80));
        assert!(cache.dirty);
    }
}
