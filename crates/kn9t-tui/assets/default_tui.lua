-- kn9t TUI — full UI defined in Lua
-- =================================
-- Rust renders; Lua decides layout, content and styling.
--
-- State, refreshed every frame (cheap — bounded size):
--   kn9t.state.session       {id, title, streaming, aborting, has_lease, last_seq}
--   kn9t.state.usage         {turn={input,output,cache_read,cache_write},
--                             total={...}, cost, toks_per_sec}
--   kn9t.state.recent_tools  [{name, status}, ...] newest first, capped
--   kn9t.state.sessions      [{id, name, is_current}, ...] all known sessions —
--                            already cached, no HTTP call; feed to a "list"
--                            widget and kn9t.action("switch_session", id)
--   kn9t.state.message_count number
--   kn9t.state.scroll        number
--   kn9t.context             {model, phase, input_height, tokens_in/out, cache_read,
--                             system_count, user_count, assistant_count, tool_count,
--                             ctx_window, max_out}  -- from the server; nil if unknown
--
-- On demand only (do NOT call every frame — copies text):
--   kn9t.get_messages(from, to)  -> [{role, content, tools={...}}, ...]
--   kn9t.get_tools()             -> [{name, description, plugin, enabled}, ...]
--
-- Widgets:
--   {type="native", view=...}   one of kn9t.native_views:
--                               "transcript" | "input" | "status" | "diff" | "welcome"
--   {type="text", content=... | spans={{text=,fg=,bold=},...},
--                 markdown=bool, syntax="lang", math=bool, linkify=bool,
--                 align="left"|"center"|"right", wrap=bool}
--   {type="box", title=..., title_align=..., border=false|"plain"|"rounded"|"thick"|"double",
--                border_fg=..., padding=N|{v,h}|{t,r,b,l}, child=...}
--   {type="split", direction="vertical"|"horizontal", children={...}}
--   {type="list", items={...}, selected=N, offset=N, selected_fg=..., selected_bg=...}
--   {type="gauge", frac=0..1, label=..., fg=..., filled="#", empty="-"}
--   {type="float", x=,y=,w=,h=, clear=bool, child=...}  -- popup over the layout
--   {type="spacer"}
--   {type="plugin", plugin="name"}
-- Any widget may also carry `id="..."` to make it clickable — see kn9t.on_click
-- below. Sizing: size = {fixed=N} | {percent=N} | {flex=N}
--
-- Styling: fg/bg accept "red", "lightred"/"brightred", "#rgb", "#rrggbb",
-- a 256-palette index ("33"), or "reset". Modifiers: bold, italic, underline,
-- dim, reverse. kn9t.theme.<slot> gives the configured palette as such a string.
--
-- Hooks Lua may define (all optional; absent means built-in behaviour):
--   render_ui(width, height) -> widget      the whole screen
--   render_status()          -> spans       the status line
--   tool_mode(name)          -> "diff"|"output"|"summary"|"streaming"
--
-- Mouse:
--   kn9t.on_click(id, function(local_x, local_y, button) ... end)
--     Fires when the widget carrying that `id=` is clicked. Coordinates are
--     local to the widget's own rect. Return false to let Rust's built-in
--     click handling (tool cards, diff viewer) still run; anything else
--     consumes the click. kn9t.remove_click(id) unbinds it.
--
-- Commands (palette + slash), additive to the built-ins:
--   kn9t.register_command({
--       id = "...", label = "...", description = "...", category = "...",
--       slash = "/name",              -- optional: also reachable as /name
--       handler = function(args) ... end,
--   })
--   kn9t.unregister_command(id)
--
-- Actions for kn9t.map(key, function() kn9t.action("...") end) or from inside
-- an on_click/register_command handler:
--   scroll_up/down/top/bottom, prev_message, next_message,
--   prev_user_message, next_user_message, session_picker, new_session,
--   switch_session (takes an id: kn9t.action("switch_session", id)),
--   open_diff (runs `git diff` and populates the viewer — pair this with
--     placing {type="native", view="diff"} in your layout; the native view
--     only draws an already-open viewer, it does not open one). While a
--     viewer is open, Rust routes every key into it FIRST (see app.rs
--     handle_key), so any Lua "close" binding must call
--     kn9t.action("diff_close") — that's the only thing that actually clears
--     App.diff_viewer. Flipping your own MAIN_VIEW-style state alone does
--     nothing on the Rust side and every key stays trapped in the viewer,
--   open_models, open_tools, open_palette, refresh_tools, search,
--   toggle_thinking, tool_mode, abort, quit, compact-free editing actions,
--   cycle_model_next/prev, and the diff viewer:
--   diff_next_hunk, diff_prev_hunk, diff_next_file, diff_prev_file,
--   diff_cursor_up, diff_cursor_down, diff_toggle_split,
--   diff_toggle_fullscreen, diff_toggle_tree, diff_comment, diff_close
--
-- Performance: render_ui()'s result is cached and only rebuilt when something
-- Rust can see changed (message/tool counts, scroll, streaming, cost, size).
-- A Lua-local toggle (a variable only this file knows about) is invisible to
-- that check — call kn9t.invalidate() after changing one, or the next redraw
-- may reuse last frame's tree.

-- ── Config ──────────────────────────────────────────────────────────────────
local SIDEBAR_WIDTH   = 34
local SIDEBAR_VISIBLE = true
local SIDEBAR_MIN_W   = 90     -- auto-hide below this terminal width

-- Usable context before compaction. Comes from the model the server reports
-- (kn9t.context.ctx_window); the fallback only applies when the provider does
-- not advertise a window.
local CONTEXT_FALLBACK = 200000

local function context_window()
    return (kn9t.context and kn9t.context.ctx_window) or CONTEXT_FALLBACK
end
local WARN_AT         = 0.75   -- amber past this fraction
local DANGER_AT       = 0.90   -- red past this fraction

-- ── Palette ─────────────────────────────────────────────────────────────────
-- Defaults come from the configured theme (kn9t.theme), so editing
-- [theme.colors] in config.toml moves the whole UI. Override a slot here only
-- to deviate from the theme on purpose.
local T = kn9t.theme or {}
local C = {
    dim     = T.muted     or "darkgray",
    label   = "gray",
    value   = T.fg        or "white",
    accent  = T.primary   or "cyan",
    ok      = T.success   or "lightgreen",
    warn    = T.warning   or "yellow",
    danger  = T.error     or "lightred",
    user    = T.user      or "cyan",
    asst    = T.assistant or "green",
    tool    = T.tool      or "yellow",
    system  = "magenta",
}

-- ── Helpers ─────────────────────────────────────────────────────────────────

local function fmt_tokens(n)
    n = n or 0
    if n >= 1000000 then return string.format("%.2fM", n / 1000000) end
    if n >= 1000 then return string.format("%.1fk", n / 1000) end
    return tostring(n)
end

local function context_color(frac)
    if frac >= DANGER_AT then return C.danger end
    if frac >= WARN_AT then return C.warn end
    return C.ok
end

-- Rows of "label  value", padded so values line up.
local function kv_lines(rows, width)
    local out = {}
    for _, r in ipairs(rows) do
        local label, value = r[1], r[2]
        local pad = width - #label - #value
        if pad < 1 then pad = 1 end
        table.insert(out, label .. string.rep(" ", pad) .. value)
    end
    return table.concat(out, "\n")
end

-- Live context size = the last prompt actually sent (fresh input + cache reads).
-- Session totals are cumulative and would only ever grow, so they cannot stand
-- in for how full the window is right now.
local function live_context()
    local ctx   = kn9t.context or {}
    local usage = kn9t.state and kn9t.state.usage or {}
    local turn  = usage.turn or {}

    local live = (turn.input or 0) + (turn.cache_read or 0)
    if live == 0 then
        live = (ctx.tokens_in or 0) + (ctx.cache_read or 0)
    end
    return live, live / context_window()
end

-- ── Sidebar sections ────────────────────────────────────────────────────────

-- Context budget: the number that actually matters before compaction.
local function section_context(inner_w)
    local live, frac = live_context()
    local left = math.max(0, context_window() - live)
    local col  = context_color(frac)

    return {
        type = "split",
        direction = "vertical",
        size = { fixed = 4 },
        children = {
            {
                type = "text",
                size = { fixed = 1 },
                content = kv_lines({
                    { "context", string.format("%d%%", math.floor(frac * 100)) },
                }, inner_w),
                fg = col,
            },
            {
                type = "gauge",
                size = { fixed = 1 },
                frac = frac,
                fg = col,
            },
            {
                type = "text",
                size = { fixed = 1 },
                content = kv_lines({
                    { "used", fmt_tokens(live) },
                }, inner_w),
                fg = C.label,
            },
            {
                type = "text",
                size = { fixed = 1 },
                content = kv_lines({
                    { "left", fmt_tokens(left) },
                }, inner_w),
                fg = C.label,
            },
        },
    }
end

local function section_usage(inner_w)
    local usage = kn9t.state and kn9t.state.usage or {}
    local turn  = usage.turn or {}
    local total = usage.total or {}

    local cache_read = total.cache_read or 0
    local input      = total.input or 0
    local hit = 0
    if (input + cache_read) > 0 then
        hit = math.floor(cache_read / (input + cache_read) * 100)
    end

    local rows = {
        { "turn in",  fmt_tokens(turn.input) },
        { "turn out", fmt_tokens(turn.output) },
        { "total in", fmt_tokens(input) },
        { "cache",    hit .. "%" },
    }
    if (usage.toks_per_sec or 0) > 0 then
        table.insert(rows, { "speed", string.format("%.0f t/s", usage.toks_per_sec) })
    end

    return {
        type = "box",
        title = " usage ",
        border = true,
        size = { fixed = #rows + 2 },
        child = { type = "text", content = kv_lines(rows, inner_w - 2), fg = C.value },
    }
end

-- Message mix: where the context is actually going.
local function section_transcript(inner_w)
    local ctx = kn9t.context or {}
    local rows = {
        { "messages",  tostring((kn9t.state and kn9t.state.message_count) or 0) },
        { "user",      tostring(ctx.user_count or 0) },
        { "assistant", tostring(ctx.assistant_count or 0) },
        { "tools",     tostring(ctx.tool_count or 0) },
    }
    return {
        type = "box",
        title = " transcript ",
        border = true,
        size = { fixed = #rows + 2 },
        child = { type = "text", content = kv_lines(rows, inner_w - 2), fg = C.value },
    }
end

-- Most recent tool calls, newest first. Rust pre-computes this (bounded), so
-- the sidebar costs nothing even in a very long session.
local function section_recent_tools(inner_w, max_rows)
    local tools = kn9t.state and kn9t.state.recent_tools or {}
    local recent = {}

    for i = 1, math.min(#tools, max_rows) do
        local t = tools[i]
        local mark = "."
        if t.status == "running" then mark = ">"
        elseif t.status == "done" then mark = "+"
        elseif t.status == "error" then mark = "!"
        end
        local name = t.name or "?"
        if #name > inner_w - 4 then name = name:sub(1, inner_w - 5) .. "~" end
        table.insert(recent, mark .. " " .. name)
    end

    local content = #recent > 0 and table.concat(recent, "\n") or "(none yet)"
    return {
        type = "box",
        title = " recent calls ",
        border = true,
        size = { flex = 1 },
        child = {
            type = "text",
            content = content,
            fg = #recent > 0 and C.value or C.dim,
            wrap = false,
        },
    }
end

local function build_sidebar(height)
    local ctx     = kn9t.context or {}
    local session = kn9t.state and kn9t.state.session or {}
    local inner_w = SIDEBAR_WIDTH - 2

    local title = session.title
    if title == nil or title == "" then title = "untitled" end
    if #title > inner_w then title = title:sub(1, inner_w - 1) .. "~" end

    local sid = session.id or ""
    if #sid > 8 then sid = sid:sub(1, 8) end

    -- Build the fixed sections first, then derive how many rows are left for
    -- the flexible one. Summing declared sizes keeps this honest when a
    -- section changes shape, instead of hand-copied constants that drift.
    local head = {
        { type = "text", content = title, fg = C.accent, size = { fixed = 1 } },
        {
            type = "text",
            size = { fixed = 1 },
            content = kv_lines({
                { sid ~= "" and ("#" .. sid) or "no session",
                  session.streaming and "streaming"
                    or (session.aborting and "aborting"
                    or (ctx.phase or "idle")) },
            }, inner_w),
            fg = C.dim,
        },
        section_context(inner_w),
        section_usage(inner_w),
        section_transcript(inner_w),
    }
    local foot = {
        {
            type = "text",
            size = { fixed = 1 },
            content = kv_lines({
                { "cost", string.format("$%.4f", (kn9t.state and kn9t.state.usage
                    and kn9t.state.usage.cost) or 0) },
            }, inner_w),
            fg = C.warn,
        },
    }

    local used = 0
    for _, s in ipairs(head) do used = used + (s.size and s.size.fixed or 0) end
    for _, s in ipairs(foot) do used = used + (s.size and s.size.fixed or 0) end

    -- -2 for this box's own border rows.
    local slots = math.max(1, height - 2 - used)

    local children = {}
    for _, s in ipairs(head) do table.insert(children, s) end
    table.insert(children, section_recent_tools(inner_w, slots))
    for _, s in ipairs(foot) do table.insert(children, s) end

    return {
        type = "box",
        title = " " .. (ctx.model or "kn9t") .. " ",
        border = true,
        child = {
            type = "split",
            direction = "vertical",
            children = children,
        },
    }
end


-- ── Layout ──────────────────────────────────────────────────────────────────

-- Plugin-supplied views.
--
-- A plugin that wants to draw ships Lua defining render(state). This decides
-- only *where* the result goes, so a plugin cannot pick its own placement or
-- push the transcript off screen.
--
-- Nothing here is per-plugin: the list comes from kn9t.state.plugin_views, so
-- a newly registered plugin appears without editing this file.
PLUGIN_VIEW_ROWS = 8
PLUGIN_COL_WIDTH = 34

local function plugin_view_children(width)
    local names = (kn9t.state and kn9t.state.plugin_views) or {}
    local out = {}
    for _, name in ipairs(names) do
        table.insert(out, {
            type = "box",
            title = " " .. name .. " ",
            border = true,
            size = { fixed = PLUGIN_VIEW_ROWS },
            child = { type = "plugin", plugin = name },
        })
    end
    return out
end


function render_ui(width, height)
    local ctx = kn9t.context or {}

    local main = {
        type = "split",
        direction = "vertical",
        children = {
            { type = "native", view = "transcript", size = { flex = 1 } },
            { type = "native", view = "input", size = { fixed = (ctx.input_height or 1) + 2 } },
            { type = "native", view = "status", size = { fixed = 1 } },
        },
    }

    -- Plugin views get their own column so they are independent of the info
    -- panel: hiding one must not hide the other.
    local plugins = plugin_view_children(width)
    local columns = { main }

    if #plugins > 0 and width >= SIDEBAR_MIN_W then
        table.insert(columns, {
            type = "split",
            direction = "vertical",
            size = { fixed = PLUGIN_COL_WIDTH },
            children = plugins,
        })
    end

    if SIDEBAR_VISIBLE and width >= SIDEBAR_MIN_W then
        table.insert(columns, {
            type = "box",
            border = false,
            size = { fixed = SIDEBAR_WIDTH },
            child = build_sidebar(height),
        })
    end

    if #columns == 1 then
        return main
    end

    main.size = { flex = 1 }
    return {
        type = "split",
        direction = "horizontal",
        children = columns,
    }
end

-- ── Status bar ──────────────────────────────────────────────────────────────

function render_status()
    local ctx     = kn9t.context or {}
    local session = kn9t.state and kn9t.state.session or {}
    local usage   = kn9t.state and kn9t.state.usage or {}
    local turn    = usage.turn or {}

    local seg = {}
    local function put(text, color) table.insert(seg, { text = text, color = color }) end

    -- Message mix across the context.
    local s = ctx.system_count or 0
    local u = ctx.user_count or 0
    local a = ctx.assistant_count or 0
    local t = ctx.tool_count or 0
    local total = s + u + a + t
    local bar = 18

    put("[", C.dim)
    if total > 0 then
        local sl = math.floor(s / total * bar + 0.5)
        local ul = math.floor(u / total * bar + 0.5)
        local al = math.floor(a / total * bar + 0.5)
        local tl = math.max(0, bar - sl - ul - al)
        if sl > 0 then put(string.rep("#", sl), C.system) end
        if ul > 0 then put(string.rep("#", ul), C.user) end
        if al > 0 then put(string.rep("#", al), C.asst) end
        if tl > 0 then put(string.rep("#", tl), C.tool) end
    else
        put(string.rep(".", bar), C.dim)
    end
    put("]", C.dim)

    -- Context pressure, the thing worth watching.
    local live = (turn.input or 0) + (turn.cache_read or 0)
    if live == 0 then live = (ctx.tokens_in or 0) + (ctx.cache_read or 0) end
    local frac = live / context_window()
    put(" ctx ", C.label)
    put(string.format("%d%%", math.floor(frac * 100)), context_color(frac))

    put("  ", nil)
    put(fmt_tokens(live), C.value)
    put("/", C.dim)
    put(fmt_tokens(context_window()), C.dim)

    -- Cost.
    put("  $", C.dim)
    put(string.format("%.4f", usage.cost or 0), C.warn)

    -- Live state.
    put("  ", nil)
    if session.streaming then
        put("streaming", C.ok)
        if (usage.toks_per_sec or 0) > 0 then
            put(string.format(" %.0ft/s", usage.toks_per_sec), C.dim)
        end
    elseif session.aborting then
        put("aborting", C.danger)
    else
        put(ctx.phase or "idle", C.dim)
    end

    return seg
end

-- ── Keymaps ─────────────────────────────────────────────────────────────────

-- Tool cards
-- ----------
-- How each tool's card renders. Returning nil falls back to the built-in
-- mapping, so only the tools you care about need an entry. This is the hook
-- that lets a plugin tool pick a renderer: the choice used to be hardcoded in
-- Rust, so only the four built-in tools could ever be styled.
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

kn9t.map("F5", function() SIDEBAR_VISIBLE = not SIDEBAR_VISIBLE end)
kn9t.map("F7", function() kn9t.action("scroll_top") end)
kn9t.map("F8", function() kn9t.action("scroll_bottom") end)

print("Lua UI loaded")
