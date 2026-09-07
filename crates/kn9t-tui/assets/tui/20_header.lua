-- 20_header.lua — breadcrumb (active main view) + transient alert slot.

local C = TUI.color

function TUI.build_header(width)
    local label = TUI.MAIN_VIEW == "chat" and "Chat" or TUI.MAIN_VIEW

    local left = {
        { text = " kn9t ", fg = C.accent, bold = true },
        { text = "> ", fg = C.dim },
        { text = label, fg = C.value },
    }

    local right = {}
    if TUI.alert then
        table.insert(right, { text = TUI.alert.text, fg = TUI.alert.fg or C.danger, bold = true })
        table.insert(right, { text = "  ", fg = C.dim })
    end

    local right_w = 0
    for _, s in ipairs(right) do
        right_w = right_w + #s.text
    end

    return {
        type = "box",
        border = "plain",
        border_fg = C.dim,
        size = { fixed = 3 },
        child = {
            type = "split",
            direction = "horizontal",
            children = {
                { type = "text", spans = left, size = { flex = 1 } },
                { type = "text", spans = right, align = "right", size = { fixed = math.max(right_w, 1) } },
            },
        },
    }
end
