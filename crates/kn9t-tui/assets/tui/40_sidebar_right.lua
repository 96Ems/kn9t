-- 40_sidebar_right.lua — session context/usage dashboard.
--
-- This panel used to repeat the status bar one number at a time: the cost, the token rate and
-- the four message counts were on both lines, so 34 columns bought a second copy of a row you
-- can already read. Those are gone. The context percentage stays on both on purpose: the
-- status chip is a glance while you type, this is the panel you open to *decide*, and a gauge
-- without its number beside it is not readable.

local C = TUI.color

-- Context pressure: a number and a bar. The `%` is deliberately repeated from the status
-- bar's chip — the chip is a glance while you type, this is the panel you open to *decide*,
-- and the gauge is useless without its number on the same screen.
local function section_context(inner_w)
    local live, frac = TUI.live_context()
    local left = math.max(0, TUI.context_window() - live)
    local col  = TUI.context_color(frac)

    return {
        type = "split",
        direction = "vertical",
        size = { fixed = 4 },
        children = {
            { type = "text", size = { fixed = 1 },
              content = TUI.kv_lines({ { "context", string.format("%d%%", math.floor(frac * 100)) } }, inner_w),
              fg = col },
            { type = "gauge", size = { fixed = 1 }, frac = frac, fg = col },
            { type = "text", size = { fixed = 1 },
              content = TUI.kv_lines({ { "used", TUI.fmt_tokens(live) } }, inner_w),
              fg = C.label },
            { type = "text", size = { fixed = 1 },
              content = TUI.kv_lines({ { "left", TUI.fmt_tokens(left) } }, inner_w),
              fg = C.label },
        },
    }
end

-- The token breakdown the status bar has no room for. `speed` is not here: it is on the
-- status bar, where it is the number you watch while a turn streams.
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
        { "turn in",  TUI.fmt_tokens(turn.input) },
        { "turn out", TUI.fmt_tokens(turn.output) },
        { "total in", TUI.fmt_tokens(input) },
        { "cache",    hit .. "%" },
    }

    return {
        type = "box",
        title = " usage ",
        border = true,
        size = { fixed = #rows + 2 },
        child = { type = "text", content = TUI.kv_lines(rows, inner_w - 2), fg = C.value },
    }
end

function TUI.build_sidebar_right(width, height)
    local ctx     = kn9t.context or {}
    local session = kn9t.state and kn9t.state.session or {}
    local inner_w = width - 2

    local title = session.title
    if title == nil or title == "" then title = "untitled" end
    if #title > inner_w then title = title:sub(1, inner_w - 1) .. "~" end

    local sid = session.id or ""
    if #sid > 8 then sid = sid:sub(1, 8) end

    local status_text
    if session.streaming then
        status_text = "streaming"
    elseif session.aborting then
        status_text = "aborting"
    else
        status_text = ctx.phase or "idle"
    end

    local head = {
        { type = "text", content = title, fg = C.accent, size = { fixed = 1 } },
        {
            type = "text",
            size = { fixed = 1 },
            content = TUI.kv_lines({
                { sid ~= "" and ("#" .. sid) or "no session", status_text },
            }, inner_w),
            fg = C.dim,
        },
        section_context(inner_w),
        section_usage(inner_w),
    }

    local used = 0
    for _, s in ipairs(head) do used = used + (s.size and s.size.fixed or 0) end

    -- Whatever is left over belongs to the plugins (D7). The transcript counters and the
    -- cost that used to fill this space repeated the status bar; a spacer is honest about
    -- there being nothing else to say here.
    local slots = math.max(1, height - 2 - used)

    local children = {}
    for _, s in ipairs(head) do table.insert(children, s) end
    table.insert(children, { type = "spacer", size = { fixed = slots } })

    return {
        type = "box",
        -- The model was here as the box title *and* in the breadcrumb *and* in the status
        -- bar *and* on the prompt frame. The panel is the session, so it says so.
        title = " [F2] session ",
        -- Rounded to match the explorer column and the prompt frame: the three side by side
        -- were drawing two different corner glyphs.
        border = "rounded",
        child = {
            type = "split",
            direction = "vertical",
            children = children,
        },
    }
end
