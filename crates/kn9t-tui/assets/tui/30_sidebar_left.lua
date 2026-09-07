-- 30_sidebar_left.lua — session list sidebar.
--
-- Generic: no plugin dependencies. Shows only sessions.

local C = TUI.color

local function session_row_click(id)
    return function(x, y, btn)
        kn9t.action("switch_session", id)
        return true
    end
end

local registered_session_clicks = {}

local function session_list(inner_w, height)
    local sessions = (kn9t.state and kn9t.state.sessions) or {}
    if #sessions == 0 then
        return {
            type = "text",
            content = "(no sessions yet)",
            fg = C.dim,
            size = { flex = 1 },
        }
    end

    local items = {}
    for i, s in ipairs(sessions) do
        local id = "session_row_" .. s.id
        if not registered_session_clicks[id] then
            kn9t.on_click(id, session_row_click(s.id))
            registered_session_clicks[id] = true
        end
        local name = s.name or s.id
        if #name > inner_w - 2 then name = name:sub(1, inner_w - 3) .. "~" end
        table.insert(items, {
            id = id,
            type = "text",
            content = (s.is_current and "> " or "  ") .. name,
            fg = s.is_current and C.accent or C.value,
        })
    end

    return {
        type = "split",
        direction = "vertical",
        size = { flex = 1 },
        children = items,
    }
end

function TUI.build_sidebar_left(height)
    local inner_w = 32

    return {
        type = "box",
        border = "plain",
        border_fg = C.dim,
        title = " [F1] Sessions ",
        child = session_list(inner_w, height),
    }
end
