//! Unit tests for render_cache — extracted from src/render_cache.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::render_cache::RenderCache;

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
