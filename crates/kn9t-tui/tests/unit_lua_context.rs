//! Unit tests for lua/context — extracted from src/lua/context.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::lua::context::{update_context, ContextStats};
use kn9t_tui::lua::panels::install_panel_api;
use mlua::{Lua, Table};

#[test]
fn test_update_context() {
    let lua = Lua::new();
    install_panel_api(&lua).unwrap();

    let stats = ContextStats {
        ctx_window: Some(200_000),
        max_out: Some(8192),
        system_count: 1,
        user_count: 5,
        assistant_count: 4,
        tool_count: 3,
        tokens_in: 1000,
        tokens_out: 500,
        cache_read: 800,
        cache_write: 200,
        cost: 0.05,
        model: "claude-3".to_string(),
        phase: "idle".to_string(),
        title: "Test session".to_string(),
        input_height: 3,
        streaming: false,
        plugins_ready: true,
    };

    update_context(&lua, &stats).unwrap();

    let ctx: Table = lua.load("return kn9t.context").eval().unwrap();
    assert_eq!(ctx.get::<i64>("user_count").unwrap(), 5);
    assert_eq!(ctx.get::<i64>("tokens_in").unwrap(), 1000);
    assert_eq!(ctx.get::<String>("model").unwrap(), "claude-3");
    assert_eq!(ctx.get::<i64>("input_height").unwrap(), 3);
    assert!(!ctx.get::<bool>("streaming").unwrap());
}
