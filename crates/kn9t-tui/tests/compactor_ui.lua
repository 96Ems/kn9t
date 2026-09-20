
function render(state)
  local status = state.status or "unknown"
  local session_id = state.session_id or ""
  local messages_count = state.messages_count or 0
  local tool_calls_count = state.tool_calls_count or 0
  local decisions = state.decisions or {}
  local summary_preview = state.summary_preview or ""
  local triage_done = state.triage_done or false
  local short_id = session_id:sub(1, 8)
  
  -- Status styling
  local status_color = "gray"
  local status_icon = "○"
  local status_text = status
  if status == "reading" then
    status_color = "blue"
    status_icon = "◔"
    status_text = "reading"
  elseif status == "triage" then
    status_color = "yellow"
    status_icon = "↻"
    status_text = "triage"
  elseif status == "triage_retry" then
    status_color = "yellow"
    status_icon = "↻"
    status_text = "triage (retry)"
  elseif status == "summary" then
    status_color = "cyan"
    status_icon = "↻"
    status_text = "summarizing"
  elseif status == "complete" then
    status_color = "green"
    status_icon = "✓"
    status_text = "done"
  elseif status == "error" then
    status_color = "red"
    status_icon = "✕"
    status_text = "failed"
  end
  
  local items = {}
  
  -- Header with status
  local header = status_icon .. " " .. status_text
  if short_id ~= "" then
    header = header .. "  " .. short_id
  end
  table.insert(items, { text = header, fg = status_color })
  
  -- Info line
  if messages_count > 0 then
    local info = tostring(messages_count) .. " msgs"
    if tool_calls_count > 0 then
      info = info .. ", " .. tostring(tool_calls_count) .. " tools"
    end
    table.insert(items, { text = info, fg = "gray" })
  end
  
  -- Tool-call decisions. Kept results are copied verbatim into the summary, so
  -- "keep" is the one the user cares about: show each call by name and command.
  if #decisions > 0 or triage_done then
    table.insert(items, { text = "─── Tool calls (keep = verbatim) ───", fg = "gray" })

    local keep_count = 0
    local summarize_count = 0
    local drop_count = 0
    for _, d in ipairs(decisions) do
      if d.action == "keep" then keep_count = keep_count + 1
      elseif d.action == "summarize" then summarize_count = summarize_count + 1
      elseif d.action == "drop" then drop_count = drop_count + 1
      end
    end

    if #decisions > 0 then
      table.insert(items, {
        text = string.format("keep %d   summarize %d   drop %d",
          keep_count, summarize_count, drop_count),
        fg = "white",
      })

      for _, d in ipairs(decisions) do
        local icon, color, verb = "○", "gray", "?"
        if d.action == "keep" then
          icon, color, verb = "✓", "green", "keep"
        elseif d.action == "summarize" then
          icon, color, verb = "≈", "yellow", "sum"
        elseif d.action == "drop" then
          icon, color, verb = "✕", "red", "drop"
        end
        local name = d.name or d.id:sub(1, 12)
        local line = string.format("%s %-4s %s", icon, verb, name)
        if d.preview and d.preview ~= "" then
          line = line .. "  " .. d.preview
        end
        table.insert(items, { text = line, fg = color })
      end
    end
  end
  
  -- Summary section (shown during/after summary phase)
  if status == "summary" or status == "complete" or summary_preview ~= "" then
    table.insert(items, { text = "─── Summary ───", fg = "gray" })
    if summary_preview ~= "" then
      -- Show full summary, wrapped
      for line in summary_preview:gmatch("[^\\n]+") do
        table.insert(items, { text = line, fg = "cyan" })
      end
    elseif status == "summary" then
      table.insert(items, { text = "generating...", fg = "cyan" })
    end
  end
  
  -- Error message
  if state.error then
    table.insert(items, { text = "⚠ " .. state.error, fg = "red" })
  end
  
  -- The layout owns the frame: it draws the border, the title and the focus
  -- ring around every plugin view. A view returns only its content — wrapping it
  -- in a box here nested two borders and printed two titles.
  return { type = "list", items = items }
end
