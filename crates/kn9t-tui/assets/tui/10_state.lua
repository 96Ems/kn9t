-- 10_state.lua — mutable Lua-local state every other file reads/writes, and the
-- documented shape of the state the host publishes.
--
-- ── What the host publishes (refreshed every frame, cheap and bounded) ──────
--
--   kn9t.state.session       {id, title, cwd, streaming, aborting, has_lease, last_seq}
--   kn9t.state.usage         {turn={input,output,cache_read,cache_write},
--                             total={...}, cost, toks_per_sec}
--   kn9t.state.recent_tools  [{name, status}, ...] newest first, capped
--   kn9t.state.sessions      [{id, name, is_current}, ...] all known sessions - already
--                            cached, no HTTP call; feed a "list" widget and
--                            kn9t.action("switch_session", id)
--   kn9t.state.message_count number
--   kn9t.state.scroll        number
--   kn9t.context             {model, phase, input_height, tokens_in/out, cache_read,
--                             system_count, user_count, assistant_count, tool_count,
--                             ctx_window, max_out}  -- from the server; nil if unknown
--
-- On demand only (do NOT call every frame - copies text):
--   kn9t.get_messages(from, to)  -> [{role, content, tools={...}}, ...]
--   kn9t.get_tools()             -> [{name, description, plugin, enabled}, ...]
--
-- Rust cannot see any of the Lua-local state below; every handler that mutates it must
-- also call kn9t.invalidate() to force a redraw.

TUI.show = {
    right_sidebar = true,
    main_plugins  = false,  -- only show main plugins when focused
}

-- A transient message for the header's alert slot: TUI.set_alert({text=, fg=}) or nil.
TUI.alert = nil

function TUI.set_alert(a)
    TUI.alert = a
    kn9t.invalidate()
end

-- Focus cycling for plugin views (Ctrl+G / F10)
function TUI.focus_cycle()
    local names = (kn9t.state and kn9t.state.plugin_views) or {}
    if #names == 0 then return false end
    local focused = (kn9t.state and kn9t.state.focused_plugin) or ""
    if focused == "" then
        kn9t.action("focus_plugin", names[1])
        return
    end
    for i, n in ipairs(names) do
        if n == focused then
            if i < #names then
                kn9t.action("focus_plugin", names[i + 1])
            else
                kn9t.action("focus_plugin")   -- past the last: release
            end
            return
        end
    end
    -- Focused view vanished (plugin unloaded): start over.
    kn9t.action("focus_plugin", names[1])
end
