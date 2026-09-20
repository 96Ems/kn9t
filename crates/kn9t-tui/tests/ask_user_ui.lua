
-- Local view state; reset whenever the question changes.
local S = {}
local seen = nil
local cursor = 1
local toggles = {}
local text = ""
local yes = true

local function is_text() return (S.kind or "text") == "text" end

local function option_at(i)
    local o = (S.options or {})[i]
    if type(o) == "table" then return o end
    if o == nil then return nil end
    return { label = o, value = o }
end

local function option_value(i)
    local o = option_at(i)
    return o and (o.value or o.label) or nil
end

local function option_label(i)
    local o = option_at(i)
    return o and (o.label or o.value) or ""
end

local function submit(payload)
    kn9t.respond(payload)
    return true
end

local function submit_current()
    local kind = S.kind or "text"
    if kind == "choice" then
        local v = option_value(cursor)
        if v == nil then return false end
        return submit({ value = v })
    elseif kind == "multi" then
        local values = {}
        for i = 1, #(S.options or {}) do
            if toggles[i] then table.insert(values, option_value(i)) end
        end
        return submit({ value = values })
    elseif kind == "confirm" then
        return submit({ value = yes })
    end
    return submit({ value = text })
end

local function move(delta)
    if (S.kind or "") == "confirm" then
        -- Yes/No is a vertical list: Up selects Yes, Down selects No.
        yes = delta < 0
        return
    end
    local n = #(S.options or {})
    if n == 0 then return end
    cursor = ((cursor - 1 + delta) % n) + 1
end

-- Exact keys; a handler returns false to fall through to on_text or the host.
kn9t.on_key("Up", function() if is_text() then return false end move(-1) return true end)
kn9t.on_key("k", function() if is_text() then return false end move(-1) return true end)
kn9t.on_key("Down", function() if is_text() then return false end move(1) return true end)
kn9t.on_key("j", function() if is_text() then return false end move(1) return true end)
kn9t.on_key("Tab", function() if is_text() then return false end move(1) return true end)

kn9t.on_key("Space", function()
    if is_text() then text = text .. " " return true end
    if (S.kind or "") == "multi" then toggles[cursor] = not toggles[cursor] end
    return true
end)

kn9t.on_key("Backspace", function()
    if not is_text() then return false end
    text = string.sub(text, 1, -2)
    return true
end)

kn9t.on_key("Left", function()
    if (S.kind or "") ~= "confirm" then return false end
    yes = true
    return true
end)

kn9t.on_key("Right", function()
    if (S.kind or "") ~= "confirm" then return false end
    yes = false
    return true
end)

kn9t.on_key("y", function()
    if (S.kind or "") ~= "confirm" then return false end
    return submit({ value = true })
end)

kn9t.on_key("n", function()
    if (S.kind or "") ~= "confirm" then return false end
    return submit({ value = false })
end)

kn9t.on_key("Enter", function() return submit_current() end)

-- Quick pick: a digit selects and submits the matching option.
local function bind_digit(i)
    kn9t.on_key(tostring(i), function()
        if is_text() or i > #(S.options or {}) then return false end
        cursor = i
        return submit_current()
    end)
end
for i = 1, 9 do bind_digit(i) end

kn9t.on_text(function(ch)
    if not is_text() then return false end
    text = text .. ch
    return true
end)

function render(state)
    state = state or {}
    S = state

    -- Reset the local view when the question changes. The host re-renders every
    -- frame, so this must key off the question, not the call.
    local key = tostring(state.index or 0) .. "|" .. tostring(state.question or "")
    if key ~= seen then
        seen = key
        cursor = 1
        toggles = {}
        text = (type(state.default) == "string") and state.default or ""
        yes = state.default ~= false
    end

    local rows = {}
    local kind = state.kind or "text"

    if state.total and state.total > 1 then
        table.insert(rows, {
            type = "text",
            size = { fixed = 1 },
            fg = "#89b4fa",
            content = string.format("question %d/%d", state.index or 1, state.total),
        })
    end

    -- Reserved height, so the question is never squeezed out of the slot.
    table.insert(rows, {
        type = "text",
        wrap = true,
        size = { fixed = 2 },
        fg = "#cdd6f4",
        content = state.question or "(waiting)",
    })

    local n = #(state.options or {})
    local hint
    if kind == "choice" or kind == "multi" then
        local items = {}
        for i = 1, n do
            local label = option_label(i)
            if kind == "multi" then
                label = (toggles[i] and "[x] " or "[ ] ") .. label
            end
            items[i] = label
        end
        table.insert(rows, {
            type = "list",
            items = items,
            -- cursor is 1-based (indexes options); the list's selected is 0-based.
            selected = cursor - 1,
            size = { fixed = math.min(n, 8) },
        })
        hint = (kind == "multi")
            and "up/down move - Space toggle - Enter submit - Esc cancel"
            or "up/down move - Enter submit - Esc cancel"
    elseif kind == "confirm" then
        table.insert(rows, {
            type = "list",
            items = { "Yes", "No" },
            selected = yes and 0 or 1,
            size = { fixed = 2 },
        })
        hint = "up/down move - Enter submit - Esc cancel"
    else
        table.insert(rows, {
            type = "text",
            size = { fixed = 1 },
            fg = "#f9e2af",
            content = "> " .. text .. "_",
        })
        hint = "type your answer - Enter submit - Esc cancel"
    end

    table.insert(rows, {
        type = "text",
        size = { fixed = 1 },
        fg = "#585b70",
        content = hint,
    })

    return { type = "split", direction = "vertical", children = rows }
end
