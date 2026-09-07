//! Serializes `git::GitState` to JSON and defines the Lua `render(state)`
//! sent once via `ui_register_lua`.
//!
//! Kept intentionally small: colours/layout are minimal here on purpose,
//! since `~/.kn9t/tui/30_sidebar_left.lua` is the thing actually deciding
//! how (and whether) this plugin's view is framed — this is the content, not
//! the chrome.

use crate::git::GitState;

/// JSON pushed via `ui_set_state`. `null` (not an empty object) when `cwd`
/// is not a git repository, so the Lua side can render a distinct message
/// instead of an empty-but-misleadingly-"clean" status.
pub fn state_to_json(state: Option<&GitState>) -> serde_json::Value {
    match state {
        None => serde_json::Value::Null,
        Some(s) => serde_json::json!({
            "branch": s.branch,
            "ahead": s.ahead,
            "behind": s.behind,
            "changes": s.changes.iter().map(|c| serde_json::json!({
                "status": c.status,
                "path": c.path,
            })).collect::<Vec<_>>(),
            "recent": s.recent.iter().map(|l| serde_json::json!({
                "sha": l.sha,
                "subject": l.subject,
            })).collect::<Vec<_>>(),
        }),
    }
}

/// Lua defining `render(state)`, sent once via `ui_register_lua` (256 KB cap
/// — this is well under it). `state` is exactly what `state_to_json` above
/// produced, round-tripped through the host's JSON<->Lua conversion.
pub const LUA_SOURCE: &str = r#"
function render(state)
    if state == nil then
        return {
            type = "text",
            content = "(not a git repository)",
            fg = "darkgray",
        }
    end

    local lines = {}
    local branch = state.branch or "?"
    local ab = ""
    if (state.ahead or 0) > 0 then ab = ab .. " +" .. state.ahead end
    if (state.behind or 0) > 0 then ab = ab .. " -" .. state.behind end
    table.insert(lines, { text = branch .. ab, fg = "cyan", bold = true })

    local changes = state.changes or {}
    if #changes == 0 then
        table.insert(lines, { text = "(clean)", fg = "darkgray" })
    else
        for _, c in ipairs(changes) do
            local col = "yellow"
            if c.status == "?" then col = "green"
            elseif c.status == "D" then col = "lightred"
            end
            table.insert(lines, { text = c.status .. " " .. c.path, fg = col })
        end
    end

    table.insert(lines, { text = "", fg = "darkgray" })
    table.insert(lines, { text = "recent:", fg = "darkgray" })
    local recent = state.recent or {}
    for _, l in ipairs(recent) do
        table.insert(lines, { text = l.sha .. " " .. l.subject, fg = "gray" })
    end

    -- One "text" node per line rather than joining with "\n": this keeps
    -- each line's own colour, which a single content= string cannot carry.
    local children = {}
    for _, spec in ipairs(lines) do
        table.insert(children, {
            type = "text",
            content = spec.text,
            fg = spec.fg,
            bold = spec.bold,
            size = { fixed = 1 },
            wrap = false,
        })
    end

    return {
        type = "split",
        direction = "vertical",
        children = children,
    }
end
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{FileChange, LogEntry};

    #[test]
    fn state_to_json_none_is_null() {
        assert_eq!(state_to_json(None), serde_json::Value::Null);
    }

    #[test]
    fn state_to_json_round_trips_fields() {
        let state = GitState {
            branch: Some("main".to_string()),
            ahead: 2,
            behind: 0,
            changes: vec![FileChange {
                status: "M".to_string(),
                path: "a.rs".to_string(),
            }],
            recent: vec![LogEntry {
                sha: "abc1234".to_string(),
                subject: "fix".to_string(),
            }],
        };
        let json = state_to_json(Some(&state));
        assert_eq!(json["branch"], "main");
        assert_eq!(json["ahead"], 2);
        assert_eq!(json["changes"][0]["status"], "M");
        assert_eq!(json["recent"][0]["sha"], "abc1234");
    }

    /// The Lua source must at least be syntactically valid and define
    /// `render`, callable with both the "not a repo" (nil) and a real state
    /// shape — this is what a config author would hit first if this string
    /// ever had a typo, so it must be caught by `cargo test`, not by running
    /// the plugin live inside a TUI session.
    #[test]
    fn lua_source_parses_and_render_handles_both_shapes() {
        let lua = mlua::Lua::new();
        lua.load(LUA_SOURCE).exec().expect("LUA_SOURCE must parse");

        let render: mlua::Function = lua.globals().get("render").unwrap();

        // Nil state (not a repo).
        let widget: mlua::Table = render.call(mlua::Value::Nil).unwrap();
        assert_eq!(widget.get::<String>("type").unwrap(), "text");

        // Real state, round-tripped through the same JSON this plugin sends.
        let state = GitState {
            branch: Some("main".to_string()),
            ahead: 1,
            behind: 0,
            changes: vec![FileChange {
                status: "M".to_string(),
                path: "a.rs".to_string(),
            }],
            recent: vec![LogEntry {
                sha: "abc1234".to_string(),
                subject: "fix".to_string(),
            }],
        };
        let json = state_to_json(Some(&state));
        let lua_value = json_to_lua_for_test(&lua, &json);
        let widget: mlua::Table = render.call(lua_value).unwrap();
        assert_eq!(widget.get::<String>("type").unwrap(), "split");
    }

    /// Minimal JSON->Lua conversion for the test above. Deliberately NOT the
    /// production path (that lives host-side in kn9t-tui's plugin_ui.rs) —
    /// this only needs to support the flat shape state_to_json produces.
    fn json_to_lua_for_test(lua: &mlua::Lua, v: &serde_json::Value) -> mlua::Value {
        match v {
            serde_json::Value::Null => mlua::Value::Nil,
            serde_json::Value::Bool(b) => mlua::Value::Boolean(*b),
            serde_json::Value::Number(n) => mlua::Value::Number(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::String(s) => mlua::Value::String(lua.create_string(s).unwrap()),
            serde_json::Value::Array(arr) => {
                let t = lua.create_table().unwrap();
                for (i, item) in arr.iter().enumerate() {
                    t.set(i + 1, json_to_lua_for_test(lua, item)).unwrap();
                }
                mlua::Value::Table(t)
            }
            serde_json::Value::Object(map) => {
                let t = lua.create_table().unwrap();
                for (k, val) in map {
                    t.set(k.as_str(), json_to_lua_for_test(lua, val)).unwrap();
                }
                mlua::Value::Table(t)
            }
        }
    }
}
