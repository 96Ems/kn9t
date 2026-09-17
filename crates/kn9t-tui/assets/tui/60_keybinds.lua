-- 60_keybinds.lua — keybinds, commands, and the tool-card renderer choice.
--
-- Panel toggles call kn9t.invalidate(): the render cache is fingerprinted from state Rust
-- can see, and a Lua-local flag is not part of that, so without it the next redraw reuses
-- the stale tree.

-- ── Panel toggles ───────────────────────────────────────────────────────────
-- F1 was the session column's toggle. The tab bar replaced that column (PLAN §P7 D3), so
-- F1 is free; L2 binds it to the file explorer. F5 is kept as an alias: it was this file's
-- original right-sidebar toggle.
kn9t.map("F2", function()
    TUI.show.right_sidebar = not TUI.show.right_sidebar
    kn9t.invalidate()
end)

kn9t.map("F5", function()
    TUI.show.right_sidebar = not TUI.show.right_sidebar
    kn9t.invalidate()
end)

-- ── Scroll ──────────────────────────────────────────────────────────────────
kn9t.map("F7", function() kn9t.action("scroll_top") end)
kn9t.map("F8", function() kn9t.action("scroll_bottom") end)

-- ── Plugin focus cycling ────────────────────────────────────────────────────
kn9t.map("C-g", TUI.focus_cycle)
kn9t.map("F10", TUI.focus_cycle)

-- Toggle main-plugin visibility (show even when unfocused).
kn9t.map("F9", function()
    TUI.show.main_plugins = not TUI.show.main_plugins
    kn9t.invalidate()
end)

-- ── Header buttons ──────────────────────────────────────────────────────────
-- The "+ new" tab button routes through the same action as Ctrl+N and `/new`, so the
-- button cannot drift from the binding.
kn9t.on_click("tab_new", function()
    kn9t.action("new_session")
    return true
end)

-- ── Tool cards ──────────────────────────────────────────────────────────────
-- How each tool's card renders. Returning nil falls back to the built-in mapping, so only
-- the tools you care about need an entry. This is the hook that lets a plugin's tool pick
-- a renderer: the choice used to be hardcoded in Rust, so only the four built-in tools
-- could ever be styled.
--
--   "diff"      show diff/progress lines      (edit, write)
--   "summary"   header summary only           (read)
--   "streaming" live progress, then output    (bash)
--   "output"    command output                (everything else)
local TOOL_MODES = {
    edit  = "diff",
    write = "diff",
    read  = "summary",
    bash  = "streaming",
}

function tool_mode(name)
    return TOOL_MODES[name]
end
