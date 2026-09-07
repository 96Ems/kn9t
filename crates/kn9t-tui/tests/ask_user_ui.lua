
local function bar(done, total, width)
    if total <= 0 then return "" end
    local filled = math.floor((done / total) * width + 0.5)
    return string.rep("#", filled) .. string.rep(".", width - filled)
end

function render(s)
    s = s or {}

    -- Idle: one dim line, so the panel does not draw attention when unused.
    if not s.pending then
        local n = s.answered or 0
        return {
            type = "text",
            fg = "#585b70",
            content = n > 0 and (n .. " answered") or "idle",
        }
    end

    local rows = {}

    -- The question itself, wrapped by the host renderer.
    table.insert(rows, {
        type = "text",
        wrap = true,
        fg = "#cdd6f4",
        content = s.question or "(waiting)",
        size = { flex = 1 },
    })

    -- Sequences show progress; a single question does not need it.
    if s.total and s.total > 1 then
        local done = (s.index or 1) - 1
        table.insert(rows, {
            type = "text",
            fg = "#89b4fa",
            size = { fixed = 1 },
            content = string.format("%d/%d %s", s.index or 1, s.total,
                                    bar(done, s.total, 10)),
        })
    end

    -- Options as a list so the shape matches what the user is choosing from.
    if s.options and #s.options > 0 then
        local items = {}
        for _, o in ipairs(s.options) do table.insert(items, o) end
        table.insert(rows, {
            type = "list",
            items = items,
            size = { fixed = math.min(#items, 4) },
        })
    end

    table.insert(rows, {
        type = "text",
        fg = "#f9e2af",
        size = { fixed = 1 },
        content = s.kind and ("[" .. s.kind .. "]") or "",
    })

    return { type = "split", direction = "vertical", children = rows }
end
