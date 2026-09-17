-- 20_header.lua — session tabs over a breadcrumb (PLAN §P7 D14).
--
-- Two rows, VS Code's grammar:
--   row 1  the open sessions as tabs — the "editor group" of an agent
--   row 2  where you are: cwd ▸ title, with the phase on the right
--
-- Tabs are the only session navigation in the built-in UI: the left column is the file
-- explorer (D3), so listing sessions there as well would show the same thing twice. The
-- full list, with a filter, stays reachable from the command palette.
--
-- **Every tab is its own widget.** A click target needs a rectangle, and only a widget has
-- one: `kn9t.on_click` matches against the rect `collect_clickable_areas` recorded for a
-- widget carrying `id=`, so a tab drawn as one more span inside a single text node is
-- unclickable no matter how the handler is registered. This file used to emit exactly that
-- — `on_click("tab_<id>")` handlers bound to ids no widget ever carried.
--
-- The model is deliberately absent from the breadcrumb: it is the prompt frame's title
-- (D12), which is the row you act in. Showing it here and in the status bar and in the
-- sidebar title meant the same string on every line of chrome at once.

local C = TUI.color

-- Registered once per session id: rebuilding the bar every frame must not re-register the
-- same click handler. `kn9t.on_click` replaces by id anyway; this just avoids the churn.
local registered_tab_clicks = {}

-- Display width in cells, counting UTF-8 codepoints.
--
-- `#s` counts **bytes**, and a tab is a widget with a fixed width: a label containing `…`
-- (three bytes, one cell) would size its widget two cells too wide and leave stray cells
-- beside the accent block. Not `utf8.len` either — the sandbox may not carry the `utf8`
-- library, and this only needs to be right for the one multibyte character it can produce.
local function cells(s)
    local n = 0
    for i = 1, #s do
        local b = s:byte(i)
        if b < 0x80 or b >= 0xC0 then n = n + 1 end   -- lead byte, not a continuation
    end
    return n
end

-- Truncate to `max` cells, cutting on a codepoint boundary. `string.sub` counts bytes, so a
-- naive `:sub(1, n)` splits a multi-byte character and the terminal renders garbage.
local function truncate(s, max)
    if cells(s) <= max then return s end
    local n, i = 0, 1
    while i <= #s and n < max - 1 do
        local b = s:byte(i)
        i = i + ((b < 0x80 and 1) or (b < 0xE0 and 2) or (b < 0xF0 and 3) or 4)
        n = n + 1
    end
    return s:sub(1, i - 1) .. "…"
end

local function tab_label(name)
    name = name or ""
    if name == "" then name = "untitled" end
    return truncate(name, TUI.TAB_LABEL_MAX)
end

-- Row 1: the tabs, plus a "+ new" button pinned right.
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

    local children, used, shown, hidden = {}, 0, 0, 0
    -- Leave room for the "+ new" button and the running indicator on the right.
    local budget = math.max(8, width - 26)

    local function push_tab(s)
        local label = tab_label(s.name or s.id)
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
        table.insert(children, {
            type = "text",
            id = id,
            spans = { {
                text = " " .. label .. " ",
                fg = active and C.ink or C.dim,
                bg = active and C.accent or nil,
                bold = active,
            } },
            size = { fixed = cells(label) + 2 },
            wrap = false,
        })
        -- Separator between tabs only: a trailing one reads as a stray glyph.
        table.insert(children, {
            type = "text",
            spans = { { text = " ", fg = C.dim } },
            size = { fixed = 1 },
            wrap = false,
        })
        used = used + cells(label) + 3   -- label + padding + separator
        shown = shown + 1
    end

    for _, s in ipairs(ordered) do
        if shown < TUI.TAB_MAX and used + cells(tab_label(s.name or s.id)) + 2 <= budget then
            push_tab(s)
        else
            hidden = hidden + 1
        end
    end

    -- Everything after the tabs rides one flexible node, so `+ new` stays pinned right.
    local state_spans = {}
    if shown == 0 then
        table.insert(state_spans, { text = " no session ", fg = C.dim })
    elseif hidden > 0 then
        table.insert(state_spans, { text = " +" .. hidden .. " ", fg = C.dim })
    end

    -- The alert belongs on the busiest row.
    if TUI.alert then
        table.insert(state_spans, { text = "  " .. TUI.alert.text, fg = TUI.alert.fg or C.danger, bold = true })
    end

    -- Running state is a property of the *session*, and only the active one can be live
    -- (one SSE stream), so the indicator goes on the bar rather than on a tab. Not
    -- animated on purpose: the transcript's streaming line already spins, and a second
    -- spinner for the same fact is noise.
    if session.streaming then
        table.insert(state_spans, { text = "  ● streaming", fg = C.ok })
    elseif session.aborting then
        table.insert(state_spans, { text = "  ● aborting", fg = C.danger, bold = true })
    end
    table.insert(children, { type = "text", spans = state_spans, size = { flex = 1 }, wrap = false })

    table.insert(children, {
        type = "text",
        id = "tab_new",
        spans = { { text = " + new ", fg = C.accent } },
        size = { fixed = 7 },
        align = "right",
        wrap = false,
    })

    return {
        type = "split",
        direction = "horizontal",
        size = { fixed = 1 },
        children = children,
    }
end

-- Row 2: cwd ▸ title, with the phase on the right. Each part is its own span so a long
-- path can be dimmed while the session title stays legible.
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
