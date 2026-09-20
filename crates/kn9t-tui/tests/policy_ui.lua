
-- Policy plugin UI
local V = { cursor = 0, adding = false, input = "" }

function on_state(s)
    V.mode = s.mode or "normal"
    V.grants = s.grants or {}
    V.recent = s.recent or {}
    -- Don't override cursor/adding/input from state - those are local UI state
end

-- Exact keys first: the host matches on_key before on_text, so a shortcut aimed
-- at a printable character must decline while a grant is being typed. That
-- leaves the single on_text handler below as the only writer of the input
-- buffer. Actions that persist (mode, grants) go to Python via kn9t.notify;
-- navigation stays local in V.

kn9t.on_key("m", function()
    if V.adding then return false end
    kn9t.notify({ event = "cycle_mode" })
    return true
end)

kn9t.on_key("a", function()
    if V.adding then return false end
    V.adding = true
    V.input = ""
    return true
end)

-- `d` and `x` are the same action.
local function delete_grant()
    if V.cursor >= 0 and V.cursor < #V.grants then
        kn9t.notify({ event = "delete_grant", index = V.cursor })
    end
    return true
end

kn9t.on_key("d", function() if V.adding then return false end return delete_grant() end)
kn9t.on_key("x", function() if V.adding then return false end return delete_grant() end)

local function move(delta)
    local n = #V.grants
    if n == 0 then return end
    V.cursor = math.max(0, math.min(n - 1, V.cursor + delta))
end

kn9t.on_key("j", function() if V.adding then return false end move(1) return true end)
kn9t.on_key("k", function() if V.adding then return false end move(-1) return true end)
kn9t.on_key("Down", function() if V.adding then return false end move(1) return true end)
kn9t.on_key("Up", function() if V.adding then return false end move(-1) return true end)

kn9t.on_key("Escape", function()
    -- Clear a half-typed grant, then let Esc release focus.
    if V.adding then
        V.adding = false
        V.input = ""
    end
    return false
end)

kn9t.on_key("Enter", function()
    if not V.adding or V.input == "" then return false end
    kn9t.notify({ event = "add_grant", pattern = V.input })
    V.adding = false
    V.input = ""
    return true
end)

kn9t.on_key("Backspace", function()
    if not V.adding then return false end
    V.input = string.sub(V.input, 1, -2)
    return true
end)

-- One handler for every printable character, Space included. It owns the
-- keystroke only while a grant is being typed; otherwise the character falls
-- through to the shortcuts above or to the host.
kn9t.on_text(function(ch)
    if not V.adding then return false end
    V.input = V.input .. ch
    return true
end)

function render(s)
    on_state(s)
    local out = {}
    local C = {
        accent = "cyan",
        dim = "darkgray",
        normal = "green",
        yolo = "yellow",
        ask_all = "magenta",
        allow = "green",
        deny = "lightred",
        ask = "yellow",
    }
    
    -- Mode selector
    local mode_spans = {}
    table.insert(mode_spans, { text = "Mode: ", fg = C.dim })
    for _, m in ipairs({"normal", "yolo", "ask_all"}) do
        if V.mode == m then
            table.insert(mode_spans, { text = "[", fg = C[m] })
            table.insert(mode_spans, { text = m, fg = C[m], bold = true })
            table.insert(mode_spans, { text = "] ", fg = C[m] })
        else
            table.insert(mode_spans, { text = m .. " ", fg = C.dim })
        end
    end
    table.insert(out, { type = "text", spans = mode_spans, size = { fixed = 1 } })
    
    -- Separator
    table.insert(out, { type = "text", content = string.rep("─", 40), fg = C.dim, size = { fixed = 1 } })
    
    -- Recent decisions
    table.insert(out, { type = "text", content = "Recent:", fg = C.dim, size = { fixed = 1 } })
    if #V.recent == 0 then
        table.insert(out, { type = "text", content = "  (none)", fg = C.dim, size = { fixed = 1 } })
    else
        for i = #V.recent, 1, -1 do
            local d = V.recent[i]
            local icon = "✓"
            local col = C.allow
            if d.result == "deny" then icon = "✗"; col = C.deny
            elseif d.result == "ask" then icon = "?"; col = C.ask end
            local spans = {
                { text = icon .. " ", fg = col },
                { text = d.cmd, fg = "white" },
            }
            if d.reason ~= "" then
                table.insert(spans, { text = " (" .. d.reason .. ")", fg = C.dim })
            end
            table.insert(out, { type = "text", spans = spans, size = { fixed = 1 } })
        end
    end
    
    -- Separator
    table.insert(out, { type = "text", content = string.rep("─", 40), fg = C.dim, size = { fixed = 1 } })
    
    -- Grants list
    table.insert(out, { type = "text", content = "Grants (always allow):", fg = C.dim, size = { fixed = 1 } })
    if #V.grants == 0 and not V.adding then
        table.insert(out, { type = "text", content = "  (none) - press 'a' to add", fg = C.dim, size = { fixed = 1 } })
    else
        for i, g in ipairs(V.grants) do
            local prefix = (i - 1 == V.cursor) and "> " or "  "
            local fg = (i - 1 == V.cursor) and C.accent or "white"
            table.insert(out, { type = "text", content = prefix .. g, fg = fg, size = { fixed = 1 } })
        end
    end
    
    -- Input line for adding
    if V.adding then
        local spans = {
            { text = "  + ", fg = C.accent },
            { text = V.input, fg = "white" },
            { text = "█", fg = C.accent },
        }
        table.insert(out, { type = "text", spans = spans, size = { fixed = 1 } })
    end
    
    -- Spacer
    table.insert(out, { type = "spacer", size = { flex = 1 } })
    
    -- Help bar
    local help_spans = {}
    if V.adding then
        table.insert(help_spans, { text = "[Enter]", fg = C.accent })
        table.insert(help_spans, { text = " save  ", fg = C.dim })
        table.insert(help_spans, { text = "[Bksp]", fg = C.accent })
        table.insert(help_spans, { text = " clear", fg = C.dim })
    else
        table.insert(help_spans, { text = "[m]", fg = C.accent })
        table.insert(help_spans, { text = " mode  ", fg = C.dim })
        table.insert(help_spans, { text = "[a]", fg = C.accent })
        table.insert(help_spans, { text = " add  ", fg = C.dim })
        table.insert(help_spans, { text = "[d]", fg = C.accent })
        table.insert(help_spans, { text = " del  ", fg = C.dim })
        table.insert(help_spans, { text = "[↑↓]", fg = C.accent })
        table.insert(help_spans, { text = " nav", fg = C.dim })
    end
    table.insert(out, { type = "text", spans = help_spans, size = { fixed = 1 } })
    
    return { type = "split", direction = "vertical", children = out }
end
