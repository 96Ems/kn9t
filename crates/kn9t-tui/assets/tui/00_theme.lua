-- 00_theme.lua — palette, globals, and TUI namespace
--
-- This file runs first. It creates the TUI namespace that all other files use.

-- ── TUI namespace ───────────────────────────────────────────────────────────
TUI = {}

-- ── Palette ─────────────────────────────────────────────────────────────────
-- Every colour comes from the configured theme (kn9t.theme), so editing
-- [theme.colors] in config.toml moves the whole UI. Override a slot here only to
-- deviate from the theme on purpose.
--
-- Three colours carry meaning, the rest is chrome (PLAN §P7 D16):
--   accent  selection, focus, active tab, headings, mentions
--   warn    attention: a running tool, cost, an approval, a truncation
--   danger  failure, abort, a full context window
-- `label` and `system` are deliberately chrome, not colours: labels are not
-- messages, and a system notice is not an alert.
local T = kn9t.theme or {}
TUI.color = {
    dim     = T.muted     or "darkgray",
    label   = T.muted     or "gray",
    value   = T.fg        or "white",
    -- Text placed *on* a solid colour block (a status chip, the active tab). The page
    -- colour cannot double as this: silver on violet is unreadable.
    ink     = T.ink       or "black",
    accent  = T.primary   or "cyan",
    ok      = T.success   or "lightgreen",
    warn    = T.warning   or "yellow",
    danger  = T.error     or "lightred",
    user    = T.user      or "cyan",
    asst    = T.assistant or "white",
    tool    = T.tool      or "yellow",
    system  = T.muted     or "darkgray",
}

-- ── Config ──────────────────────────────────────────────────────────────────
TUI.SIDEBAR_WIDTH   = 34
TUI.SIDEBAR_MIN_W   = 110     -- auto-hide sidebars below this terminal width

-- The file explorer column (PLAN §P7 L2 / D3). Visibility is owned by Rust (F1 toggles it,
-- and the same flag decides whether the explorer takes the keyboard), so it is read from
-- kn9t.state.explorer_visible rather than kept here, where the two could disagree.
TUI.EXPLORER_WIDTH = 32

-- The file viewer (PLAN §P7 L2 / D5) takes this share of the centre column when open.
TUI.VIEWER_PERCENT = 50

-- Session tabs (PLAN §P7 D2/D14). They are the only session list on screen, so the cap is
-- generous; past it the command palette's session picker is the way to reach a session.
TUI.TAB_MAX       = 10
TUI.TAB_LABEL_MAX = 22

TUI.PLUGIN_VIEW_ROWS         = 8
TUI.PLUGIN_VIEW_ROWS_FOCUSED = 24
TUI.PLUGIN_COL_WIDTH         = 34

-- Context window fallback (when provider doesn't advertise)
TUI.CONTEXT_FALLBACK = 200000
TUI.WARN_AT          = 0.75
TUI.DANGER_AT        = 0.90

-- ── Helpers ─────────────────────────────────────────────────────────────────
function TUI.context_window()
    return (kn9t.context and kn9t.context.ctx_window) or TUI.CONTEXT_FALLBACK
end

function TUI.fmt_tokens(n)
    n = n or 0
    if n >= 1000000 then return string.format("%.2fM", n / 1000000) end
    if n >= 1000 then return string.format("%.1fk", n / 1000) end
    return tostring(n)
end

function TUI.context_color(frac)
    if frac >= TUI.DANGER_AT then return TUI.color.danger end
    if frac >= TUI.WARN_AT then return TUI.color.warn end
    return TUI.color.ok
end

function TUI.kv_lines(rows, width)
    local out = {}
    for _, r in ipairs(rows) do
        local label, value = r[1], r[2]
        local pad = width - #label - #value
        if pad < 1 then pad = 1 end
        table.insert(out, label .. string.rep(" ", pad) .. value)
    end
    return table.concat(out, "\n")
end

function TUI.live_context()
    local ctx   = kn9t.context or {}
    local usage = kn9t.state and kn9t.state.usage or {}
    local turn  = usage.turn or {}
    local live = (turn.input or 0) + (turn.cache_read or 0)
    if live == 0 then
        live = (ctx.tokens_in or 0) + (ctx.cache_read or 0)
    end
    return live, live / TUI.context_window()
end

function TUI.phase_display(phase)
    local C = TUI.color
    if phase == "streaming" then return { "streaming", C.ok }
    elseif phase == "aborting" then return { "aborting", C.danger }
    elseif phase == "thinking" then return { "thinking", C.warn }
    elseif phase == "tool" then return { "tool", C.tool }
    else return { phase or "idle", C.dim }
    end
end
