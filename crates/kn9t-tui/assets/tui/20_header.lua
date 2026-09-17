-- 20_header.lua — session tabs over a breadcrumb (PLAN §P7 D14).
--
-- Two rows, VS Code's grammar:
--   row 1  the open sessions as tabs — the "editor group" of an agent
--   row 2  where you are: cwd ▸ session ▸ model, with the phase on the right
--
-- Tabs are the only session navigation in the built-in UI: the left column is the file
-- explorer (D3), so listing sessions there as well would show the same thing twice. The
-- full list, with a filter, stays reachable from the command palette.

local C = TUI.color

-- Registered once per session id: rebuilding the bar every frame must not re-register the
-- same click handler. `kn9t.on_click` replaces by id anyway; this just avoids the churn.
local registered_tab_clicks = {}

local function tab_label(name)
    name = name or ""
    if name == "" then name = "untitled" end
    if #name > TUI.TAB_LABEL_MAX then name = name:sub(1, TUI.TAB_LABEL_MAX - 1) .. "…" end
    return name
end

-- Row 1: the tabs, plus a "+ new" button on the right.
local function tab_bar(width)
    local sessions = (kn9t.state and kn9t.state.sessions) or {}
    local session  = (kn9t.state and kn9t.state.session) or {}
    local current  = session.id or ""

    -- The current session leads, so switching tabs never reorders what you are looking at;
    -- the rest keep the order the host reported (most recent first).
    local ordered, rest = {}, {}
    for _, s in ipairs(sessions) do
        if s.id == current then table.insert(ordered, 1, s) else table.insert(rest, s) end
    end
    for _, s in ipairs(rest) do table.insert(ordered, s) end

    local spans, used, shown, hidden = {}, 0, 0, 0
    -- Leave room for the "+ new" button and the running indicator on the right.
    local budget = math.max(8, width - 26)

    for _, s in ipairs(ordered) do
        if shown >= TUI.TAB_MAX then
            hidden = hidden + 1
        else
            local label = tab_label(s.name or s.id)
            local w = #label + 2
            if used + w > budget then
                hidden = hidden + 1
            else
                local id = "tab_" .. s.id
                if not registered_tab_clicks[id] then
                    local sid = s.id
                    kn9t.on_click(id, function()
                        kn9t.action("switch_session", sid)
                        return true
                    end)
                    registered_tab_clicks[id] = true
                end
                local active = (s.id == current)
                table.insert(spans, {
                    text = " " .. label .. " ",
                    fg = active and C.ink or C.dim,
                    bg = active and C.accent or nil,
                    bold = active,
                })
                -- Separator between tabs only: a trailing one reads as a stray glyph.
                table.insert(spans, { text = " ", fg = C.dim })
                used = used + w
                shown = shown + 1
            end
        end
    end

    if shown == 0 then
        table.insert(spans, { text = " no session ", fg = C.dim })
    elseif hidden > 0 then
        table.insert(spans, { text = " +" .. hidden .. " ", fg = C.dim })
    end

    -- The alert belongs on the busiest row.
    if TUI.alert then
        table.insert(spans, { text = "  " .. TUI.alert.text, fg = TUI.alert.fg or C.danger, bold = true })
    end

    -- Running state is a property of the *session*, and only the active one can be live
    -- (one SSE stream), so the indicator goes on the bar rather than on a tab. Not
    -- animated on purpose: the transcript's streaming line already spins, and a second
    -- spinner for the same fact is noise.
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

-- Row 2: cwd ▸ title ▸ model, with the phase on the right. Each part is its own span so a
-- long path can be dimmed while the model stays legible.
local function breadcrumb(width)
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
        type = "split",
        direction = "horizontal",
        size = { fixed = 1 },
        children = {
            { type = "text", spans = left, size = { flex = 1 }, wrap = false },
            {
                type = "text",
                spans = right,
                size = { fixed = #phase + 1 },
                align = "right",
                wrap = false,
            },
        },
    }
end

-- The header block: two rows, as one node so `render_ui` stays a three-line assembly.
function TUI.build_header(width)
    return {
        type = "split",
        direction = "vertical",
        size = { fixed = 2 },
        children = { tab_bar(width), breadcrumb(width) },
    }
end
