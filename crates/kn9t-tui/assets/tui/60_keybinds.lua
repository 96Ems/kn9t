-- 60_keybinds.lua — keybinds and commands (generic, no plugin deps).

-- Sidebar toggles
kn9t.map("F1", function()
    TUI.show.left_sidebar = not TUI.show.left_sidebar
    kn9t.invalidate()
end)

kn9t.map("F2", function()
    TUI.show.right_sidebar = not TUI.show.right_sidebar
    kn9t.invalidate()
end)

-- Alias F5 for right sidebar (legacy)
kn9t.map("F5", function()
    TUI.show.right_sidebar = not TUI.show.right_sidebar
    kn9t.invalidate()
end)

-- Scroll
kn9t.map("F7", function() kn9t.action("scroll_top") end)
kn9t.map("F8", function() kn9t.action("scroll_bottom") end)

-- Plugin focus cycling
kn9t.map("C-g", TUI.focus_cycle)
kn9t.map("F10", TUI.focus_cycle)

-- Toggle main plugin visibility (show even when unfocused)
kn9t.map("F9", function()
    TUI.show.main_plugins = not TUI.show.main_plugins
    kn9t.invalidate()
end)

-- Tool display modes
local TOOL_MODES = {
    edit  = "diff",
    write = "diff",
    read  = "summary",
    bash  = "streaming",
}

function tool_mode(name)
    return TOOL_MODES[name]
end
