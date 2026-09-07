-- 40_sidebar_right.lua — session context/usage dashboard.

local C = TUI.color

local function section_context(inner_w)
    local live, frac = TUI.live_context()
    local left = math.max(0, TUI.context_window() - live)
    local col  = TUI.context_color(frac)

    return {
        type = "split",
        direction = "vertical",
        size = { fixed = 4 },
        children = {
            {
                type = "text",
                size = { fixed = 1 },
                content = TUI.kv_lines({
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
                content = TUI.kv_lines({ { "used", TUI.fmt_tokens(live) } }, inner_w),
                fg = C.label,
            },
            {
                type = "text",
                size = { fixed = 1 },
                content = TUI.kv_lines({ { "left", TUI.fmt_tokens(left) } }, inner_w),
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
        { "turn in",  TUI.fmt_tokens(turn.input) },
        { "turn out", TUI.fmt_tokens(turn.output) },
        { "total in", TUI.fmt_tokens(input) },
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
        child = { type = "text", content = TUI.kv_lines(rows, inner_w - 2), fg = C.value },
    }
end

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
        child = { type = "text", content = TUI.kv_lines(rows, inner_w - 2), fg = C.value },
    }
end

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
        section_transcript(inner_w),
    }
    local foot = {
        {
            type = "text",
            size = { fixed = 1 },
            content = TUI.kv_lines({
                { "cost", string.format("$%.4f", (kn9t.state and kn9t.state.usage
                    and kn9t.state.usage.cost) or 0) },
            }, inner_w),
            fg = C.warn,
        },
    }

    local used = 0
    for _, s in ipairs(head) do used = used + (s.size and s.size.fixed or 0) end
    for _, s in ipairs(foot) do used = used + (s.size and s.size.fixed or 0) end

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
