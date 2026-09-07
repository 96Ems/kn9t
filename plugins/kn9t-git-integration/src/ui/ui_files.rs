//! Sidebar view: file list for navigation.
//!
//! Shows branch, changed files, recent commits. Click a file to select it
//! (the diff view shows the selected file's content).

pub const LUA_FILES: &str = r##"
-- View state
V = {
  file = 1,  -- 1-based index, synced with diff view via shared state
}

local LAST = nil

local function files()
  if LAST == nil or LAST.diff == nil then return {} end
  return LAST.diff
end

-- Click a file to select it
kn9t.on_click("file_list", function(x, y)
  local f = files()
  local idx = y + 1
  if idx >= 1 and idx <= #f then
    V.file = idx
  end
end)

-- j/k navigation
kn9t.on_key("j", function()
  local f = files()
  if V.file < #f then V.file = V.file + 1 end
end)

kn9t.on_key("k", function()
  if V.file > 1 then V.file = V.file - 1 end
end)

function render(state)
  LAST = state
  if state == nil or state.repo == nil then
    return { type = "text", content = "(not a git repo)", fg = "darkgray" }
  end

  local repo = state.repo
  local branch = repo.branch or "?"
  local ab = ""
  if (repo.ahead or 0) > 0 then ab = ab .. " +" .. repo.ahead end
  if (repo.behind or 0) > 0 then ab = ab .. " -" .. repo.behind end

  local children = {
    { type = "text", content = branch .. ab, fg = "cyan", bold = true,
      size = { fixed = 1 }, wrap = false },
  }

  -- File list
  local f = files()
  if #f == 0 then
    table.insert(children, { type = "text", content = "(clean)", fg = "darkgray",
                             size = { fixed = 1 } })
  else
    local items = {}
    for _, file in ipairs(f) do
      local col = "yellow"
      if file.status == "A" then col = "green"
      elseif file.status == "D" then col = "lightred" end
      table.insert(items, { spans = {
        { text = file.status .. " ", fg = col },
        { text = file.path },
      }})
    end
    table.insert(children, {
      type = "list",
      id = "file_list",
      items = items,
      selected = V.file - 1,
    })
  end

  -- Recent commits
  table.insert(children, { type = "text", content = "", size = { fixed = 1 } })
  table.insert(children, { type = "text", content = "recent:", fg = "darkgray",
                           size = { fixed = 1 }, wrap = false })
  for _, l in ipairs(repo.recent or {}) do
    table.insert(children, { type = "text",
      content = string.sub(l.sha, 1, 7) .. " " .. l.subject,
      fg = "gray", size = { fixed = 1 }, wrap = false })
  end

  return { type = "split", direction = "vertical", children = children }
end
"##;
