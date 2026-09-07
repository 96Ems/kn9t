//! kn9t-git-status — background git status/log poller for the TUI sidebar.
//!
//! Ships a `git_status` tool (real, agent-callable — "show me the current
//! git state" is a legitimate ask) that doubles as the bootstrap trigger for
//! a background poll thread: the plugin protocol gives no way to reach the
//! host API outside a live tool/hook dispatch (`PluginHook::call` gets no
//! `ctx`, and there is no session-start hook), so the first `git_status`
//! call in a session is what starts the thread that keeps pushing updates
//! afterward on its own timer. See `poller.rs` for the detail.
//!
//! Rust does the git invocation (real subprocess, real shell access — this
//! process, unlike the TUI's Lua sandbox, is exactly where that belongs);
//! Lua only renders. `ui.lua` is sent once via `ui_register_lua`, then every
//! poll pushes fresh JSON via `ui_set_state`. See AGENTS.md 11.1 and the
//! session's design notes for why the split is drawn there.

mod git;
mod poller;
mod tool;
mod ui;

fn main() {
    kn9t_plugin_sdk::Plugin::new("kn9t-git-status")
        .tool(tool::GitStatus)
        .run();
}
