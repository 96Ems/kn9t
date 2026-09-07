-- 50_status.lua — render_status(): ReAct-style phase indicator + cost.

local C = TUI.color

function render_status()
    local ctx     = kn9t.context or {}
    local session = kn9t.state and kn9t.state.session or {}
    local usage   = kn9t.state and kn9t.state.usage or {}

    local seg = {}
    local function put(text, color) table.insert(seg, { text = text, color = color }) end

    -- Message mix bar
    local s = ctx.system_count or 0
    local u = ctx.user_count or 0
    local a = ctx.assistant_count or 0
    local t = ctx.tool_count or 0
    local total = s + u + a + t
    local bar = 14

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

    -- Context pressure
    local turn = usage.turn or {}
    local live = (turn.input or 0) + (turn.cache_read or 0)
    if live == 0 then live = (ctx.tokens_in or 0) + (ctx.cache_read or 0) end
    local frac = live / TUI.context_window()
    put(" ctx ", C.label)
    put(string.format("%d%%", math.floor(frac * 100)), TUI.context_color(frac))

    put("  ", nil)
    put(TUI.fmt_tokens(live), C.value)
    put("/", C.dim)
    put(TUI.fmt_tokens(TUI.context_window()), C.dim)

    -- Cost
    put("  $", C.dim)
    put(string.format("%.4f", usage.cost or 0), C.warn)

    -- Live state
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

    -- Contextual help bar (right-aligned)
    put("  ", nil)
    local focused = (kn9t.state and kn9t.state.focused_plugin) or ""
    if focused ~= "" then
        -- Plugin focused: show plugin controls
        put("j/k", C.accent)
        put(":nav ", C.dim)
        put("d", C.accent)
        put(":diff ", C.dim)
        put("Esc", C.accent)
        put(":release", C.dim)
    else
        -- Normal mode: show global controls
        put("C-p", C.accent)
        put(":cmd ", C.dim)
        put("F1/F2", C.accent)
        put(":side ", C.dim)
        put("F10", C.accent)
        put(":git", C.dim)
    end

    return seg
end
