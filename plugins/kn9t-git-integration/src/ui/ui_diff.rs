//! Main view: diff content with syntax highlighting and comments.

pub const LUA_DIFF: &str = r##"
-- View state
V = {
  file = 1,
  cursor = 1,
  scroll = 0,
  split = false,
  comments = {},
  typing = nil,
  height = 30,
}

local LAST = nil

local function files()
  if LAST == nil or LAST.diff == nil then return {} end
  return LAST.diff
end

local function cur_file()
  local f = files()
  if #f == 0 then return nil end
  if V.file > #f then V.file = #f end
  if V.file < 1 then V.file = 1 end
  return f[V.file]
end

local function rows(file)
  local out = {}
  if file == nil then return out end
  for _, h in ipairs(file.hunks or {}) do
    table.insert(out, { hunk = true, text = h.header })
    for _, l in ipairs(h.lines or {}) do
      table.insert(out, {
        kind = l.kind,
        text = l.text,
        new_lineno = l.new_lineno,
        old_lineno = l.old_lineno,
      })
    end
  end
  return out
end

local function clamp_cursor(n)
  if V.cursor < 1 then V.cursor = 1 end
  if n > 0 and V.cursor > n then V.cursor = n end
  local view = math.max(1, V.height - 2)
  if V.cursor <= V.scroll then V.scroll = V.cursor - 1 end
  if V.cursor > V.scroll + view then V.scroll = V.cursor - view end
  if V.scroll < 0 then V.scroll = 0 end
end

local function comment_at(path, line)
  for _, c in ipairs(V.comments) do
    if c.path == path and c.line == line then return c.text end
  end
  return nil
end

-- Language detection for syntax highlighting
local function lang_from_path(path)
  if path == nil then return nil end
  local ext = path:match("%.([^%.]+)$")
  if ext == nil then return nil end
  ext = ext:lower()
  local map = {
    rs = "rust", py = "python", js = "javascript", ts = "typescript",
    tsx = "tsx", jsx = "jsx", lua = "lua", rb = "ruby", go = "go",
    c = "c", h = "c", cpp = "cpp", hpp = "cpp", cc = "cpp",
    java = "java", kt = "kotlin", swift = "swift", cs = "csharp",
    sh = "bash", bash = "bash", zsh = "bash",
    json = "json", yaml = "yaml", yml = "yaml", toml = "toml",
    xml = "xml", html = "html", css = "css", md = "markdown",
  }
  return map[ext]
end

local function line_style(kind)
  if kind == "add" then return "green" end
  if kind == "del" then return "lightred" end
  return "gray"
end

local function line_bg(kind)
  if kind == "add" then return "#1a2e1a" end
  if kind == "del" then return "#2e1a1a" end
  return nil
end

-- ══════════════════════════════════════════════════════════════════════════════
-- Key bindings
-- ══════════════════════════════════════════════════════════════════════════════

local BOUND = {}

local function bind(key, fn)
  BOUND[key] = true
  kn9t.on_key(key, function()
    if V.typing ~= nil and #key == 1 then
      V.typing = V.typing .. key
      return true
    end
    return fn()
  end)
end

bind("j", function() V.cursor = V.cursor + 1; clamp_cursor(#rows(cur_file())) end)
bind("k", function() V.cursor = V.cursor - 1; clamp_cursor(#rows(cur_file())) end)
bind("Down", function() V.cursor = V.cursor + 1; clamp_cursor(#rows(cur_file())) end)
bind("Up", function() V.cursor = V.cursor - 1; clamp_cursor(#rows(cur_file())) end)

bind("PageDown", function()
  V.cursor = V.cursor + math.max(1, V.height - 2)
  clamp_cursor(#rows(cur_file()))
end)
bind("PageUp", function()
  V.cursor = V.cursor - math.max(1, V.height - 2)
  clamp_cursor(#rows(cur_file()))
end)

bind("n", function()
  local f = files()
  if V.file < #f then V.file = V.file + 1; V.cursor = 1; V.scroll = 0 end
end)
bind("p", function()
  if V.file > 1 then V.file = V.file - 1; V.cursor = 1; V.scroll = 0 end
end)

bind("]", function()
  local r = rows(cur_file())
  for i = V.cursor + 1, #r do
    if r[i].hunk then V.cursor = i; clamp_cursor(#r); return end
  end
  local f = files()
  if V.file < #f then V.file = V.file + 1; V.cursor = 1; V.scroll = 0 end
end)
bind("[", function()
  local r = rows(cur_file())
  for i = V.cursor - 1, 1, -1 do
    if r[i].hunk then V.cursor = i; clamp_cursor(#r); return end
  end
  if V.file > 1 then
    V.file = V.file - 1
    local pr = rows(cur_file())
    V.cursor = #pr > 0 and #pr or 1
    clamp_cursor(#pr)
  end
end)

bind("u", function() V.split = not V.split end)

-- Comment capture
bind("c", function()
  V.typing = ""
end)

bind("Enter", function()
  if V.typing == nil then return false end
  local f = cur_file()
  local r = rows(f)
  local row = r[V.cursor]
  if f ~= nil and row ~= nil and V.typing ~= "" then
    local line = row.new_lineno or row.old_lineno
    if line ~= nil then
      table.insert(V.comments, { path = f.path, line = line, text = V.typing })
    end
  end
  V.typing = nil
end)

bind("Backspace", function()
  if V.typing == nil then return false end
  V.typing = string.sub(V.typing, 1, -2)
end)

bind("Esc", function()
  if V.typing ~= nil then
    V.typing = nil
    return true
  end
  return false
end)

-- Send comments to input
bind("C-s", function()
  if #V.comments == 0 then return false end
  local parts = {}
  for _, c in ipairs(V.comments) do
    table.insert(parts, "[" .. c.path .. ":" .. c.line .. "] " .. c.text)
  end
  kn9t.insert_input(table.concat(parts, "\n"))
  V.comments = {}
end)

-- Printable keys for comment typing
local PRINTABLE = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.,:;!?/()<>-_=+*#@'\"`~$%^&|\\"
for i = 1, #PRINTABLE do
  local ch = string.sub(PRINTABLE, i, i)
  if not BOUND[ch] then
    bind(ch, function() return false end)
  end
end

kn9t.on_key("Space", function()
  if V.typing ~= nil then
    V.typing = V.typing .. " "
    return true
  end
  return false
end)

-- Click on diff body
kn9t.on_click("body", function(x, y)
  local target = V.scroll + y + 1
  local n = #rows(cur_file())
  if target >= 1 and target <= n then
    if target == V.cursor and V.typing == nil then
      V.typing = ""
    else
      V.cursor = target
      clamp_cursor(n)
    end
  end
end)

-- ══════════════════════════════════════════════════════════════════════════════
-- Render functions
-- ══════════════════════════════════════════════════════════════════════════════

local function unified_body(file)
  local r = rows(file)
  local items = {}
  local lang = lang_from_path(file.path)
  for i, row in ipairs(r) do
    if row.hunk then
      table.insert(items, { spans = { { text = row.text, fg = "cyan", bold = true } } })
    else
      local mark = (i == V.cursor) and ">" or " "
      local num = row.new_lineno or row.old_lineno
      local prefix = (row.kind == "add" and "+") or (row.kind == "del" and "-") or " "
      local bg = line_bg(row.kind)
      local code_span = { text = row.text, syntax = lang, bg = bg }
      table.insert(items, { spans = {
        { text = mark, fg = "yellow", bold = true, bg = bg },
        { text = string.format("%5s ", num and tostring(num) or ""), fg = "darkgray", bg = bg },
        { text = prefix, fg = line_style(row.kind), bg = bg },
        code_span,
      }})
      local existing = comment_at(file.path, num)
      if existing ~= nil then
        table.insert(items, { spans = { { text = "      > " .. existing, fg = "magenta" } } })
      end
    end
  end
  return { type = "list", id = "body", items = items, offset = V.scroll }
end

local function split_body(file)
  local r = rows(file)
  local left, right = {}, {}
  local lang = lang_from_path(file.path)
  local del_bg = line_bg("del")
  local add_bg = line_bg("add")
  for i, row in ipairs(r) do
    if row.hunk then
      table.insert(left, { spans = { { text = row.text, fg = "cyan", bold = true } } })
      table.insert(right, { spans = { { text = "", fg = "cyan" } } })
    else
      local mark = (i == V.cursor) and ">" or " "
      if row.kind == "del" then
        table.insert(left, { spans = {
          { text = mark .. "-", fg = "lightred", bg = del_bg },
          { text = row.text, syntax = lang, bg = del_bg },
        }})
        table.insert(right, { spans = { { text = "" } } })
      elseif row.kind == "add" then
        table.insert(left, { spans = { { text = "" } } })
        table.insert(right, { spans = {
          { text = mark .. "+", fg = "green", bg = add_bg },
          { text = row.text, syntax = lang, bg = add_bg },
        }})
      else
        table.insert(left, { spans = {
          { text = mark .. " ", fg = "gray" },
          { text = row.text, syntax = lang },
        }})
        table.insert(right, { spans = {
          { text = mark .. " ", fg = "gray" },
          { text = row.text, syntax = lang },
        }})
      end
    end
  end
  return {
    type = "split",
    direction = "horizontal",
    children = {
      { type = "list", id = "body", items = left, offset = V.scroll },
      { type = "list", items = right, offset = V.scroll },
    },
  }
end

function render(state)
  LAST = state
  if state == nil then
    return { type = "text", content = "(no state)", fg = "darkgray" }
  end

  local f = cur_file()
  if f == nil then
    return { type = "text", content = "(no changes to review)", fg = "darkgray" }
  end

  local comment_info = ""
  if #V.comments > 0 then
    comment_info = string.format("  [%d pending]", #V.comments)
  end
  local head = string.format("%s  %d/%d  %s%s",
    f.path, V.file, #files(),
    V.split and "split" or "unified",
    comment_info)

  local body = V.split and split_body(f) or unified_body(f)

  local children = {
    { type = "text", content = head, fg = "cyan", bold = true,
      size = { fixed = 1 }, wrap = false },
    body,
  }

  if V.typing ~= nil then
    table.insert(children, {
      type = "text", content = "comment> " .. V.typing .. "  (Enter: save, Esc: cancel)",
      fg = "magenta", size = { fixed = 1 }, wrap = false,
    })
  else
    local help = "j/k: move  n/p: file  c/click: comment  C-s: send  u: split"
    if #V.comments > 0 then
      help = string.format("C-s: send %d  |  %s", #V.comments, help)
    end
    table.insert(children, {
      type = "text", content = help, fg = "darkgray",
      size = { fixed = 1 }, wrap = false,
    })
  end

  return { type = "split", direction = "vertical", children = children }
end
"##;
