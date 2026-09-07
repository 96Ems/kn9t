-- 10_state.lua — mutable Lua-local state every other file reads/writes.
--
-- Rust cannot see any of this; every handler that mutates it must also call
-- kn9t.invalidate() to force a redraw.

TUI.show = {
    left_sidebar  = true,
    right_sidebar = true,
}

-- Which native/plugin view occupies the main pane.
TUI.MAIN_VIEW = "chat"   -- "chat" or a plugin name

-- A transient message for the header's alert slot.
TUI.alert = nil

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
