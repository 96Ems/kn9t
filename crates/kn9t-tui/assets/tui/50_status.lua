-- 50_status.lua — render_status(): the segmented status bar (PLAN §P7 D13).
--
-- Solid blocks of state read left to right, with the contextual key hints on the right —
-- VS Code's grammar. Three colours carry meaning (accent for "where we are", amber for
-- "watch this", danger for "this is wrong") and the rest is chrome, so the eye lands on
-- what changed.
--
-- A segment takes the same fields as a text span: `{text=, fg=, bg=, bold=, dim=,
-- reverse=}`. `bg` is what makes a block rather than coloured text.

local C = TUI.color

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
    local frac = live / TUI.context_window()
    local pct  = math.floor(frac * 100)
    text("  ctx ")
    block(" " .. pct .. "% ", C.ink, TUI.context_color(frac), { bold = true })
    text(" " .. TUI.fmt_tokens(live) .. "/" .. TUI.fmt_tokens(TUI.context_window()))

    -- ── Cost ────────────────────────────────────────────────────────────────
    text("   $")
    text(string.format("%.4f", usage.cost or 0), C.warn)

    -- ── Throughput, only while it means something ───────────────────────────
    if (usage.toks_per_sec or 0) > 0 then
        text(string.format("  %.0f t/s", usage.toks_per_sec))
    end

    -- ── Message mix, as four numbers ────────────────────────────────────────
    -- This used to be an 18-cell bar: the loudest thing on the line, answering a question
    -- nobody asks mid-turn, and wider than everything else combined.
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
