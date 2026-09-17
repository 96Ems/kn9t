-- 90_render.lua — assembles the layout from earlier files.
--
-- ── The widget vocabulary (what `render_ui` may return) ─────────────────────
--
--   {type="native", view=...}   one of kn9t.native_views:
--                               "transcript" | "input" | "status" | "explorer" | "viewer"
--                               (the explorer column and the read-only file viewer; their
--                                visibility is kn9t.state.explorer_visible / .viewer_open)
--   {type="text", content=... | spans={{text=,fg=,bg=,bold=},...},
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
--   {type="input", id=...}      a text field whose editing Rust owns
--
-- Any widget may also carry `id="..."` to make it clickable - see kn9t.on_click.
-- Sizing: size = {fixed=N} | {percent=N} | {flex=N}
--
-- Styling: fg/bg accept "red", "lightred"/"brightred", "#rgb", "#rrggbb", a 256-palette
-- index ("33"), or "reset". Modifiers: bold, italic, underline, dim, reverse.
-- kn9t.theme.<slot> gives the configured palette as such a string. There is one colour
-- parser (theme::parse_color) behind config.toml, every widget colour and kn9t.theme, so a
-- colour that works in one works in all of them.
--
-- ── Hooks a config may define (all optional) ────────────────────────────────
--
--   render_ui(width, height) -> widget      the whole screen
--   render_status()          -> spans       the status line
--   tool_mode(name)          -> "diff"|"output"|"summary"|"streaming"
--
-- ── Actions, for kn9t.map / kn9t.on_click / a command handler ───────────────
--
--   scroll_up/down/top/bottom, prev_message, next_message, prev_user_message,
--   next_user_message, session_picker, new_session, switch_session (takes an id:
--   kn9t.action("switch_session", id)), open_models, open_tools, open_palette,
--   refresh_tools, search, toggle_thinking, tool_mode, abort, quit,
--   cycle_model_next/prev,
--   focus_plugin (takes a plugin name; with no argument it releases focus). A focused
--     plugin view receives keys it bound via kn9t.on_key BEFORE the host sees them, so a
--     panel can own j/k while focused; Esc always releases.
--
-- ── Commands, additive to the palette and the slash menu ────────────────────
--
--   kn9t.register_command({id=, label=, description=, category=,
--                          slash="/name", handler=function(args) ... end})
--   kn9t.unregister_command(id)
--
-- ── Mouse ───────────────────────────────────────────────────────────────────
--
--   kn9t.on_click(id, function(local_x, local_y, button) ... end) — fires when the widget
--   carrying that `id=` is clicked; return false to let Rust's own click handling run.
--   kn9t.remove_click(id) unbinds it.
--
-- ── Performance ─────────────────────────────────────────────────────────────
-- `render_ui`'s result is cached and rebuilt only when something Rust can see changed
-- (message/tool counts, scroll, streaming, cost, size). A Lua-local flag is invisible to
-- that check — call kn9t.invalidate() after changing one, or the next redraw may reuse
-- last frame's tree.

local SIDEBAR_MIN_W = TUI.SIDEBAR_MIN_W or 110
local EXPLORER_W    = TUI.EXPLORER_WIDTH or 32
local RIGHT_WIDTH   = TUI.SIDEBAR_WIDTH or 34
local PLUGIN_COL_W  = TUI.PLUGIN_COL_WIDTH or 34

-- Build plugin views for a given placement zone.
-- For the "main" zone: only shown when focused, unless TUI.show.main_plugins is set.
local function plugin_views_in(zone)
    local specs = (kn9t.state and kn9t.state.plugin_view_specs) or {}
    local focused = (kn9t.state and kn9t.state.focused_plugin) or ""
    local out = {}
    for _, spec in ipairs(specs) do
        local placed = (spec.placement ~= "" and spec.placement) or "sidebar"
        if placed == zone then
            local is_focused = (spec.name == focused)

            if zone == "main" and not is_focused and not TUI.show.main_plugins then
                -- skip: hidden until focused
            else
                local title = (spec.title ~= "" and spec.title) or spec.name
                local rows = TUI.PLUGIN_VIEW_ROWS or 8
                if is_focused then
                    rows = (spec.rows > 0 and spec.rows) or (TUI.PLUGIN_VIEW_ROWS_FOCUSED or 24)
                end
                local box_title
                if is_focused then
                    -- The plugin shows its own help bar; just say how to release.
                    box_title = " " .. title .. " [Esc] release "
                else
                    box_title = " [F10] " .. title .. " "
                end
                table.insert(out, {
                    type = "box",
                    title = box_title,
                    border = "rounded",
                    -- The accent slot, not a literal: a focused panel must follow the theme.
                    border_fg = is_focused and TUI.color.accent or nil,
                    size = { fixed = rows },
                    child = { type = "plugin", plugin = spec.name },
                })
            end
        end
    end
    return out
end

-- The centre column: transcript, input, status — with panes stacked above the transcript: the
-- file viewer (D5) and any plugin view that declared placement="main". Stacking is what makes
-- a full-size review or diff panel possible without this file knowing which plugin provides it.
local function main_pane()
    local ctx = kn9t.context or {}
    local st = kn9t.state or {}

    local main = {
        type = "split",
        direction = "vertical",
        children = {
            { type = "native", view = "transcript", size = { flex = 1 } },
            { type = "native", view = "input", size = { fixed = (ctx.input_height or 1) + 2 } },
            { type = "native", view = "status", size = { fixed = 1 } },
        },
    }

    local top = {}
    if st.viewer_open then
        table.insert(top, {
            type = "box",
            border = false,
            size = { percent = TUI.VIEWER_PERCENT or 50 },
            child = { type = "native", view = "viewer" },
        })
    end
    for _, p in ipairs(plugin_views_in("main")) do
        table.insert(top, p)
    end

    if #top > 0 then
        local stacked = {
            size = { flex = 1 },
            type = "split",
            direction = "vertical",
            children = {},
        }
        for _, p in ipairs(top) do
            table.insert(stacked.children, p)
        end
        table.insert(stacked.children, main)
        main.size = { flex = 1 }
        main = stacked
    end

    return main
end

function render_ui(width, height)
    local narrow = width < SIDEBAR_MIN_W
    local st = kn9t.state or {}

    local main = main_pane()
    local plugins = plugin_views_in("sidebar")
    local columns = { main }

    -- The file explorer column (PLAN §P7 L2 / D3). There is no session column any more: the
    -- tab bar in the header is the session list (D2/D3), so listing sessions here too would be
    -- the same information twice. Visibility is Rust's (`kn9t.state.explorer_visible`, F1);
    -- this file only decides where the column goes.
    if st.explorer_visible and not narrow then
        table.insert(columns, 1, {
            type = "box",
            border = false,
            size = { fixed = EXPLORER_W },
            child = { type = "native", view = "explorer" },
        })
    end

    if #plugins > 0 and not narrow then
        table.insert(columns, {
            type = "split",
            direction = "vertical",
            size = { fixed = PLUGIN_COL_W },
            children = plugins,
        })
    end

    if TUI.show.right_sidebar and not narrow then
        table.insert(columns, {
            type = "box",
            border = false,
            size = { fixed = RIGHT_WIDTH },
            child = TUI.build_sidebar_right(RIGHT_WIDTH, height),
        })
    end

    if #columns == 1 then
        return {
            type = "split",
            direction = "vertical",
            children = { TUI.build_header(width), main },
        }
    end

    main.size = { flex = 1 }
    return {
        type = "split",
        direction = "vertical",
        children = {
            TUI.build_header(width),
            {
                type = "split",
                direction = "horizontal",
                size = { flex = 1 },
                children = columns,
            },
        },
    }
end

