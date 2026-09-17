-- kn9t TUI - full UI defined in Lua
-- =================================
-- Rust renders; Lua decides layout, content and styling.
--
-- State, refreshed every frame (cheap - bounded size):
--   kn9t.state.session       {id, title, cwd, streaming, aborting, has_lease, last_seq}
--   kn9t.state.usage         {turn={input,output,cache_read,cache_write},
--                             total={...}, cost, toks_per_sec}
--   kn9t.state.recent_tools  [{name, status}, ...] newest first, capped
--   kn9t.state.sessions      [{id, name, is_current}, ...] all known sessions -
--                            already cached, no HTTP call; feed to a "list"
--                            widget and kn9t.action("switch_session", id)
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
-- Widgets:
--   {type="native", view=...}   one of kn9t.native_views:
--                               "transcript" | "input" | "status" | "welcome"
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
-- Any widget may also carry `id="..."` to make it clickable - see kn9t.on_click
-- below. Sizing: size = {fixed=N} | {percent=N} | {flex=N}
--
-- Styling: fg/bg accept "red", "lightred"/"brightred", "#rgb", "#rrggbb",
-- a 256-palette index ("33"), or "reset". Modifiers: bold, italic, underline,
-- dim, reverse. kn9t.theme.<slot> gives the configured palette as such a string.
--
-- Hooks Lua may define (all optional; absent means built-in behaviour):
--   render_ui(width, height) -> widget      the whole screen
--   render_status()          -> spans       the status line. A segment takes the same
--                                           fields as a text span (text, fg, bg, bold,
--                                           dim, reverse), so a segment can be a solid
--                                           colour block rather than only coloured text.
--   tool_mode(name)          -> "diff"|"output"|"summary"|"streaming"
--
-- Mouse:
--   kn9t.on_click(id, function(local_x, local_y, button) ... end)
--     Fires when the widget carrying that `id=` is clicked. Coordinates are
--     local to the widget's own rect. Return false to let Rust's built-in
--     click handling (tool cards) still run; anything else consumes the
--     click. kn9t.remove_click(id) unbinds it.
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
--   focus_plugin (takes a plugin name: kn9t.action("focus_plugin", name);
--     with no argument it releases focus). A focused plugin view receives keys
--     it bound via kn9t.on_key BEFORE the host sees them, so a panel can own
--     j/k while focused; Esc always releases. This is how an interactive
--     plugin panel - the kn9t-git-integration diff review, for instance - is
--     driven. Clicking a plugin view focuses it too.
--   open_models, open_tools, open_palette, refresh_tools, search,
--   toggle_thinking, tool_mode, abort, quit, compact-free editing actions,
--   cycle_model_next/prev
--
-- Diff review is NOT a native view: it ships as the kn9t-git-integration
-- plugin, which runs `git diff` itself and renders/handles input in its own
-- Lua. Place it like any plugin view ({type="plugin", plugin=...}) and focus
-- it to interact.
--
-- Performance: render_ui()'s result is cached and only rebuilt when something
-- Rust can see changed (message/tool counts, scroll, streaming, cost, size).
-- A Lua-local toggle (a variable only this file knows about) is invisible to
-- that check - call kn9t.invalidate() after changing one, or the next redraw
-- may reuse last frame's tree.

-- ── Config ──────────────────────────────────────────────────────────────────
-- Globals, not locals, so a user file layered on top (`~/.kn9t/tui/50_mine.lua`)
-- can override any of them without copying this whole file.
SIDEBAR_WIDTH   = 34
SIDEBAR_VISIBLE = true
SIDEBAR_MIN_W   = 90     -- auto-hide sidebars below this terminal width

-- Which view the main pane shows, displayed in the header. "chat" is the only
-- value this file produces; a plugin panel is a separate column, not a mode. A
-- user file may set it to anything and render accordingly.
MAIN_VIEW = "chat"

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
-- Every colour comes from the configured theme (kn9t.theme), so editing
-- [theme.colors] in config.toml moves the whole UI. Override a slot here only
-- to deviate from the theme on purpose.
--
-- Three colours carry meaning, the rest is chrome (PLAN P7 D16):
--   accent  selection, focus, active tab, headings, mentions
--   warn    attention: a running tool, cost, an approval, a truncation
--   danger  failure, abort, a full context window
-- `label` and `system` are deliberately chrome, not colours: labels are not
-- messages, and a system notice is not an alert.
local T = kn9t.theme or {}
local C = {
    dim     = T.muted     or "darkgray",
    label   = T.muted     or "gray",
    value   = T.fg        or "white",
    -- Text placed *on* a solid block (status chips, the active tab). Silver on
    -- violet is unreadable, so a block needs its own foreground.
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

    -- Whatever is left over belongs to the plugins, not to a list Rust already
    -- publishes elsewhere. This used to be a "recent calls" panel, which showed
    -- the same tool names the transcript's own cards show, one column over.
    local slots = math.max(1, height - 2 - used)

    local children = {}
    for _, s in ipairs(head) do table.insert(children, s) end
    table.insert(children, {
        type = "spacer",
        size = { fixed = slots },
    })
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
--
-- A focused view is given far more room and a highlighted border: an
-- interactive panel (a diff review, say) is unusable in a status-sized strip,
-- but permanently reserving that space would crowd out the transcript. Focus is
-- the signal for which of the two a view currently needs.
PLUGIN_VIEW_ROWS = 8
PLUGIN_VIEW_ROWS_FOCUSED = 24
PLUGIN_COL_WIDTH = 34

-- Build the boxes for plugin views in a given zone.
--
-- Routing is by the plugin's *declared* placement, never by its name: a plugin
-- says `placement="sidebar"` / `"main"` when it registers, and this decides what
-- that means. Installing a plugin therefore never requires editing this file,
-- and a plugin still cannot seize space - the zone it asks for is a request this
-- function is free to ignore.
--
-- `title` and `rows` come from the plugin too, with sane fallbacks: `title`
-- defaults to the plugin id (which reads poorly as a heading, hence the hint),
-- and `rows` to PLUGIN_VIEW_ROWS.
local function plugin_views_in(zone)
    local specs = (kn9t.state and kn9t.state.plugin_view_specs) or {}
    local focused = (kn9t.state and kn9t.state.focused_plugin) or ""
    local out = {}
    for _, spec in ipairs(specs) do
        -- An unplaced view defaults to the sidebar: somewhere visible beats
        -- nowhere, and the narrow column is the least disruptive default.
        local placed = (spec.placement ~= "" and spec.placement) or "sidebar"
        if placed == zone then
            local is_focused = (spec.name == focused)
            local title = (spec.title ~= "" and spec.title) or spec.name
            -- Focused panels get the room the plugin asked for: an interactive
            -- view is unusable in a status-sized strip, but permanently
            -- reserving that space would crowd out the transcript.
            local rows = PLUGIN_VIEW_ROWS
            if is_focused then
                rows = (spec.rows > 0 and spec.rows) or PLUGIN_VIEW_ROWS_FOCUSED
            end
            table.insert(out, {
                type = "box",
                -- Spell out how to leave: the panel's own keys are the plugin's,
                -- so nothing here can list them per-plugin.
                title = is_focused and (" " .. title .. " - Esc to release ")
                                    or (" " .. title .. " "),
                border = true,
                border_fg = is_focused and "cyan" or nil,
                size = { fixed = rows },
                child = { type = "plugin", plugin = spec.name },
            })
        end
    end
    return out
end

-- Focus the next plugin view, or release focus after the last one.
--
-- Cycling by index over kn9t.state.plugin_views keeps this free of plugin names:
-- whatever is installed is reachable, in the order the host reports. Clicking a
-- view focuses it too; this is the keyboard route, so a panel works without a
-- mouse. Esc always releases (handled by the host).
local function focus_cycle()
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

kn9t.map("C-g", focus_cycle)
kn9t.map("F10", focus_cycle)

-- The "+ new" button on the tab bar. A session is created by the same action
-- the Ctrl+N binding and `/new` use, so the button cannot drift from them.
kn9t.on_click("tab_new", function()
    kn9t.action("new_session")
    return true
end)


-- ── Header: session tabs + breadcrumb ────────────────────────────────────────
--
-- Two rows, VS Code's grammar (PLAN P7 D14):
--   row 1  the open sessions as tabs — the "editor group" of an agent
--   row 2  where you are: cwd ▸ session ▸ model, with an alert slot on the right
--
-- Tabs are the only session navigation in this file. The left column is the file
-- explorer (D3), so listing sessions there too would show the same thing twice.
-- The full list (with a filter) stays reachable from the command palette.
--
-- `ALERT` is a plain global any config file (or a later override) can set:
--   ALERT = { text = "reconnecting", fg = "yellow" }
-- Set it to nil to clear. Remember kn9t.invalidate() after changing it, since
-- the render cache cannot see a Lua-local variable.
ALERT = nil

-- How many tabs before the bar collapses the tail into a "+N" counter. Tabs are
-- the only session list on screen, so the cap is generous; past it the palette's
-- session picker is the way to reach a session.
TAB_MAX = 10
TAB_LABEL_MAX = 22

-- Registered once per session id, like the old sidebar rows: rebuilding the bar
-- every frame must not re-register the same handler.
local registered_tab_clicks = {}

local function tab_label(name)
    name = name or ""
    if name == "" then name = "untitled" end
    if #name > TAB_LABEL_MAX then name = name:sub(1, TAB_LABEL_MAX - 1) .. "…" end
    return name
end

local function tab_bar(width)
    local sessions = (kn9t.state and kn9t.state.sessions) or {}
    local current  = (kn9t.state and kn9t.state.session or {}).id or ""
    local session  = (kn9t.state and kn9t.state.session) or {}

    -- The current session leads, so switching tabs never reorders what you are
    -- looking at; the rest keep the order Rust reported (most recent first).
    local ordered = {}
    local rest = {}
    for _, s in ipairs(sessions) do
        if s.id == current then table.insert(ordered, 1, s) else table.insert(rest, s) end
    end
    for _, s in ipairs(rest) do table.insert(ordered, s) end

    local spans = {}
    local used = 0
    local shown = 0
    local hidden = 0

    -- Leave room for the "+ new" button on the right.
    local budget = math.max(8, width - 6 - 4)

    for _, s in ipairs(ordered) do
        local label = tab_label(s.name or s.id)
        local w = #label + 2
        if used + w > budget then
            hidden = hidden + 1
        else
            local id = "tab_" .. s.id
            if not registered_tab_clicks[id] then
                local sid = s.id
                kn9t.on_click(id, function() kn9t.action("switch_session", sid); return true end)
                registered_tab_clicks[id] = true
            end
            local active = (s.id == current)
            table.insert(spans, {
                text = " " .. label .. " ",
                fg = active and C.tab_active_fg or C.dim,
                bg = active and C.accent or nil,
                bold = active,
            })
            -- Separator between tabs only: a trailing one reads as a stray glyph.
            table.insert(spans, { text = " ", fg = C.dim })
            used = used + w
            shown = shown + 1
        end
    end

    if shown == 0 then
        table.insert(spans, { text = " no session ", fg = C.dim })
    elseif hidden > 0 then
        table.insert(spans, { text = " +" .. hidden .. " ", fg = C.dim })
    end

    -- The alert belongs on the busiest row, which is this one.
    if ALERT then
        table.insert(spans, { text = "  " .. ALERT.text, fg = ALERT.fg or C.danger, bold = true })
    end

    -- Running state is a property of the *session*, and only the active one can
    -- be live (one SSE stream), so the indicator goes on the bar rather than a tab.
    -- Not animated on purpose: the transcript's streaming line is Rust's and already
    -- spins, and a second spinner for the same fact is noise, not feedback.
    if session.streaming then
        table.insert(spans, { text = "  ● streaming", fg = C.ok })
    elseif session.aborting then
        table.insert(spans, { text = "  ● aborting", fg = C.danger, bold = true })
    end

    return {
        type = "split",
        direction = "horizontal",
        size = { fixed = 1 },
        children = {
            { type = "text", id = "tabbar", spans = spans, size = { flex = 1 }, wrap = false },
            {
                type = "text",
                id = "tab_new",
                spans = { { text = " + new ", fg = C.accent } },
                size = { fixed = 7 },
                align = "right",
                wrap = false,
            },
        },
    }
end

-- Breadcrumb: cwd ▸ title ▸ model. Each segment is a separate span so a long
-- path can be dimmed while the model stays legible.
local function build_breadcrumb(width)
    local ctx     = kn9t.context or {}
    local session = (kn9t.state and kn9t.state.session) or {}

    local cwd = session.cwd or ""
    if #cwd > 40 then cwd = "…" .. cwd:sub(#cwd - 39) end
    if cwd == "" then cwd = "(no cwd)" end

    local title = session.title
    if title == nil or title == "" then title = "untitled" end
    if #title > 30 then title = title:sub(1, 29) .. "…" end

    local left = {
        { text = " " .. cwd, fg = C.dim },
        { text = "  ▸  ", fg = C.dim },
        { text = title, fg = C.value },
        { text = "  ▸  ", fg = C.dim },
        { text = ctx.model or "no model", fg = C.accent },
    }

    local phase = session.streaming and "streaming"
        or (session.aborting and "aborting" or (ctx.phase or "idle"))
    local right = { { text = phase .. " ", fg = session.aborting and C.danger or C.dim } }

    return {
        type = "box",
        border = false,
        size = { fixed = 1 },
        child = {
            type = "split",
            direction = "horizontal",
            children = {
                { type = "text", spans = left, size = { flex = 1 }, wrap = false },
                { type = "text", spans = right, size = { fixed = #phase + 1 }, align = "right", wrap = false },
            },
        },
    }
end

local function build_header(width)
    return {
        type = "split",
        direction = "vertical",
        size = { fixed = 2 },
        children = { tab_bar(width), build_breadcrumb(width) },
    }
end


-- ── Left column: the file explorer ──────────────────────────────────────────
--
-- Reserved for PLAN P7 L2. It is deliberately absent rather than filled with a
-- placeholder: sessions are the tab bar now (D2/D3), so a session list here would
-- be the same information twice, and an empty frame would look like a bug.
--
-- L2 adds `build_explorer()` (fed by a Rust file index) and the column below,
-- at EXPLORER_WIDTH. The widths are declared here now so the layout maths does
-- not have to move when it lands.
EXPLORER_VISIBLE = false
EXPLORER_WIDTH   = 32


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
    -- A plugin that declared placement="main" takes over the centre column,
    -- stacked above the transcript. This is what makes a full-size review panel
    -- possible without this file knowing which plugin provides it.
    local main_plugins = plugin_views_in("main")
    if #main_plugins > 0 then
        local stacked = { size = { flex = 1 }, type = "split", direction = "vertical",
                          children = {} }
        for _, p in ipairs(main_plugins) do
            table.insert(stacked.children, p)
        end
        table.insert(stacked.children, main)
        main.size = { flex = 1 }
        main = stacked
    end

    local plugins = plugin_views_in("sidebar")
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
        return {
            type = "split",
            direction = "vertical",
            children = { build_header(width), main },
        }
    end

    main.size = { flex = 1 }
    return {
        type = "split",
        direction = "vertical",
        children = {
            build_header(width),
            {
                type = "split",
                direction = "horizontal",
                size = { flex = 1 },
                children = columns,
            },
        },
    }
end

-- ── Status bar ──────────────────────────────────────────────────────────────
--
-- A segmented bar, VS Code's grammar (PLAN P7 D13): solid blocks of state read
-- left to right, with the contextual key hints on the right. Three colours carry
-- meaning — accent for "where we are", amber for "watch this", danger for "this
-- is wrong" — and the rest is chrome, so the eye lands on what changed.
--
-- A segment is `{text=, fg=, bg=, bold=, dim=, reverse=}`; `bg` is what makes a
-- block rather than coloured text.

function render_status()
    local ctx     = kn9t.context or {}
    local session = kn9t.state and kn9t.state.session or {}
    local usage   = kn9t.state and kn9t.state.usage or {}
    local turn    = usage.turn or {}

    local seg = {}
    local function block(text, fg, bg, opts)
        opts = opts or {}
        table.insert(seg, {
            text = text, fg = fg, bg = bg,
            bold = opts.bold, dim = opts.dim,
        })
    end
    -- Chrome text: no background, reads as a label rather than a value.
    local function text(s, fg) table.insert(seg, { text = s, fg = fg or C.dim }) end

    -- ── Left: what is running and where ─────────────────────────────────────
    local phase, phase_col
    if session.streaming then
        phase, phase_col = "streaming", C.ok
    elseif session.aborting then
        phase, phase_col = "aborting", C.danger
    else
        phase, phase_col = (ctx.phase or "idle"), C.dim
    end
    block(" " .. phase .. " ", C.ink, phase_col, { bold = true })

    text(" " .. (ctx.model or "no model") .. " ", C.value)

    -- ── Context pressure: the number that decides when compaction bites ─────
    local live = (turn.input or 0) + (turn.cache_read or 0)
    if live == 0 then live = (ctx.tokens_in or 0) + (ctx.cache_read or 0) end
    local frac = live / context_window()
    local pct  = math.floor(frac * 100)
    text("  ctx ")
    block(" " .. pct .. "% ", C.ink, context_color(frac), { bold = true })
    text(" " .. fmt_tokens(live) .. "/" .. fmt_tokens(context_window()))

    -- ── Cost ────────────────────────────────────────────────────────────────
    text("   $")
    text(string.format("%.4f", usage.cost or 0), C.warn)

    -- ── Throughput, only while it means something ───────────────────────────
    if (usage.toks_per_sec or 0) > 0 then
        text(string.format("  %.0f t/s", usage.toks_per_sec))
    end

    -- ── Message mix: a compact glyph strip rather than a 18-cell bar.
    -- The bar was the loudest thing on the line and answered a question nobody
    -- asks mid-turn; four numbers do the same job in a third of the width.
    text("   ")
    text(tostring(ctx.system_count or 0), C.system)
    text("/")
    text(tostring(ctx.user_count or 0), C.user)
    text("/")
    text(tostring(ctx.assistant_count or 0), C.asst)
    text("/")
    text(tostring(ctx.tool_count or 0), C.tool)

    -- ── Right: what you can do from here ───────────────────────────────────
    local focused = (kn9t.state and kn9t.state.focused_plugin) or ""
    text("    ")
    if focused ~= "" then
        text("Esc", C.accent)
        text(" release " .. focused, C.value)
    else
        text("C-p", C.accent)
        text(" commands", C.value)
        text("   F2", C.accent)
        text(" panels", C.value)
        if #((kn9t.state and kn9t.state.plugin_views) or {}) > 0 then
            text("   F10", C.accent)
            text(" plugins", C.value)
        end
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

-- Panel toggles. Each calls kn9t.invalidate(): the render cache is fingerprinted
-- from state Rust can see, and a Lua-local flag is not part of that, so without it
-- the next redraw reuses the stale tree.
--
-- F1 is free: it used to toggle the session column, which the tab bar replaced.
-- L2 binds it to the file explorer.
kn9t.map("F2", function()
    SIDEBAR_VISIBLE = not SIDEBAR_VISIBLE
    kn9t.invalidate()
end)
-- Kept as an alias for the right sidebar: F5 was this file's original toggle.
kn9t.map("F5", function()
    SIDEBAR_VISIBLE = not SIDEBAR_VISIBLE
    kn9t.invalidate()
end)
kn9t.map("F7", function() kn9t.action("scroll_top") end)
kn9t.map("F8", function() kn9t.action("scroll_bottom") end)

print("Lua UI loaded")