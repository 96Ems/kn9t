-- 90_render.lua — assembles the layout from earlier files.

local SIDEBAR_MIN_W = TUI.SIDEBAR_MIN_W or 90
local LEFT_WIDTH    = TUI.LEFT_WIDTH or 32
local RIGHT_WIDTH   = TUI.SIDEBAR_WIDTH or 34
local PLUGIN_COL_W  = TUI.PLUGIN_COL_WIDTH or 34

-- Build plugin views for a given placement zone
-- For "main" zone: only show if focused OR TUI.show.main_plugins is true
local function plugin_views_in(zone)
    local specs = (kn9t.state and kn9t.state.plugin_view_specs) or {}
    local focused = (kn9t.state and kn9t.state.focused_plugin) or ""
    local out = {}
    for _, spec in ipairs(specs) do
        local placed = (spec.placement ~= "" and spec.placement) or "sidebar"
        if placed == zone then
            local is_focused = (spec.name == focused)
            
            -- Main plugins only appear when focused (unless show.main_plugins)
            if zone == "main" and not is_focused and not TUI.show.main_plugins then
                -- skip: hidden until focused
            else
                local title = (spec.title ~= "" and spec.title) or spec.name
                local rows = TUI.PLUGIN_VIEW_ROWS or 8
                if is_focused then
                    rows = (spec.rows > 0 and spec.rows) or (TUI.PLUGIN_VIEW_ROWS_FOCUSED or 24)
                end
                table.insert(out, {
                    type = "box",
                    title = is_focused and (" " .. title .. " - Esc to release ")
                                        or (" " .. title .. " "),
                    border = true,
                    border_fg = is_focused and "cyan" or nil,
                    size = { fixed = rows },
                    child = { type = "plugin", plugin = spec.name },
                })
            end
        end
    end
    return out
end

local function main_pane()
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

    -- Plugin views with placement="main" stack above the transcript
    local main_plugins = plugin_views_in("main")
    if #main_plugins > 0 then
        local stacked = {
            size = { flex = 1 },
            type = "split",
            direction = "vertical",
            children = {},
        }
        for _, p in ipairs(main_plugins) do
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

    local main = main_pane()
    local plugins = plugin_views_in("sidebar")
    local columns = { main }

    if TUI.show.left_sidebar and not narrow then
        table.insert(columns, 1, {
            type = "box",
            border = false,
            size = { fixed = LEFT_WIDTH },
            child = TUI.build_sidebar_left(height),
        })
    end

    -- Plugin sidebar column
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
